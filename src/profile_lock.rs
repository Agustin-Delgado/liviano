//! Locking a profile: the options a profile keeps in `profiles.json`, when
//! a window locks, what the lock screen says and which keys it takes. Pure:
//! the window side is in `app/lock_ui`, Windows Hello and the password in
//! `hello`.
//!
//! The lock hides a profile's pages from a person who walks up to the
//! computer. It is not encryption: the files on disk do not change.
//!
//! Two settings, both off by default, so nothing changes for a person who
//! never turns them on:
//! - `lock`: the profile asks for Windows Hello when it opens.
//! - `lock_after`: when an open window locks again. "When the window
//!   closes" is the same as `lock`, said from the other side: the profile
//!   opens locked the next time.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// When an open window of the profile locks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockAfter {
    #[default]
    Never,
    #[serde(rename = "5_minutes")]
    FiveMinutes,
    #[serde(rename = "15_minutes")]
    FifteenMinutes,
    #[serde(rename = "1_hour")]
    OneHour,
    /// It opens locked the next time.
    WindowCloses,
}

/// The items of the "Lock after" list in Settings, in order.
pub const LOCK_AFTER_CHOICES: [(LockAfter, &str); 5] = [
    (LockAfter::Never, "Never"),
    (LockAfter::FiveMinutes, "5 minutes away"),
    (LockAfter::FifteenMinutes, "15 minutes away"),
    (LockAfter::OneHour, "1 hour away"),
    (LockAfter::WindowCloses, "When the window closes"),
];

impl LockAfter {
    /// How long the person is away before the window locks. None: it
    /// never locks for being away.
    pub fn away_minutes(self) -> Option<u32> {
        match self {
            LockAfter::FiveMinutes => Some(5),
            LockAfter::FifteenMinutes => Some(15),
            LockAfter::OneHour => Some(60),
            LockAfter::Never | LockAfter::WindowCloses => None,
        }
    }
}

/// A profile's lock options, as the registry keeps them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockOptions {
    /// "Ask for Windows Hello to open it".
    pub lock: bool,
    pub lock_after: LockAfter,
}

impl LockOptions {
    /// The profile can lock at all.
    pub fn locks(self) -> bool {
        self.lock || self.lock_after != LockAfter::Never
    }

    /// Its window opens locked.
    pub fn starts_locked(self) -> bool {
        self.lock || self.lock_after == LockAfter::WindowCloses
    }

    /// How long without input on the system locks an open window.
    pub fn away_limit(self) -> Option<Duration> {
        self.lock_after.away_minutes().map(|m| Duration::from_secs(u64::from(m) * 60))
    }

    /// The window locks when Windows locks the session: when the person
    /// asked for any lock while it is open.
    pub fn locks_with_windows(self) -> bool {
        self.lock_after != LockAfter::Never
    }
}

/// Why a window is locked. The lock screen says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The profile opens locked.
    Started,
    /// No input on the system for this many minutes.
    Away(u32),
    /// Windows locked the session.
    WindowsLocked,
    /// Someone asked for it now (a script).
    Asked,
}

impl Reason {
    /// "Locked after 15 minutes away.", "Locked when Windows locked."…
    pub fn text(self) -> String {
        match self {
            Reason::Started => "Locked when it started.".to_string(),
            Reason::Away(minutes) => format!("Locked after {} away.", duration_words(minutes)),
            Reason::WindowsLocked => "Locked when Windows locked.".to_string(),
            Reason::Asked => "Locked.".to_string(),
        }
    }
}

/// "5 minutes", "1 minute", "1 hour", "2 hours".
fn duration_words(minutes: u32) -> String {
    let (n, unit) =
        if minutes >= 60 && minutes.is_multiple_of(60) { (minutes / 60, "hour") } else { (minutes, "minute") };
    if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") }
}

/// The line under the name on the lock screen.
pub fn lock_line(reason: Reason) -> String {
    format!("{} Its tabs stay hidden until it opens.", reason.text())
}

/// The window's title while locked: no page's title.
pub fn locked_title(app: &str, profile: &str) -> String {
    format!("{app} — {profile} (locked)")
}

/// What the timer decides: lock now (and why), or not. `idle` is how long
/// the system had no keyboard or mouse input; None when it is not known.
pub fn away_lock(options: LockOptions, locked: bool, idle: Option<Duration>) -> Option<Reason> {
    let limit = options.away_limit()?;
    let minutes = options.lock_after.away_minutes()?;
    (!locked && idle.is_some_and(|idle| idle >= limit)).then_some(Reason::Away(minutes))
}

