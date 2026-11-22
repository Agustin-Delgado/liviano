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
    COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE, ICoreWebView2Controller, ICoreWebView2Environment,
};
use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{GetClassNameW, KillTimer, MSG, PostMessageW, SetTimer, WM_USER},
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
/// Views being made at the same time for tabs out of sight. Each one keeps
/// the engine's browser process busy: with 200 tabs made at once, each call
/// to the engine (a hide, a show) waited on it for about a second.
const MAX_IN_FLIGHT: usize = 2;

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
/// The engine did not start (the runtime is missing, or being updated):
/// asked for again after a wait that doubles each time, up to this.
const ENV_RETRY_MAX: Duration = Duration::from_secs(60);
const TIMER_ENV_RETRY: usize = 0x44;

/// The engine's own work on this thread (a new view wired to the window,
/// its first showing) comes as one message, posted to its message window
/// whenever work waits: 25-95 ms each, measured. Inside a slide that is two
/// to five frames lost. While the strip or the field moves it waits for the
/// motion to end, at most `ENGINE_HOLD_AT_MOST`: the engine works on in its
/// own processes meanwhile, only what it hands back here is later. After a
/// click or a tab switch the person looks at the slide, and the page comes
/// a little later for it. After Enter in the field they look at where the
/// page comes: the work for it goes at once (a hold made Enter -> page
/// 45-90 ms slower, measured), until the page arrives.
const ENGINE_WORK: u32 = WM_USER + 1;
const ENGINE_CLASS: &str = "Chrome_MessageWindow";
const ENGINE_HOLD_AT_MOST: Duration = Duration::from_millis(250);
const TIMER_ENGINE_HOLD: usize = 0x44;
const ENGINE_HOLD_CHECK_MS: u32 = 8;
/// A page asked for in the field longer ago than this no longer counts as
/// waited for (a slow server, or its end was not seen).
const TYPED_PAGE_WAITS_AT_MOST: Duration = Duration::from_secs(3);

thread_local! {
    /// The engine's message window, once seen.
    static ENGINE_WINDOW: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// Engine messages held back, in order, and since when.
    static ENGINE_HELD: std::cell::RefCell<(Vec<MSG>, Option<Instant>)> =
        const { std::cell::RefCell::new((Vec::new(), None)) };
    /// This motion already held the engine for as long as it may: what
    /// comes until it ends goes at once (without it, the messages given
    /// back were held again, the limit never applied).
    static ENGINE_HOLD_SPENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the engine's work waits now: something moves, nobody waits for
/// a page they typed, and it has not waited for too long already.
pub(super) fn hold_engine_work(moving: bool, typed: bool, held_since: Option<Instant>, now: Instant) -> bool {
    moving && !typed && held_since.is_none_or(|since| now.duration_since(since) < ENGINE_HOLD_AT_MOST)
}

/// Whether the page asked for in the field at `asked` is still waited for.
pub(super) fn typed_page_waits(asked: Option<Instant>, now: Instant) -> bool {
    asked.is_some_and(|asked| now.duration_since(asked) < TYPED_PAGE_WAITS_AT_MOST)
}

/// Whether one more engine message is held: behind one already held it
/// always is (the order stays); else only when the work waits now and this
/// motion has not used up its hold.
pub(super) fn holds_message(behind: bool, spent: bool, waits: bool) -> bool {
    behind || (!spent && waits)
}

fn is_engine_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    if ENGINE_WINDOW.with(|w| w.get()) == hwnd.0 as isize {
        return true;
    }
    let mut name = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut name) }.max(0) as usize;
    let found = String::from_utf16_lossy(&name[..len.min(name.len())]) == ENGINE_CLASS;
    if found {
        ENGINE_WINDOW.with(|w| w.set(hwnd.0 as isize));
    }
    found
}

