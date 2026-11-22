//! The engine's life beyond single tabs: a view made ahead of time so a new
//! or woken tab doesn't wait for one, and what to do when one of the
//! engine's processes fails.
//!
//! Making a view takes the engine a few hundred milliseconds, and starting
//! the engine itself (no tab open yet) about a second: that was the wait
//! between Enter and the page. So when the field opens — the person is
//! about to type an address —, each keystroke there, the pointer resting on
//! a sleeping tab or a switch while some tab sleeps, the engine starts and
//! one spare view is made. Enter (or the click) takes it and navigates at
//! once; if a spare is still being made, the tab waits for that one instead
//! of asking for a second. Nobody wanted it in `KEEP_SPARE`: it is closed,
//! and an engine started only for it is let go too, so an idle window holds
//! nothing extra — an open field nobody types in included.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, ICoreWebView2Controller,
};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{KillTimer, SetTimer},
};

use super::{App, with_app};
use crate::{
    tabs::Live,
    web::{self, Event},
};

/// How often held engine work looks again whether the strip is at rest.
const HELD_CHECK_MS: u32 = 16;
const TIMER_HELD: usize = 0x43;
/// Engine work is held at most this long, motion or not.
const HOLD_AT_MOST: Duration = Duration::from_millis(600);

/// The id the spare is made under; no tab ever gets it.
pub(super) const SPARE: u64 = u64::MAX;
const KEEP_SPARE: Duration = Duration::from_secs(30);
/// After a spare is taken, the next one waits this long: made at once, it
/// competes with the page that just started.
const REPLENISH_AFTER: Duration = Duration::from_millis(1500);
const TIMER_WARM: usize = 0x42;
const WARM_CHECK_EVERY: Duration = Duration::from_secs(5);
/// A spare wanted while something moves is looked at again this soon.
const SPARE_AFTER_MOTION_MS: u64 = 300;
/// Engine restarts allowed within `RESTART_WINDOW` before giving up.
const MAX_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(120);

#[derive(Default)]
pub(super) struct EngineUi {
    spare: Option<ICoreWebView2Controller>,
    /// A spare is being made.
    making: bool,
    /// The tab waiting for the spare being made.
    claim: Option<u64>,
    /// Keep (or make) a spare until then.
    wanted_until: Option<Instant>,
    /// The engine was started for a spare, with no tab needing it.
    started_for_spare: bool,
    /// No new spare before then (one was just taken).
    hold_until: Option<Instant>,
    restarts: Vec<Instant>,
    /// Page reloads after a lost page process, by tab, within `RESTART_WINDOW`.
    page_reloads: HashMap<u64, Vec<Instant>>,
    /// Tabs waiting for the strip to settle before their view is made.
    held_wakes: Vec<u64>,
    /// Views made while the strip moved, waiting to be shown.
    held_ready: Vec<(u64, windows::core::Result<ICoreWebView2Controller>)>,
    /// The tab that took the spare, until its view is wired.
    from_spare: Option<u64>,
    /// A spare is wanted once the strip is at rest.
    held_spare: bool,
    /// The tab whose page gets the keyboard once the strip is at rest.
    held_focus: Option<u64>,
    /// When the oldest held work was held.
    held_since: Option<Instant>,
}

/// What a failed engine process means for the app.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Response {
    /// The engine recovers on its own (GPU, utility, a frame's process).
    Nothing,
    /// The tab's page is gone or stuck: put it to sleep, reload it if shown.
    Reload,
    /// The whole engine is gone: every view with it.
    Restart,
}

pub(super) fn response(kind: COREWEBVIEW2_PROCESS_FAILED_KIND) -> Response {
    match kind {
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => Response::Restart,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
        | COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => Response::Reload,
        _ => Response::Nothing,
    }
}

/// Whether one more restart fits, counting those in the window before `now`.
fn may_restart(restarts: &mut Vec<Instant>, now: Instant) -> bool {
    restarts.retain(|at| now.duration_since(*at) < RESTART_WINDOW);
    if restarts.len() >= MAX_RESTARTS {
        return false;
    }
    restarts.push(now);
    true
}

/// Whether the page of `tab` loads again after its process ended. A page
/// that ends it on every load (edge://kill, a page that runs out of memory)
/// reloaded about three times a second for ever: after `MAX_RESTARTS` in
/// the window the tab stays asleep until the person selects it again.
fn reload_after_crash(reloads: &mut HashMap<u64, Vec<Instant>>, tab: u64, now: Instant) -> bool {
    reloads.retain(|_, times| times.last().is_some_and(|at| now.duration_since(*at) < RESTART_WINDOW));
    may_restart(reloads.entry(tab).or_default(), now)
}

unsafe extern "system" fn on_warm_timer(_: HWND, _: u32, _: usize, _: u32) {
    with_app(|app| app.warm_tick());
}

