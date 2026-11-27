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

/// A change in whether a person can see the window (`App::sight_changed`).
pub(super) enum Sight {
    Screen { on: bool },
    Session { locked: bool },
}

#[derive(Default)]
pub(super) struct MemoryUi {
    minimized_at: Option<Instant>,
    /// The views are hidden because the window is minimized.
    pages_hidden: bool,
    /// The screen is off: nobody can see the pages.
    screen_off: bool,
    /// The session is locked: nobody can see the pages.
    locked: bool,
    trimmed_at: Option<Instant>,
    /// A view was suspended: the renderers of the suspended pages are
    /// trimmed at this time (`memory::ENGINE_TRIM_AFTER`).
    engine_trim_at: Option<Instant>,
    /// How many of the trims after a suspend ran (`memory::ENGINE_TRIM_AGAIN`
    /// tells when the next ones come).
    engine_trims: usize,
    /// The last trim of the engine at rest. It has its own time: under
    /// memory pressure the app trims itself again and again, and that must
    /// not stop the trim of the engine.
    engine_rested_at: Option<Instant>,
    /// Since when a window of a different program is in front, as the sleep
    /// timer saw it.
    background_since: Option<Instant>,
}

impl MemoryUi {
    pub(super) fn minimized(&self) -> bool {
        self.minimized_at.is_some()
    }

    pub(super) fn pages_hidden(&self) -> bool {
        self.pages_hidden || self.unseen()
    }

    /// Nobody can see the window: the screen is off or the session is locked.
    fn unseen(&self) -> bool {
        self.screen_off || self.locked
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

    /// The screen went off or came on, or the session was locked or
    /// unlocked. While nobody can see the window, its views are hidden: the
    /// engine then stops drawing them, as it does for a minimized window. A
    /// page with an animation that never stops would use the processor for
    /// all of that time. Sound continues.
    pub(super) fn sight_changed(&mut self, sight: Sight) {
        let before = self.memory.unseen();
        match sight {
            Sight::Screen { on } => self.memory.screen_off = !on,
            Sight::Session { locked } => self.memory.locked = locked,
        }
        let unseen = self.memory.unseen();
        if unseen == before {
            return;
        }
        crate::diag::trace(|| format!("pages unseen: {unseen}"));
        if unseen {
            // The idle time of the pages on screen starts now (refer to `sleep_pass`).
            let now = Instant::now();
            for id in self.visible_tabs() {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.last_active = now;
                }
            }
        }
        self.show_active();
        if !unseen {
            self.invalidate();
        }
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
        self.trim_engine_at_rest();
    }

    /// A window that got no input for `IDLE_TRIM_AFTER` gives back the app's
    /// memory, one time for each pause. The engine gives back its memory
    /// also when the window is in the background for that time, and again
    /// each `REST_TRIM_EVERY`. Called by the sleep timer.
    pub(super) fn idle_trim(&mut self) {
        let now = Instant::now();
        if !super::in_background() {
            self.memory.background_since = None;
        }
        let background = super::in_background().then(|| *self.memory.background_since.get_or_insert(now));
        let away = super::away_time(background.map(|since| now.duration_since(since)));
        let Some(quiet) = super::quiet_time() else { return };
        if memory::idle_trim_due(quiet, self.memory.trimmed_at.map(|at| now.duration_since(at))) {
            self.memory.trimmed_at = Some(now);
            crate::diag::trace(|| "idle trim".to_string());
            memory::trim_working_set();
        }
        let rested = self.memory.engine_rested_at.map(|at| now.duration_since(at));
        if away.is_some_and(|away| memory::rest_trim_due(away, rested)) {
            self.trim_engine_at_rest();
        }
    }

    /// Nobody uses the window: the engine gives back its working set too.
    /// Not while a page plays sound: a person can watch a video with no
    /// input, and a trim of a process that runs can drop a frame.
    fn trim_engine_at_rest(&mut self) {
        self.memory.engine_rested_at = Some(Instant::now());
        let sound = self.tabs.all().any(|tab| matches!(&tab.live, super::Live::Ready(web) if web.is_playing_audio()));
        if !sound {
            let _ = self.trim_engine(true);
        }
    }

    /// A view was suspended now: its renderer is trimmed soon.
    pub(super) fn suspended_a_view(&mut self, now: Instant) {
        self.memory.engine_trim_at = Some(now + memory::ENGINE_TRIM_AFTER);
        self.memory.engine_trims = 0;
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
        let wait =
            memory::ENGINE_TRIM_AGAIN.get(self.memory.engine_trims).copied().unwrap_or(memory::ENGINE_TRIM_EVERY);
        self.memory.engine_trims += 1;
        // With no suspended page there is nothing to trim: the next suspend starts the trims again.
        self.memory.engine_trim_at = self.trim_engine(false).then(|| now + wait);
    }

    /// `rest`: all the engine processes, also the renderer of the page on
    /// screen.
    /// Tells whether there are suspended pages.
    fn trim_engine(&self, rest: bool) -> bool {
        let suspended: std::collections::HashSet<u32> = self
            .tabs
            .all()
            .filter_map(|tab| match &tab.live {
                super::Live::Ready(web) if web.suspended => web.main_frame(),
                _ => None,
            })
            .collect();
        let any = !suspended.is_empty();
        if let Some(env) = &self.env
            && (rest || any)
        {
            crate::web::trim_suspended(env, suspended, rest);
        }
        any
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