/// Called by the message loop before it dispatches `msg`: true when the
/// message is the engine's work and waits for the motion to end.
pub(super) fn hold_engine_message(owner: HWND, msg: &MSG) -> bool {
    if msg.message != ENGINE_WORK || !is_engine_window(msg.hwnd) {
        return false;
    }
    let now = Instant::now();
    let (behind, since) = ENGINE_HELD.with(|h| {
        let h = h.borrow();
        (!h.0.is_empty(), h.1)
    });
    let (moving, typed) = super::with_app(|app| app.motion_holds_engine(now)).unwrap_or_default();
    if !moving {
        ENGINE_HOLD_SPENT.with(|s| s.set(false));
    }
    let spent = ENGINE_HOLD_SPENT.with(|s| s.get());
    if !holds_message(behind, spent, hold_engine_work(moving, typed, since, now)) {
        return false;
    }
    ENGINE_HELD.with(|h| {
        let mut h = h.borrow_mut();
        h.0.push(*msg);
        h.1.get_or_insert(now);
    });
    crate::diag::trace(|| "engine work held".to_string());
    unsafe {
        SetTimer(Some(owner), TIMER_ENGINE_HOLD, ENGINE_HOLD_CHECK_MS, Some(on_engine_hold_timer));
    }
    true
}

unsafe extern "system" fn on_engine_hold_timer(hwnd: HWND, _: u32, _: usize, _: u32) {
    let now = Instant::now();
    let since = ENGINE_HELD.with(|h| h.borrow().1);
    let (moving, typed) = super::with_app(|app| app.motion_holds_engine(now)).unwrap_or_default();
    if hold_engine_work(moving, typed, since, now) {
        return;
    }
    ENGINE_HOLD_SPENT.with(|s| s.set(moving));
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ENGINE_HOLD);
    }
    release_engine_messages();
}

/// Gives the held engine messages back to the queue, in order.
fn release_engine_messages() {
    let held = ENGINE_HELD.with(|h| {
        let mut h = h.borrow_mut();
        h.1 = None;
        std::mem::take(&mut h.0)
    });
    if !held.is_empty() {
        crate::diag::trace(|| format!("engine work released ({})", held.len()));
    }
    for msg in held {
        unsafe {
            let _ = PostMessageW(Some(msg.hwnd), msg.message, msg.wParam, msg.lParam);
        }
    }
}

#[derive(Default)]
pub(super) struct EngineUi {
    spare: Option<ICoreWebView2Controller>,
    /// A spare is being made.
    making: bool,
    /// The tab waiting for the spare being made.
    claim: Option<u64>,
    /// When a page was last asked for in the field (see `typed_page_waits`).
    typed_at: Option<Instant>,
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
    /// Tabs on screen waiting for the engine to make fewer views.
    shown_wakes: Vec<u64>,
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
    /// Starts of the engine that failed in a row.
    env_failures: u32,
    /// No new start of the engine before then.
    env_retry_at: Option<Instant>,
}

/// What an answer to a view request means for its tab.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Arrival {
    /// The tab waits for this view: wire it.
    Wire,
    /// Nobody waits for it any more (the tab closed, went to sleep, or got
    /// a view from another request): the view is closed and the tab stays
    /// as it is. A tab with a view must never lose it to a late answer.
    Unwanted,
    /// No view: the tab sleeps. `retry` when it is on screen and may try
    /// again, so that a page in sight does not stay empty.
    Failed { retry: bool },
}

/// `waiting`: the tab exists and waits for a view (`Live::Creating`).
pub(super) fn arrival(waiting: bool, made: bool, on_screen: bool, may_retry: impl FnOnce() -> bool) -> Arrival {
    match (waiting, made) {
        (false, _) => Arrival::Unwanted,
        (true, true) => Arrival::Wire,
        (true, false) => Arrival::Failed { retry: on_screen && may_retry() },
    }
}