unsafe extern "system" fn on_held_timer(_: HWND, _: u32, _: usize, _: u32) {
    let _doing = crate::diag::doing(&"timer:held", 0);
    with_app(|app| app.release_held());
}

/// Whether held engine work may run now: the strip is at rest, or it was
/// held for too long already (a strip that never settles).
fn may_release(settling: bool, held_since: Option<Instant>, now: Instant) -> bool {
    !settling || held_since.is_some_and(|since| now.duration_since(since) >= HOLD_AT_MOST)
}

impl App {
    /// A tab needs its view. Making one holds the UI thread inside the
    /// engine for 50-120 ms. For a tab in the background nobody waits on
    /// the page, so the making waits until the strip is at rest: its slide
    /// never pays for it. A tab on screen is the page the person waits for,
    /// and goes at once (from the spare when there is one, a few ms).
    pub(super) fn wake(&mut self, id: u64) {
        let private = self.tabs.get(id).is_some_and(|t| t.private);
        let cheap = !private
            && self.env.is_some()
            && (self.engine.spare.is_some() || (self.engine.making && self.engine.claim.is_none()));
        if cheap || self.on_screen(id) {
            self.wake_now(id);
            return;
        }
        if !self.engine.held_wakes.contains(&id) {
            self.engine.held_wakes.push(id);
        }
        self.hold();
    }

    /// A view is ready: shown now, or once the strip is at rest. The spare
    /// was made long before: wiring it is cheap, and the page starts now.
    pub(super) fn view_ready(&mut self, id: u64, result: windows::core::Result<ICoreWebView2Controller>) {
        let from_spare = self.engine.from_spare.take() == Some(id);
        if !from_spare && !self.on_screen(id) && self.something_moves(Instant::now()) {
            self.engine.held_ready.push((id, result));
            self.hold();
            return;
        }
        self.controller_ready(id, result);
    }

    /// Gives `id`'s page the keyboard once the strip is at rest, if it is
    /// still the active tab then.
    pub(super) fn focus_when_settled(&mut self, id: u64) {
        self.engine.held_focus = Some(id);
        self.hold();
    }

    fn on_screen(&self, id: u64) -> bool {
        self.shown_panes().iter().any(|(shown, _)| *shown == id)
    }

    /// The strip's layout, the field or a fading window moves: engine work
    /// waits. The loading ring and hovers do not count (see `settling`).
    pub(super) fn something_moves(&self, now: Instant) -> bool {
        self.motion.settling(now) || crate::omnibar::moving() || crate::fade::running()
    }

