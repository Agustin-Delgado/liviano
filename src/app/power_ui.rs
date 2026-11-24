//! The end of the Windows session and the sleep of the computer.
//!
//! At sign-out, restart and shut down, Windows sends `WM_QUERYENDSESSION`
//! and then `WM_ENDSESSION`, never `WM_CLOSE`: the process stops when
//! `WM_ENDSESSION` returns. Thus the session is written there, or the
//! tabs opened since the last save timer are lost. The window's thread can
//! be busy then (a modal loop, a long handler): `storage::watch_session_end`
//! waits for the queued files on a thread that is free.
//!
//! Before the computer goes to sleep (`PBT_APMSUSPEND`), the session is
//! written too: the battery can go empty while it sleeps. The clock of
//! `Instant` continues while the computer sleeps, so after the resume each
//! tab out of sight would seem idle for all of the night, and the next
//! sleep tick would discard all of them. The idle time of each tab is
//! moved forward by the time that the computer slept.

use std::time::{Duration, Instant};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WM_ENDSESSION, WM_POWERBROADCAST,
        WM_QUERYENDSESSION,
    },
};

use super::{App, with_app};

/// How long the end of the session waits for the files to be written.
/// Windows gives an app approximately 5 s before it asks the person.
const END_SESSION_WRITE_LIMIT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct PowerUi {
    /// When the computer went to sleep; none while it is awake.
    asleep_since: Option<Instant>,
}

/// The time of `at` moved forward by the sleep from `slept_at` to
/// `woke_at`. A time after the start of the sleep is not moved.
pub(super) fn without_sleep(at: Instant, slept_at: Instant, woke_at: Instant) -> Instant {
    if at > slept_at {
        return at;
    }
    (at + woke_at.saturating_duration_since(slept_at)).min(woke_at)
}

/// The power and session messages; `None` lets the window proc go on.
pub(super) fn window_message(_hwnd: HWND, msg: u32, wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_QUERYENDSESSION => {
            let _doing = crate::diag::doing(&"query end session", 0);
            write_or_flush();
            // Nea never stops the end of the session.
            Some(LRESULT(1))
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                let _doing = crate::diag::doing(&"end session", 0);
                write_or_flush();
                crate::diag::flush_now();
            }
            Some(LRESULT(0))
        }
        WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                PBT_APMSUSPEND => {
                    let _doing = crate::diag::doing(&"suspend", 0);
                    with_app(|app| app.going_to_sleep(Instant::now()));
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    let _doing = crate::diag::doing(&"resume", 0);
                    with_app(|app| app.woke_up(Instant::now()));
                }
                _ => {}
            }
            Some(LRESULT(1))
        }
        _ => None,
    }
}

/// The session and the history to disk before the process stops. An app
/// busy in a modal loop (a menu, a dialog) gets the message inside that
/// loop and cannot make a new session: what it saved last (each change
/// within 5 s, and before each modal loop) is written then.
fn write_or_flush() {
    if with_app(|app| app.write_now()).is_none() {
        crate::storage::flush_writes(END_SESSION_WRITE_LIMIT);
    }
}

impl App {
    /// Writes the session and the history, and waits until they are on disk.
    fn write_now(&mut self) {
        self.save(true);
        crate::storage::flush_writes(END_SESSION_WRITE_LIMIT);
    }

    pub(super) fn going_to_sleep(&mut self, now: Instant) {
        self.power.asleep_since.get_or_insert(now);
        self.write_now();
    }

    /// Windows can send two resume messages for one sleep: only the first
    /// one moves the times.
    pub(super) fn woke_up(&mut self, now: Instant) {
        let Some(slept_at) = self.power.asleep_since.take() else { return };
        for tab in self.tabs.all_mut() {
            tab.last_active = without_sleep(tab.last_active, slept_at, now);
        }
        self.invalidate();
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/power_ui.rs"]
#[rustfmt::skip]
mod tests;
