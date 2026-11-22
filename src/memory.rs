//! How Nea keeps its memory low: the stages of a tab out of sight, the
//! window while minimized, and what changes when the system is low on
//! memory.
//!
//! A tab out of sight goes through three stages, each cheaper than the one
//! before and each slower to come back from:
//!
//! 1. Lowered, after `LOWER_AFTER`: the engine may drop what it can make
//!    again (caches, decoded images, garbage). Scripts and timers go on.
//!    Coming back costs a few milliseconds of decoding.
//! 2. Suspended, after the "suspend" setting: scripts and timers stop, and
//!    the engine trims the page's memory. Coming back is a resume.
//! 3. Discarded, after the "discard" setting: the view and its renderer are
//!    closed and only the address and the title stay. Coming back loads
//!    the page again.
//!
//! Stage 2 gives back the working set but not the commit: a suspended view
//! still holds tens of megabytes. Thus the "live tabs out of sight" setting
//! sets how many views out of sight stay live: the most recently seen
//! ones. Nea discards the others at once, as at stage 3. Tabs that are
//! never discarded (pinned, private, playing sound, kept awake) are not
//! counted.
//!
//! The tabs on screen stay as they are. While the window is minimized, they
//! are hidden after `MINIMIZED_HIDE_AFTER` and then follow stages 1 and 2
//! from the time the window was minimized, but are never discarded: they
//! are the pages the person sees first when the window comes back.
//!
//! When the system is low on memory (the load setting, or the low memory
//! signal of Windows), every tab out of sight goes to its last stage at
//! once, the app gives back its own working set, and no spare view is
//! made ahead of time.

use std::time::{Duration, Instant};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    System::{
        Memory::{CreateMemoryResourceNotification, LowMemoryResourceNotification},
        ProcessStatus::EmptyWorkingSet,
        Threading::{GetCurrentProcess, INFINITE, WaitForSingleObject},
    },
    UI::WindowsAndMessaging::PostMessageW,
};

/// A hidden tab is lowered this long after it left the screen. Shorter, and
/// a quick look back at the last tab decodes its images again.
pub const LOWER_AFTER: Duration = Duration::from_secs(20);
/// A window minimized for this long hides its pages. A minimize that comes
/// right back (a look at the desktop) costs nothing.
pub const MINIMIZED_HIDE_AFTER: Duration = Duration::from_secs(3);
/// After a low memory signal, the next one is waited for this long later.
/// The signal stays on while memory is low, and the sleep timer already
/// looks at the load every few seconds.
pub const PRESSURE_RECHECK: Duration = Duration::from_secs(30);

/// When a tab out of sight goes to its next stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub lower_after: Duration,
    pub suspend_after: Duration,
    pub discard_after: Duration,
}

impl Policy {
    pub fn new(suspend_after_secs: u64, discard_after_secs: u64) -> Self {
        let suspend_after = Duration::from_secs(suspend_after_secs);
        Policy {
            // A suspend set sooner than the lower stage takes its place.
            lower_after: LOWER_AFTER.min(suspend_after),
            suspend_after,
            discard_after: Duration::from_secs(discard_after_secs),
        }
    }
}

/// What the policy needs to know about one tab with a live view that is out
/// of sight, or on screen in a minimized window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// How long since the tab was last on screen.
    pub idle: Duration,
    /// On screen in a minimized window: it may sleep but not be discarded.
    pub minimized_on_screen: bool,
    /// A site the person keeps awake.
    pub keep_awake: bool,
    pub pinned: bool,
    pub private: bool,
    pub playing_audio: bool,
    pub lowered: bool,
    pub suspended: bool,
}

/// The next stage of one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep,
    Lower,
    Suspend,
    Discard,
}

/// The next stage of a tab, now. `pressure`: the system is low on memory.
pub fn step(facts: &Facts, policy: &Policy, pressure: bool) -> Step {
    if facts.keep_awake {
        return Step::Keep;
    }
    // Pinned and private tabs may sleep but are never destroyed; a page that
    // plays sound is never cut off.
    let may_discard = !facts.pinned && !facts.private && !facts.playing_audio && !facts.minimized_on_screen;
    if may_discard && (pressure || facts.idle >= policy.discard_after) {
        return Step::Discard;
    }
    if !facts.suspended && (pressure || facts.idle >= policy.suspend_after) {
        return Step::Suspend;
    }
    if !facts.suspended && !facts.lowered && facts.idle >= policy.lower_after {
        return Step::Lower;
    }
    Step::Keep
}

/// Of the tabs out of sight that may be discarded (when each was last on
/// screen, and its id), the ones beyond the `cap` seen most recently: the
/// least recently seen first. A suspended view still holds most of its
/// memory (its commit), so only a cap on their number bounds the total.
pub fn over_cap(candidates: &[(Instant, u64)], cap: usize) -> Vec<(Instant, u64)> {
    let mut newest_first = candidates.to_vec();
    newest_first.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut over = newest_first.split_off(cap.min(newest_first.len()));
    over.reverse();
    over
}

/// Posts `message` to `hwnd` each time Windows signals that the system is
/// low on memory. The thread waits on the signal and uses no CPU.
pub fn watch_low_memory(hwnd: HWND, message: u32) {
    let window = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("low memory".into()).spawn(move || unsafe {
        let Ok(signal) = CreateMemoryResourceNotification(LowMemoryResourceNotification) else { return };
        loop {
            if WaitForSingleObject(signal, INFINITE).0 != 0 {
                return;
            }
            crate::diag::trace(|| "low memory signal".to_string());
            if PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0)).is_err() {
                return;
            }
            std::thread::sleep(PRESSURE_RECHECK);
        }
    });
}

/// Gives the app's working set back to the system. Only the pages in RAM
/// go, not the memory the app holds (its commit): the pages come back
/// from the page file when they are used again. For a window that nobody
/// looks at (minimized) or a system that is low on memory.
pub fn trim_working_set() {
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/memory.rs"]
#[rustfmt::skip]
mod tests;