    fn hold(&mut self) {
        self.engine.held_since.get_or_insert_with(Instant::now);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HELD, HELD_CHECK_MS, Some(on_held_timer));
        }
    }

    /// Runs the held engine work if the strip is at rest; else looks again soon.
    fn release_held(&mut self) {
        let now = Instant::now();
        if !may_release(self.something_moves(now), self.engine.held_since, now) {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HELD);
        }
        self.engine.held_since = None;
        for (id, result) in std::mem::take(&mut self.engine.held_ready) {
            self.controller_ready(id, result);
        }
        for id in std::mem::take(&mut self.engine.held_wakes) {
            self.wake_now(id);
        }
        if std::mem::take(&mut self.engine.held_spare) {
            self.make_spare();
        }
        if let Some(id) = self.engine.held_focus.take()
            && self.tabs.active == Some(id)
        {
            self.focus_page();
        }
    }

    /// Someone is about to need a view: start the engine and make a spare.
    pub(super) fn warm_up(&mut self) {
        self.engine.wanted_until = Some(Instant::now() + KEEP_SPARE);
        if self.env.is_none() && !self.env_requested {
            self.engine.started_for_spare = self.tabs.tabs.is_empty();
            self.request_environment();
        }
        // Asked for in the middle of a switch or a key press: the motion it
        // starts has not drawn its first frame yet.
        self.engine.held_spare = true;
        self.hold();
    }

    /// Makes the spare now if none is there or coming, and sets when to look
    /// again.
    fn make_spare(&mut self) {
        let mut next = WARM_CHECK_EVERY;
        // Making a view holds the UI thread for 50 ms or more inside the
        // engine: never while something moves on screen.
        let moving = self.something_moves(Instant::now());
        if moving {
            next = Duration::from_millis(SPARE_AFTER_MOTION_MS);
        }
        if let Some(env) = self.env.clone()
            && self.engine.spare.is_none()
            && !self.engine.making
            && !moving
            // A spare is a renderer nobody uses yet: not while memory is low.
            && super::memory_load() < self.settings.memory_pressure_percent
        {
            match self.engine.hold_until.and_then(|until| until.checked_duration_since(Instant::now())) {
                Some(wait) => next = wait.max(Duration::from_millis(50)),
                None => self.engine.making = web::create_controller(&env, self.hwnd, SPARE).is_ok(),
            }
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_WARM, next.as_millis() as u32, Some(on_warm_timer));
        }
    }

    /// A switch with sleeping tabs around: the next one may wake one.
    pub(super) fn warm_up_if_asleep(&mut self) {
        if self.tabs.tabs.iter().any(|t| !t.private && matches!(t.live, Live::Asleep)) {
            self.warm_up();
        }
    }

    /// The engine just started: make the spare if it is still wanted.
    pub(super) fn warm_environment_ready(&mut self) {
        if self.engine.wanted_until.is_some_and(|until| Instant::now() < until) {
            // A tab asked for its own view first: the engine's first view
            // also starts its browser process, and a spare made alongside
            // only slows that one down.
            if self.tabs.tabs.iter().any(|t| matches!(t.live, Live::Creating)) {
                self.engine.hold_until = Some(Instant::now() + REPLENISH_AFTER);
            }
            self.warm_up();
        }
    }

    /// A tab needs a view: the spare's, when there is one (true), or the one
    /// being made, which it then waits for (true). False: make its own.
    pub(super) fn take_spare(&mut self, id: u64) -> bool {
        if let Some(controller) = self.engine.spare.take() {
            crate::diag::trace(|| format!("spare taken tab={id}"));
            self.engine.from_spare = Some(id);
            self.engine.hold_until = Some(Instant::now() + REPLENISH_AFTER);
            // Through the queue, as any new view: the caller is in the
            // middle of showing tabs.
            web::push(Event::ControllerReady(id, Ok(controller)));
            return true;
        }
        if self.engine.making && self.engine.claim.is_none() {
            crate::diag::trace(|| format!("spare claimed tab={id}"));
            self.engine.claim = Some(id);
            return true;
        }
        false
    }

    pub(super) fn spare_ready(&mut self, result: windows::core::Result<ICoreWebView2Controller>) {
        self.engine.making = false;
        let waiting =
            self.engine.claim.take().filter(|id| self.tabs.get(*id).is_some_and(|t| matches!(t.live, Live::Creating)));
        match (result, waiting) {
            (Ok(controller), Some(id)) => self.controller_ready(id, Ok(controller)),
            (Ok(controller), None) => unsafe {
                // Visible but zero-sized: nothing on screen, and ready to draw
                // the moment it gets real bounds (see `WebTab::new`).
                let _ = controller.SetBounds(RECT::default());
                self.engine.spare = Some(controller);
            },
            (Err(_), Some(id)) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.live = Live::Asleep;
                }
                self.show_active();
            }
            (Err(_), None) => {}
        }
    }

    /// Every few seconds while a spare is wanted or held: let it go once
    /// nobody has wanted it for a while.
    fn warm_tick(&mut self) {
        if self.engine.wanted_until.is_some_and(|until| Instant::now() < until) {
            self.make_spare();
            return;
        }
        if !self.engine.making {
            self.release_spare();
        }
    }

    pub(super) fn release_spare(&mut self) {
        self.engine.wanted_until = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_WARM);
        }
        if let Some(controller) = self.engine.spare.take() {
            unsafe {
                let _ = controller.Close();
            }
        }
        // Started only for the spare and still no tab: no engine at all.
        if self.engine.started_for_spare && self.tabs.tabs.is_empty() {
            self.env = None;
            self.env_requested = false;
        }
        self.engine.started_for_spare = false;
    }

    pub(super) fn process_failed(&mut self, id: u64, kind: COREWEBVIEW2_PROCESS_FAILED_KIND) {
        match response(kind) {
            Response::Nothing => {}
            Response::Reload => {
                let visible = self.visible_tabs().contains(&id);
                self.discard(id);
                if visible && reload_after_crash(&mut self.engine.page_reloads, id, Instant::now()) {
                    self.show_active();
                }
            }
            Response::Restart => self.restart_engine(id),
        }
        self.invalidate();
    }

    /// The browser process died: every view went with it. Each view reports
    /// it; the first one with a live view does the work.
    fn restart_engine(&mut self, id: u64) {
        if !self.tabs.get(id).is_some_and(|t| matches!(t.live, Live::Ready(_))) {
            return;
        }
        for tab in &mut self.tabs.tabs {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
            tab.loading = false;
        }
        self.engine.spare = None;
        self.engine.making = false;
        self.engine.claim = None;
        self.env = None;
        if may_restart(&mut self.engine.restarts, Instant::now()) {
            self.env_requested = false;
            self.env_error = None;
            self.show_active();
        } else {
            self.env_error =
                Some("The page engine stopped many times in a row. Close Nea and open it again.".to_string());
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/engine_ui.rs"]
#[rustfmt::skip]
mod tests;
