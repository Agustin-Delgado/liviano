//! The window's part of the memory policy (refer to `memory`): the pages of
//! a minimized window, and the low memory signal of Windows.
//!
//! A minimized window keeps its layout: the views keep their size, so
//! when the window comes back the engine only shows them again, with no
//! new layout of the page. After `MINIMIZED_HIDE_AFTER` the views are
//! hidden (the engine then stops drawing them and frees what drew them),
//! the window's own drawing target is released, and the app gives back
//! its working set.

use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SIZE_MINIMIZED, SetTimer, WM_APP};

use super::{App, with_app};
use crate::memory;

/// Windows says that the system is low on memory (`memory::watch_low_memory`).
pub(super) const WM_APP_LOW_MEMORY: u32 = WM_APP + 4;
const TIMER_MINIMIZED: usize = 0x4B;
/// Under memory pressure, the app gives back its working set at most this
/// often: each time, the pages it uses next come back from the page file.
const TRIM_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct MemoryUi {
    minimized_at: Option<Instant>,
    /// The views are hidden because the window is minimized.
    pages_hidden: bool,
    trimmed_at: Option<Instant>,
}

impl MemoryUi {
    pub(super) fn minimized(&self) -> bool {
        self.minimized_at.is_some()
    }

    pub(super) fn pages_hidden(&self) -> bool {
        self.pages_hidden
    }

    /// Whether a trim is due now, and if so, notes it.
    fn trim_due(&mut self, now: Instant) -> bool {
        if trim_due(self.trimmed_at, now) {
            self.trimmed_at = Some(now);
            return true;
        }
        false
    }
}

/// A trim is due when none was done in the last `TRIM_EVERY`.
fn trim_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.duration_since(at) >= TRIM_EVERY)
}

unsafe extern "system" fn on_minimized_timer(_: windows::Win32::Foundation::HWND, _: u32, _: usize, _: u32) {
    let _doing = crate::diag::doing(&"timer:minimized", 0);
    with_app(|app| app.minimized_tick());
}

impl App {
    /// `WM_SIZE` with its kind. False: the layout stays as it is (the window
    /// was minimized).
    pub(super) fn window_sized(&mut self, kind: u32) -> bool {
        if kind == SIZE_MINIMIZED {
            if self.memory.minimized_at.is_none() {
                self.memory.minimized_at = Some(Instant::now());
                unsafe {
                    SetTimer(
                        Some(self.hwnd),
                        TIMER_MINIMIZED,
                        memory::MINIMIZED_HIDE_AFTER.as_millis() as u32,
                        Some(on_minimized_timer),
                    );
                }
            }
            return false;
        }
        if self.memory.minimized_at.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_MINIMIZED);
            }
            self.memory.pages_hidden = false;
        }
        true
    }

    fn minimized_tick(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_MINIMIZED);
        }
        if !self.memory.minimized() {
            return;
        }
        // The idle time of the pages on screen starts now (refer to `sleep_pass`).
        let now = Instant::now();
        for id in self.visible_tabs() {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.last_active = now;
            }
        }
        self.memory.pages_hidden = true;
        self.show_active();
        // Made again by the first paint after the window comes back.
        self.surface = Default::default();
        self.painted_sheet = None;
        self.memory.trimmed_at = Some(now);
        memory::trim_working_set();
    }

    /// The low memory signal: every tab out of sight to its last stage now.
    pub(super) fn low_memory(&mut self) {
        self.sleep_pass(true);
    }

    /// Under pressure: the app's working set back to the system, at most
    /// every `TRIM_EVERY`.
    pub(super) fn trim_under_pressure(&mut self) {
        if self.memory.trim_due(Instant::now()) {
            memory::trim_working_set();
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/memory_ui.rs"]
#[rustfmt::skip]
mod tests;