/// What Windows locking the session does to a window that is not locked.
pub fn session_lock(options: LockOptions, locked: bool) -> Option<Reason> {
    (!locked && options.locks_with_windows()).then_some(Reason::WindowsLocked)
}

/// What a key does while the window is locked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockedKey {
    /// Enter or Space: unlock.
    Unlock,
    /// Windows' own keys for the window (Alt+F4, Alt for the window menu).
    Pass,
    /// Everything else, every shortcut of the app included: nothing.
    Swallow,
}

pub const VK_RETURN: u32 = 0x0D;
pub const VK_SPACE: u32 = 0x20;
pub const VK_MENU: u32 = 0x12;
pub const VK_F4: u32 = 0x73;

/// The key `vk`, pressed with Ctrl, Shift or Alt held or not.
pub fn locked_key(vk: u32, ctrl: bool, shift: bool, alt: bool) -> LockedKey {
    match vk {
        VK_RETURN | VK_SPACE if !ctrl && !shift && !alt => LockedKey::Unlock,
        VK_F4 if alt && !ctrl => LockedKey::Pass,
        VK_MENU => LockedKey::Pass,
        _ => LockedKey::Swallow,
    }
}

/// Whether a script may open a locked window without the verification.
/// Only in a test run: the script control is on (`bench_on`) and the data
/// is in a folder of its own (`NEA_DATA_DIR`). A person's browser never
/// has `NEA_DATA_DIR`, so a program on the script pipe of a normal run
/// cannot open a locked profile.
pub fn script_unlock_allowed(bench_on: bool, data_dir: Option<&std::ffi::OsStr>) -> bool {
    bench_on && data_dir.is_some_and(|d| !d.to_string_lossy().trim().is_empty())
}

/// Whether a change of `id`'s options needs Windows Hello first. Turning a
/// lock on does, so a person who cannot pass Hello cannot lock themselves
/// out. Any change to another profile does too: from one open window, the
/// lock of a locked profile must not be taken off.
pub fn needs_verification(before: LockOptions, after: LockOptions, own: bool) -> bool {
    if before == after {
        return false;
    }
    (!before.locks() && after.locks()) || !own
}

/// Whether Settings lets the person use the two controls. `hello`: Windows
/// Hello is set up (None while that is not known yet). With it missing,
/// nothing new can lock, but a lock that is on can still be taken off.
pub fn controls_enabled(hello: Option<bool>, options: LockOptions) -> bool {
    hello != Some(false) || options.locks()
}

/// The line under the controls in Settings.
pub fn settings_note(hello: Option<bool>, options: LockOptions) -> &'static str {
    match (hello, options.locks()) {
        (Some(false), false) => "Set up Windows Hello in Windows Settings first (Accounts → Sign-in options).",
        (Some(false), true) => "Windows Hello is not set up: the profile opens with your Windows password.",
        _ => "",
    }
}

/// What Windows Hello answered (`UserConsentVerificationResult`, or
/// `ERROR` when the call failed), and what follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloNext {
    Verified,
    /// The person closed the prompt.
    Canceled,
    /// Hello cannot verify now (no device, not set up, busy, too many
    /// tries, an error): the Windows password, when it is an unlock.
    Fallback,
}

pub const HELLO_VERIFIED: u8 = 0;
pub const HELLO_CANCELED: u8 = 6;
/// The call itself failed.
pub const ERROR: u8 = 255;

pub fn after_hello(code: u8) -> HelloNext {
    match code {
        HELLO_VERIFIED => HelloNext::Verified,
        HELLO_CANCELED => HelloNext::Canceled,
        _ => HelloNext::Fallback,
    }
}

/// `UserConsentVerifierAvailability::Available`.
pub fn hello_available(code: u8) -> bool {
    code == 0
}

/// The parts of a name typed in the Windows password prompt: "DOMAIN\user"
/// gives both; "user@domain" and a bare "user" keep the domain the prompt
/// gave, if any.
pub fn split_account(user: &str, domain: &str) -> (String, Option<String>) {
    if let Some((d, u)) = user.split_once('\\') {
        return (u.to_string(), (!d.is_empty()).then(|| d.to_string()));
    }
    (user.to_string(), (!domain.is_empty() && !user.contains('@')).then(|| domain.to_string()))
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/profile_lock.rs"]
#[rustfmt::skip]
mod tests;
