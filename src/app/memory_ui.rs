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
    /// A view was suspended: the renderers of the suspended pages are
    /// trimmed at this time (`memory::ENGINE_TRIM_AFTER`).
    engine_trim_at: Option<Instant>,
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

    /// A window that got no input for `IDLE_TRIM_AFTER` gives back the app's
    /// memory, one time for each pause. Called by the sleep timer.
    pub(super) fn idle_trim(&mut self) {
        let Some(quiet) = super::quiet_time() else { return };
        let now = Instant::now();
        if memory::idle_trim_due(quiet, self.memory.trimmed_at.map(|at| now.duration_since(at))) {
            self.memory.trimmed_at = Some(now);
            crate::diag::trace(|| "idle trim".to_string());
            memory::trim_working_set();
            // The engine too, but not while a page plays sound: a person
            // can watch a video with no input, and a trim of a process
            // that runs can drop a frame.
            let sound =
                self.tabs.all().any(|tab| matches!(&tab.live, super::Live::Ready(web) if web.is_playing_audio()));
            if !sound {
                self.trim_engine(true);
            }
        }
    }

    /// A view was suspended now: its renderer is trimmed soon.
    pub(super) fn suspended_a_view(&mut self, now: Instant) {
        self.memory.engine_trim_at = Some(now + memory::ENGINE_TRIM_AFTER);
    }

    /// How long until the engine trim, if one waits.
    pub(super) fn engine_trim_in(&self, now: Instant) -> Option<Duration> {
        self.memory.engine_trim_at.map(|at| at.saturating_duration_since(now))
    }

    /// The renderers that serve only suspended pages give back their
    /// working set, some time after the last suspend. Called by the sleep
    /// timer.
    pub(super) fn engine_trim(&mut self) {
        let now = Instant::now();
        if self.memory.engine_trim_at.is_none_or(|at| at > now) {
            return;
        }
        self.memory.engine_trim_at = None;
        self.trim_engine(false);
    }

    /// `rest`: also the engine processes that are not renderers.
    fn trim_engine(&self, rest: bool) {
        let suspended: std::collections::HashSet<u32> = self
            .tabs
            .all()
            .filter_map(|tab| match &tab.live {
                super::Live::Ready(web) if web.suspended => web.main_frame(),
                _ => None,
            })
            .collect();
        if let Some(env) = &self.env
            && (rest || !suspended.is_empty())
        {
            crate::web::trim_suspended(env, suspended, rest);
        }
    }

    /// The low memory signal: every tab out of sight to its last stage now.
    pub(super) fn low_memory(&mut self) {
        self.sleep_pass(memory::Pressure::High);
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