/// How long to wait before the engine is asked for again after `failures`
/// starts in a row that failed: 1 s, 2 s, 4 s… up to `ENV_RETRY_MAX`.
pub(super) fn env_retry_delay(failures: u32) -> Duration {
    let doubled = 1u64.checked_shl(failures.saturating_sub(1)).unwrap_or(u64::MAX);
    Duration::from_secs(doubled).min(ENV_RETRY_MAX)
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

/// After the last restart allowed: how long until the oldest one leaves
/// the window and the engine may start again (at least a second).
pub(super) fn restart_wait(restarts: &[Instant], now: Instant) -> Duration {
    let left = restarts
        .iter()
        .min()
        .map_or(Duration::ZERO, |oldest| (*oldest + RESTART_WINDOW).saturating_duration_since(now));
    left.max(Duration::from_secs(1))
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

unsafe extern "system" fn on_env_retry_timer(hwnd: HWND, _: u32, _: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ENV_RETRY);
    }
    let _doing = crate::diag::doing(&"timer:engine retry", 0);
    with_app(|app| app.retry_environment());
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

/// One piece of held engine work. Each one holds the UI thread inside the
/// engine for 20-120 ms: one in each tick, so the window paints and takes
/// input between them.
#[derive(Debug, PartialEq, Eq)]
enum Held {
    /// A view is made for a tab on screen.
    Shown,
    /// The keyboard to the active page.
    Focus,
    /// A view that is ready is wired.
    Ready,
    /// A view is made for a tab out of sight.
    Wake,
    Spare,
    /// Work is left, but the strip moves or the engine makes enough views now.
    Wait,
    Done,
}

/// The held work, counted.
#[derive(Clone, Copy, Debug, Default)]
struct Work {
    /// Tabs on screen that wait for their view.
    shown: usize,
    focus: bool,
    ready: usize,
    wakes: usize,
    spare: bool,
}

/// The next piece of held work. A page on screen is what the person waits
/// for: it goes first, also while the strip moves. Then, at rest, the
/// keyboard, views that are ready, new views and the spare. New views are
/// made at most `MAX_IN_FLIGHT` at once.
fn next_held(work: Work, in_flight: usize, at_rest: bool) -> Held {
    let room = in_flight < MAX_IN_FLIGHT;
    if work.shown > 0 && room {
        Held::Shown
    } else if !at_rest {
        Held::Wait
    } else if work.focus {
        Held::Focus
    } else if work.ready > 0 {
        Held::Ready
    } else if work.wakes > 0 && room && work.shown == 0 {
        Held::Wake
    } else if work.wakes > 0 || work.shown > 0 {
        Held::Wait
    } else if work.spare {
        Held::Spare
    } else {
        Held::Done
    }
}

impl App {
    /// A tab needs its view. Making one holds the UI thread inside the
    /// engine for 50-120 ms. For a tab in the background nobody waits on
    /// the page, so the making waits until the strip is at rest: its slide
    /// never pays for it. A tab on screen is the page the person waits for,
    /// and goes at once (from the spare when there is one, a few ms), unless
    /// the engine already makes `MAX_IN_FLIGHT` views: then it goes first
    /// when one is done, if it is still on screen then. A quick walk over
    /// sleeping tabs thus makes the views of the tabs it stops on, not of
    /// each tab it passes.
    pub(super) fn wake(&mut self, id: u64) {
        let private = self.tabs.get(id).is_some_and(|t| t.private);
        let cheap = !private
            && self.env.is_some()
            && (self.engine.spare.is_some() || (self.engine.making && self.engine.claim.is_none()));
        let shown = self.on_screen(id);
        if cheap || (shown && self.in_flight() < MAX_IN_FLIGHT) {
            self.wake_now(id);
            return;
        }
        let list = if shown { &mut self.engine.shown_wakes } else { &mut self.engine.held_wakes };
        if !list.contains(&id) {
            list.push(id);
        }
        self.hold();
    }

    /// Views the engine makes now.
    fn in_flight(&self) -> usize {
        let creating = self.tabs.tabs.iter().filter(|t| matches!(t.live, Live::Creating)).count();
        // A spare that a tab claimed is that tab's view, counted already.
        creating + usize::from(self.engine.making && self.engine.claim.is_none())
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

    /// Whether the strip's layout or the field coming in moves, and whether
    /// a page typed in the field is waited for: the engine's own work on
    /// this thread waits for the first, not for the second (see
    /// `hold_engine_message`). The field going out is no motion here: it
    /// goes after Enter.
    pub(super) fn motion_holds_engine(&self, now: Instant) -> (bool, bool) {
        let moving = self.motion.settling(now) || crate::omnibar::opening();
        (moving, typed_page_waits(self.engine.typed_at, now))
    }

    /// The person asked for a page in the field.
    pub(super) fn page_typed(&mut self) {
        self.engine.typed_at = Some(Instant::now());
    }

    /// A page arrived: nobody waits for a typed one any more.
    pub(super) fn page_arrived(&mut self) {
        self.engine.typed_at = None;
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

    /// Runs one piece of the held engine work; the timer looks again soon
    /// while work is left.
    fn release_held(&mut self) {
        let now = Instant::now();
        // A tab that left the screen before its turn sleeps on.
        let shown: Vec<u64> = self.shown_panes().iter().map(|(id, _)| *id).collect();
        self.engine.shown_wakes.retain(|id| shown.contains(id));
        let work = Work {
            shown: self.engine.shown_wakes.len(),
            focus: self.engine.held_focus.is_some(),
            ready: self.engine.held_ready.len(),
            wakes: self.engine.held_wakes.len(),
            spare: self.engine.held_spare,
        };
        let at_rest = may_release(self.something_moves(now), self.engine.held_since, now);
        match next_held(work, self.in_flight(), at_rest) {
            Held::Shown => {
                let id = self.engine.shown_wakes.remove(0);
                self.wake_now(id);
            }
            Held::Focus => {
                if let Some(id) = self.engine.held_focus.take()
                    && self.tabs.active == Some(id)
                {
                    self.focus_page();
                }
            }
            Held::Ready => {
                let (id, result) = self.engine.held_ready.remove(0);
                self.controller_ready(id, result);
            }
            Held::Wake => {
                let id = self.engine.held_wakes.remove(0);
                self.wake_now(id);
            }
            Held::Spare => {
                self.engine.held_spare = false;
                self.make_spare();
            }
            Held::Wait => {}
            Held::Done => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_HELD);
                }
                self.engine.held_since = None;
            }
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
            web::push(Event::ControllerReady(id, web::generation(), Ok(controller)));
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
            Response::Restart => self.restart_engine(),
        }
        self.invalidate();
    }

    /// The browser process of the engine in use ended. `failed`: it
    /// crashed or was killed, and the views went with it. A normal end
    /// comes when no view needs the process; the next view starts it again.
    pub(super) fn browser_exited(&mut self, failed: bool) {
        if failed {
            self.restart_engine();
            self.invalidate();
        }
    }

    /// An answer or a report of an engine that is no longer the one in use
    /// (it died, or it was let go). A view it made is closed: nobody waits
    /// for it, and the tab that asked has asked the new engine since.
    pub(super) fn stale_engine_event(&mut self, event: Event) {
        crate::diag::trace(|| format!("old engine {}: {}", event.generation().unwrap_or(0), event.name()));
        match event {
            Event::ControllerReady(_, _, Ok(controller)) => unsafe {
                let _ = controller.Close();
            },
            Event::EnvironmentReady(..) => {
                // Let go of an environment nobody asked for any more.
            }
            _ => {}
        }
    }

    /// The engine started, or did not.
    pub(super) fn environment_ready(&mut self, result: windows::core::Result<ICoreWebView2Environment>) {
        match result {
            Ok(env) => {
                self.engine.env_failures = 0;
                self.engine.env_retry_at = None;
                self.env_error = None;
                web::watch_browser(&env, web::generation());
                self.env = Some(env);
                self.show_active();
                self.warm_environment_ready();
            }
            Err(err) => self.environment_failed(format!(
                "Cannot start WebView2 ({err}). Install the WebView2 Runtime. Nea tries again by itself."
            )),
        }
    }

    /// The engine did not start: the message is shown, and it is asked for
    /// again later (a runtime that is being updated comes back by itself).
    pub(super) fn environment_failed(&mut self, message: String) {
        crate::diag::log(crate::diag::Log::Crash, format!("{} engine: {message}", crate::diag::stamp()));
        self.env_error = Some(message);
        self.env_requested = false;
        self.engine.env_failures = self.engine.env_failures.saturating_add(1);
        let delay = env_retry_delay(self.engine.env_failures);
        self.engine.env_retry_at = Some(Instant::now() + delay);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ENV_RETRY, delay.as_millis() as u32, Some(on_env_retry_timer));
        }
        self.invalidate();
    }

    /// Whether the engine may be asked for now (not too soon after a failure).
    pub(super) fn environment_allowed(&self) -> bool {
        self.engine.env_retry_at.is_none_or(|at| Instant::now() >= at)
    }

    /// The wait after a failed start is over: the tabs on screen ask again.
    fn retry_environment(&mut self) {
        self.engine.env_retry_at = None;
        if self.env.is_none() && !self.env_requested {
            self.show_active();
        }
    }

    /// The answer to a view request for tab `id` (not the spare's): the
    /// view to wire, if the tab still waits for it.
    pub(super) fn view_arrived(
        &mut self,
        id: u64,
        result: windows::core::Result<ICoreWebView2Controller>,
    ) -> Option<ICoreWebView2Controller> {
        let waiting = self.tabs.get(id).is_some_and(|t| matches!(t.live, Live::Creating));
        let on_screen = self.on_screen(id);
        let reloads = &mut self.engine.page_reloads;
        let outcome = arrival(waiting, result.is_ok(), on_screen, || reload_after_crash(reloads, id, Instant::now()));
        match (outcome, result) {
            (Arrival::Wire, Ok(controller)) => Some(controller),
            (Arrival::Unwanted, Ok(controller)) => {
                unsafe {
                    let _ = controller.Close();
                }
                None
            }
            (Arrival::Failed { retry }, result) => {
                if let Err(err) = result {
                    crate::diag::log(
                        crate::diag::Log::Crash,
                        format!("{} engine: no view for tab {id} ({err})", crate::diag::stamp()),
                    );
                }
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.live = Live::Asleep;
                    if tab.private {
                        self.close_tab(id);
                        return None;
                    }
                }
                if retry {
                    self.show_active();
                }
                None
            }
            (_, Err(_)) => None,
        }
    }

    /// The browser process died: every view went with it. Each view reports
    /// it, and the environment too; the first report does the work, and
    /// the new engine number makes the others old (see `web::generation`).
    /// The views being made go to sleep as well: their answers, from the
    /// engine that died, would never come or come as errors, and the tab
    /// waited for them for ever.
    fn restart_engine(&mut self) {
        web::next_generation();
        for tab in &mut self.tabs.tabs {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
            tab.loading = false;
        }
        if let Some(spare) = self.engine.spare.take() {
            unsafe {
                let _ = spare.Close();
            }
        }
        for (_, result) in self.engine.held_ready.drain(..) {
            if let Ok(controller) = result {
                unsafe {
                    let _ = controller.Close();
                }
            }
        }
        self.engine.making = false;
        self.engine.claim = None;
        self.engine.from_spare = None;
        self.env = None;
        if may_restart(&mut self.engine.restarts, Instant::now()) {
            self.env_requested = false;
            self.env_error = None;
            self.show_active();
        } else {
            // Not for good: an engine that a runtime update ends three
            // times starts again once the window has passed.
            let wait = restart_wait(&self.engine.restarts, Instant::now());
            crate::diag::log(
                crate::diag::Log::Crash,
                format!("{} engine: stopped too often, next start in {} s", crate::diag::stamp(), wait.as_secs()),
            );
            self.env_error = Some("The page engine stopped many times in a row. Nea starts it again soon.".to_string());
            self.env_requested = false;
            self.engine.env_retry_at = Some(Instant::now() + wait);
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ENV_RETRY, wait.as_millis() as u32, Some(on_env_retry_timer));
            }
            self.invalidate();
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/engine_ui.rs"]
#[rustfmt::skip]
mod tests;
