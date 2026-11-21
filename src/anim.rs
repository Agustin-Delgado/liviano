//! The interface's motion: CSS timing curves evaluated exactly as a browser
//! does, values that ease from one place to another and pick up from
//! wherever they are when the target changes, and one frame clock for the
//! whole UI thread.
//!
//! The curves and durations come from a web design system and its
//! component library (`docs/design.md` lists each one), so a popover, a
//! menu or a toast moves here as it moves on the web.
//!
//! The clock is a single thread timer that exists only while something
//! asked for a frame: every request is for the *next* frame, and a window
//! that is still moving asks again while it paints. With nothing moving the
//! timer is killed, so a still window costs no CPU at all. `DwmFlush` was
//! left out on purpose: it blocks the thread until the compositor's next
//! frame, and on the UI thread that is input waiting behind decoration.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    hash::Hash,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::InvalidateRect,
    UI::WindowsAndMessaging::{KillTimer, SetTimer},
};

use crate::{
    design::Rgb,
    gfx::{Color, RectF},
};

// ---------- curves ----------

/// A CSS `cubic-bezier(x1, y1, x2, y2)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn sample(a1: f32, a2: f32, t: f32) -> f32 {
        // B(t) for P0 = 0, P3 = 1, in Horner form.
        let (c, b, a) = (3.0 * a1, 3.0 * (a2 - a1) - 3.0 * a1, 1.0 - 3.0 * a2 + 3.0 * a1);
        ((a * t + b) * t + c) * t
    }

    fn slope(a1: f32, a2: f32, t: f32) -> f32 {
        let (c, b, a) = (3.0 * a1, 3.0 * (a2 - a1) - 3.0 * a1, 1.0 - 3.0 * a2 + 3.0 * a1);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    /// The curve's output at time `x` (0..1): solve x(t) = x for t, then
    /// read y(t). Newton first (fast where the slope is healthy), bisection
    /// where it isn't, as browsers do.
    pub fn at(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        let mut t = x;
        for _ in 0..8 {
            let err = Self::sample(self.x1, self.x2, t) - x;
            if err.abs() < 1e-6 {
                return Self::sample(self.y1, self.y2, t);
            }
            let d = Self::slope(self.x1, self.x2, t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        t = x;
        for _ in 0..40 {
            let v = Self::sample(self.x1, self.x2, t);
            if (v - x).abs() < 1e-6 {
                break;
            }
            if v < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) / 2.0;
        }
        Self::sample(self.y1, self.y2, t)
    }
}

/// How a motion spends its time.
#[allow(dead_code)] // the whole catalogue, used or not yet
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Linear,
    Bezier(CubicBezier),
    /// svelte/easing `quintOut`: 1 − (1 − t)⁵.
    QuintOut,
    /// svelte/easing `cubicOut`: 1 − (1 − t)³.
    CubicOut,
    /// A CSS `linear(...)` function: (output, input) stops, inputs rising
    /// from 0 to 1. Outputs may pass 1 (a spring's overshoot).
    Stops(&'static [(f32, f32)]),
}

impl Curve {
    pub fn at(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Curve::Linear => t,
            Curve::Bezier(b) => b.at(t),
            Curve::QuintOut => 1.0 - (1.0 - t).powi(5),
            Curve::CubicOut => 1.0 - (1.0 - t).powi(3),
            Curve::Stops(stops) => {
                let Some(&(first, _)) = stops.first() else { return t };
                let mut previous = (first, 0.0);
                for &(value, at) in stops.iter().skip(1) {
                    if t <= at {
                        let span = at - previous.1;
                        let k = if span <= 0.0 { 1.0 } else { (t - previous.1) / span };
                        return previous.0 + (value - previous.0) * k;
                    }
                    previous = (value, at);
                }
                previous.0
            }
        }
    }
}

/// The named curves. Sources in `docs/design.md`.
#[allow(dead_code)] // a catalogue: not every curve has a user yet
pub mod curves {
    use super::{CubicBezier, Curve};

    /// CSS `ease`: what tw-animate's enter/exit run on when no ease is set
    /// (popover, menu, dialog, tooltip).
    pub const EASE: Curve = Curve::Bezier(CubicBezier::new(0.25, 0.1, 0.25, 1.0));
    /// Tailwind v4 `ease-out`: hovers, colours, collapsibles, the tabs' indicator.
    pub const EASE_OUT: Curve = Curve::Bezier(CubicBezier::new(0.0, 0.0, 0.2, 1.0));
    /// Tailwind's default transition curve (a bare `transition`).
    pub const STANDARD: Curve = Curve::Bezier(CubicBezier::new(0.4, 0.0, 0.2, 1.0));
    /// brand.css `--ease-move`: something travelling to a new place.
    pub const MOVE: Curve = Curve::Bezier(CubicBezier::new(0.77, 0.0, 0.175, 1.0));
    /// brand.css `--ease-out-strong`: something answering a press.
    pub const OUT_STRONG: Curve = Curve::Bezier(CubicBezier::new(0.23, 1.0, 0.32, 1.0));
    /// The switch's thumb.
    pub const THUMB: Curve = Curve::Bezier(CubicBezier::new(0.32, 0.72, 0.0, 1.0));
    pub const QUINT_OUT: Curve = Curve::QuintOut;
    pub const CUBIC_OUT: Curve = Curve::CubicOut;
    /// toast/styles.css `--toast-spring-easing`: a spring that overshoots
    /// to 1.038 at 45 % and settles.
    pub const SPRING: Curve = Curve::Stops(&[
        (0.0, 0.0),
        (0.002, 0.006),
        (0.007, 0.012),
        (0.015, 0.018),
        (0.026, 0.024),
        (0.041, 0.031),
        (0.06, 0.038),
        (0.108, 0.053),
        (0.157, 0.066),
        (0.214, 0.08),
        (0.467, 0.137),
        (0.577, 0.163),
        (0.631, 0.177),
        (0.682, 0.191),
        (0.73, 0.205),
        (0.771, 0.218),
        (0.808, 0.231),
        (0.844, 0.245),
        (0.874, 0.258),
        (0.903, 0.272),
        (0.928, 0.286),
        (0.952, 0.301),
        (0.972, 0.316),
        (0.988, 0.331),
        (1.01, 0.357),
        (1.025, 0.385),
        (1.034, 0.416),
        (1.038, 0.45),
        (1.035, 0.501),
        (1.012, 0.642),
        (1.003, 0.73),
        (0.999, 0.837),
        (1.0, 1.0),
    ]);
}

/// A duration and its curve: one movement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub ms: u32,
    pub curve: Curve,
}

impl Spec {
    pub const fn new(ms: u32, curve: Curve) -> Self {
        Self { ms, curve }
    }
}

/// Every movement the interface makes, named for what moves. Sources in
/// `docs/design.md`.
#[allow(dead_code)] // a catalogue: menus are still the system's
pub mod specs {
    use super::{Spec, curves::*};

    /// A hover or a press colouring a control (Button: 150 ms ease-out).
    pub const HOVER: Spec = Spec::new(150, EASE_OUT);
    /// A popover or a combobox list opening and closing (100 ms, fade and 0.98).
    pub const POPOVER_IN: Spec = Spec::new(100, EASE);
    pub const POPOVER_OUT: Spec = Spec::new(100, EASE);
    /// The field arriving as a macOS panel does (Spotlight): it grows from
    /// `FIELD_FROM_SCALE` on the spring, most of the way in the first
    /// 100 ms and then settling, while its opacity is there sooner.
    pub const FIELD_GROW: Spec = Spec::new(320, SPRING);
    pub const FIELD_FADE_IN: Spec = Spec::new(140, EASE_OUT);
    /// The field leaving: it shrinks and fades together, quicker than it came.
    pub const FIELD_OUT: Spec = Spec::new(150, EASE);
    /// A menu: 120 ms in, 100 ms out.
    pub const MENU_IN: Spec = Spec::new(120, EASE);
    pub const MENU_OUT: Spec = Spec::new(100, EASE);
    /// A dialog: 100 ms each way, fade and 0.95.
    pub const DIALOG: Spec = Spec::new(100, EASE);
    /// A tooltip: fade only, 100 ms in, 80 ms out.
    pub const TOOLTIP_IN: Spec = Spec::new(100, EASE);
    pub const TOOLTIP_OUT: Spec = Spec::new(80, EASE);
    /// A toast rising in (opacity and transform: 0.66 of its 600 ms on the
    /// spring) and leaving (400 ms).
    pub const TOAST_IN: Spec = Spec::new(396, SPRING);
    pub const TOAST_OUT: Spec = Spec::new(400, EASE);
    /// The tabs' indicator sliding to the chosen tab.
    pub const INDICATOR: Spec = Spec::new(200, EASE_OUT);
    /// The live tab's ground moving to the chosen tab: shorter than the
    /// indicator and front-loaded, so it is already there when the eye lands
    /// on it (200 ms ease-out read as slow across a whole strip).
    pub const LIVE_CARD: Spec = Spec::new(140, OUT_STRONG);
    /// Neighbours making room (follow-shift).
    pub const SHIFT: Spec = Spec::new(160, OUT_STRONG);
    /// A box changing its size (morph).
    pub const MORPH: Spec = Spec::new(200, OUT_STRONG);
    /// A tab group folding or unfolding: the morph's curve, which covers
    /// most of the way in the first frames (a press answered at once) and
    /// settles softly, a little longer so the rows' fade reads.
    pub const COLLAPSE: Spec = Spec::new(220, OUT_STRONG);
    /// The rail sliding in from its edge (the app's drawer).
    pub const RAIL: Spec = Spec::new(250, MOVE);
    /// A check box filling in.
    pub const CHECK: Spec = Spec::new(75, EASE_OUT);
}

/// Pixels a toast travels while it fades.
pub const TOAST_RISE: f32 = 6.0;
/// How far the field travels as it comes and goes, toward the edge it
/// hangs from: the design system's popovers grow 2 % from that edge, about this much
/// on the field's height.
pub const POPOVER_SLIDE: f32 = 4.0;
/// The size the field grows from and shrinks to (macOS panels: about 0.96).
pub const FIELD_FROM_SCALE: f32 = 0.96;
/// Tooltip timing: the pointer rests this long before one opens, it closes
/// this long after the pointer leaves, and within the skip window after one
/// closed the next opens at once.
pub const TOOLTIP_DELAY_MS: u64 = 600;
pub const TOOLTIP_CLOSE_MS: u64 = 100;
pub const TOOLTIP_SKIP_MS: u64 = 300;

// ---------- reduced motion ----------

thread_local! {
    static REDUCED: Cell<bool> = const { Cell::new(false) };
}

/// Windows' "Animation effects" switch off: every movement lands at once
/// (the app's prefers-reduced-motion). Read again on WM_SETTINGCHANGE.
pub fn refresh_reduced() {
    REDUCED.with(|r| r.set(!crate::design::animations_enabled()));
}

pub fn reduced() -> bool {
    REDUCED.with(|r| r.get())
}

#[cfg(all(test, nea_tests))]
pub fn set_reduced(on: bool) {
    REDUCED.with(|r| r.set(on));
}

// ---------- values that move ----------

/// Anything with an in-between.
pub trait Lerp: Copy + PartialEq {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for RectF {
    fn lerp(a: RectF, b: RectF, t: f32) -> RectF {
        a.lerp(&b, t)
    }
}

impl Lerp for Color {
    fn lerp(a: Color, b: Color, t: f32) -> Color {
        let m = |x: f32, y: f32| x + (y - x) * t;
        Color { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }
    }
}

impl Lerp for Rgb {
    /// Outside 0..1 (a spring's overshoot) the colour holds at its end.
    fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
        a.mix(b, t)
    }
}

/// A value easing toward a target. Retargeted while it moves, it starts
/// again from where it is, so an interrupted movement never jumps.
#[derive(Clone, Copy, Debug)]
pub struct Tween<T: Lerp> {
    from: T,
    to: T,
    start: Instant,
    duration: Duration,
    curve: Curve,
}

impl<T: Lerp> Tween<T> {
    /// At rest on `value`.
    pub fn rest(value: T) -> Self {
        Self { from: value, to: value, start: Instant::now(), duration: Duration::ZERO, curve: Curve::Linear }
    }

    /// From `from` to `to`, starting `now`.
    pub fn new(from: T, to: T, now: Instant, spec: Spec) -> Self {
        let mut t = Self::rest(from);
        t.restart(from, to, now, spec);
        t
    }

    fn restart(&mut self, from: T, to: T, now: Instant, spec: Spec) {
        self.from = from;
        self.to = to;
        self.start = now;
        self.duration = if reduced() { Duration::ZERO } else { Duration::from_millis(spec.ms as u64) };
        self.curve = spec.curve;
    }

    /// Heads to `target` from wherever the value is at `now`.
    pub fn to(&mut self, target: T, now: Instant, spec: Spec) {
        if target == self.to {
            return;
        }
        let here = self.value(now);
        self.restart(here, target, now, spec);
    }

    pub fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub fn value(&self, now: Instant) -> T {
        let p = self.progress(now);
        if p >= 1.0 {
            return self.to;
        }
        T::lerp(self.from, self.to, self.curve.at(p))
    }

    pub fn target(&self) -> T {
        self.to
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.progress(now) < 1.0
    }
}

/// A set of 0..1 levels keyed by what they belong to (hovers, presses, a
/// list's picked row). A level heading back takes the share of the
/// duration it has left to cover, as a reversed CSS transition does.
#[derive(Debug)]
pub struct Levels<K> {
    map: HashMap<K, Tween<f32>>,
}

impl<K> Default for Levels<K> {
    fn default() -> Self {
        Self { map: HashMap::new() }
    }
}

impl<K: Eq + Hash + Copy> Levels<K> {
    pub fn set(&mut self, key: K, target: f32, now: Instant, spec: Spec) {
        let tween = self.map.entry(key).or_insert_with(|| Tween::rest(0.0));
        if tween.target() == target {
            return;
        }
        let here = tween.value(now);
        let share = (target - here).abs().clamp(0.0, 1.0);
        let spec = Spec { ms: (spec.ms as f32 * share).round() as u32, ..spec };
        tween.to(target, now, spec);
    }

    /// Jumps to 1 and heads back to 0: a press answered.
    pub fn pulse(&mut self, key: K, now: Instant, spec: Spec) {
        self.map.insert(key, Tween::new(1.0, 0.0, now, spec));
    }

    /// Sets every key not in `keep` heading to 0.
    pub fn release_except(&mut self, keep: &[K], now: Instant, spec: Spec) {
        let others: Vec<K> = self.map.keys().filter(|k| !keep.contains(k)).copied().collect();
        for key in others {
            self.set(key, 0.0, now, spec);
        }
    }

    pub fn get(&self, key: K, now: Instant) -> f32 {
        self.map.get(&key).map_or(0.0, |t| t.value(now))
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.map.values().any(|t| t.moving(now))
    }

    /// Forgets what is at rest on 0.
    pub fn prune(&mut self, now: Instant) {
        self.map.retain(|_, t| t.moving(now) || t.target() != 0.0);
    }

    #[cfg(all(test, nea_tests))]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

// ---------- the frame clock ----------

/// About 60 frames a second. 15 rather than 16: the system timer ticks
/// every 15.6 ms, and asking for 16 waits for the second tick.
pub const FRAME_MS: u32 = 15;

#[derive(Clone, Copy)]
enum Job {
    Paint(isize, Option<RECT>),
    Drive(fn() -> bool),
}

struct Entry {
    job: Job,
    due: Instant,
}

#[derive(Default)]
struct Clock {
    entries: Vec<Entry>,
    timer: usize,
    timer_ms: u32,
    /// The system timer is at 1 ms while something moves (see `fine_timer`).
    fine: bool,
    stats: Stats,
}

/// While something moves, Windows' timer runs at 1 ms instead of its usual
/// 15.6: a 15 ms `SetTimer` otherwise fires on the 15.6 ms tick or, when it
/// just misses one, on the next at 31.2, and measured frames came 20 ms
/// apart on average with every other one doubled, which reads as a stutter.
/// Since Windows 10 2004 the resolution is the process's own, and it goes
/// back the moment the clock stops, so a still window costs nothing.
/// winmm is loaded the first time something moves, never at startup.
fn fine_timer(on: bool) {
    type Period = unsafe extern "system" fn(u32) -> u32;
    thread_local! {
        static WINMM: Option<(crate::system_dll::Library, Period, Period)> = {
            let library = crate::system_dll::Library::load(windows::core::w!("winmm.dll"));
            library.and_then(|l| unsafe {
                let begin = l.function::<Period>(c"timeBeginPeriod")?;
                let end = l.function::<Period>(c"timeEndPeriod")?;
                Some((l, begin, end))
            })
        };
    }
    WINMM.with(|w| {
        if let Some((_, begin, end)) = w {
            unsafe {
                if on {
                    begin(1)
                } else {
                    end(1)
                };
            }
        }
    });
}

/// What the clock has done, for the script control.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub ticks: u64,
    pub running: bool,
    /// The longest a frame's work took (painting included, as reported by
    /// the windows that paint), in microseconds.
    pub worst_frame_us: u64,
    pub frames_over_16ms: u64,
    /// Frames reported and their summed time, for the mean.
    pub frames: u64,
    pub total_frame_us: u64,
    /// Time between one frame of a moving window and its next (what the
    /// eye sees as smooth or not): the longest, and the sum and count for
    /// the mean. Gaps past `GAP_RESET` are the start of another motion.
    pub worst_gap_us: u64,
    pub gaps: u64,
    pub total_gap_us: u64,
    /// The longest gap of the last motion that came to rest.
    pub last_motion_worst_gap_us: u64,
    motion_worst_gap_us: u64,
    last_shown: Option<Instant>,
}

/// A pause this long between two frames is two motions, not one stutter.
const GAP_RESET: Duration = Duration::from_millis(120);

/// A moving window put a frame on screen: the gap since its previous one
/// is counted.
pub fn frame_shown(now: Instant) {
    CLOCK.with(|c| {
        let mut c = c.borrow_mut();
        let s = &mut c.stats;
        if let Some(gap) = s.last_shown.map(|l| now.saturating_duration_since(l)).filter(|g| *g < GAP_RESET) {
            let us = gap.as_micros() as u64;
            s.worst_gap_us = s.worst_gap_us.max(us);
            s.motion_worst_gap_us = s.motion_worst_gap_us.max(us);
            s.gaps += 1;
            s.total_gap_us += us;
        }
        s.last_shown = Some(now);
    });
}

/// A motion ended: the next frame starts a new count of gaps.
pub fn motion_rested() {
    CLOCK.with(|c| {
        let s = &mut c.borrow_mut().stats;
        s.last_shown = None;
        s.last_motion_worst_gap_us = std::mem::take(&mut s.motion_worst_gap_us);
    });
}

thread_local! {
    static CLOCK: RefCell<Clock> = RefCell::new(Clock::default());
}

fn union(a: Option<RECT>, b: Option<RECT>) -> Option<RECT> {
    match (a, b) {
        (Some(a), Some(b)) => Some(RECT {
            left: a.left.min(b.left),
            top: a.top.min(b.top),
            right: a.right.max(b.right),
            bottom: a.bottom.max(b.bottom),
        }),
        _ => None,
    }
}

fn schedule(job: Job, after_ms: u32) {
    let due = Instant::now() + Duration::from_millis(after_ms.saturating_sub(FRAME_MS) as u64);
    CLOCK.with(|c| {
        let mut c = c.borrow_mut();
        let found = c.entries.iter_mut().find(|e| match (e.job, job) {
            (Job::Paint(a, _), Job::Paint(b, _)) => a == b,
            (Job::Drive(a), Job::Drive(b)) => std::ptr::fn_addr_eq(a, b),
            _ => false,
        });
        match found {
            Some(entry) => {
                if let (Job::Paint(h, a), Job::Paint(_, b)) = (entry.job, job) {
                    entry.job = Job::Paint(h, union(a, b));
                }
                entry.due = entry.due.min(due);
            }
            None => c.entries.push(Entry { job, due }),
        }
        arm(&mut c);
    });
}

fn arm(c: &mut Clock) {
    if c.entries.is_empty() {
        if c.timer != 0 {
            unsafe {
                let _ = KillTimer(None, c.timer);
            }
            c.timer = 0;
            c.timer_ms = 0;
        }
        if c.fine {
            fine_timer(false);
            c.fine = false;
        }
        c.stats.running = false;
        return;
    }
    // A slow request (the loading ring) lets the timer tick slower.
    let now = Instant::now();
    let soonest = c.entries.iter().map(|e| e.due.saturating_duration_since(now)).min().unwrap_or_default();
    let ms = (soonest.as_millis() as u32).max(FRAME_MS);
    let ms = if ms <= FRAME_MS + 2 { FRAME_MS } else { ms };
    // Only a frame-rate request needs the fine timer; the loading ring's
    // slow ticks are fine on the system's own.
    let fine = ms == FRAME_MS;
    if fine != c.fine {
        fine_timer(fine);
        c.fine = fine;
    }
    if c.timer == 0 || c.timer_ms != ms {
        c.timer = unsafe { SetTimer(None, c.timer, ms, Some(tick)) };
        c.timer_ms = ms;
    }
    c.stats.running = c.timer != 0;
}

unsafe extern "system" fn tick(_: HWND, _: u32, _: usize, _: u32) {
    let started = Instant::now();
    let due: Vec<Job> = CLOCK.with(|c| {
        let mut c = c.borrow_mut();
        c.stats.ticks += 1;
        let mut due = Vec::new();
        c.entries.retain(|e| {
            if e.due <= started + Duration::from_millis(2) {
                due.push(e.job);
                false
            } else {
                true
            }
        });
        due
    });
    for job in due {
        match job {
            Job::Paint(hwnd, rect) => unsafe {
                let _ = InvalidateRect(Some(HWND(hwnd as _)), rect.as_ref().map(|r| r as *const RECT), false);
            },
            Job::Drive(f) => {
                if f() {
                    schedule(job, FRAME_MS);
                }
            }
        }
    }
    report_frame(started.elapsed());
    CLOCK.with(|c| arm(&mut c.borrow_mut()));
}

/// Repaints `hwnd` (or `rect` of it) on the next frame. A window that is
/// still moving calls this again while it paints.
pub fn repaint(hwnd: HWND, rect: Option<RECT>) {
    schedule(Job::Paint(hwnd.0 as isize, rect), FRAME_MS);
}

/// Repaints after `ms` rather than on the next frame (the loading ring).
pub fn repaint_after(hwnd: HWND, rect: Option<RECT>, ms: u32) {
    schedule(Job::Paint(hwnd.0 as isize, rect), ms);
}

/// Calls `f` every frame for as long as it returns true.
pub fn drive(f: fn() -> bool) {
    schedule(Job::Drive(f), FRAME_MS);
}

/// A window says how long a frame's paint took.
pub fn report_frame(took: Duration) {
    CLOCK.with(|c| {
        let mut c = c.borrow_mut();
        let us = took.as_micros() as u64;
        c.stats.worst_frame_us = c.stats.worst_frame_us.max(us);
        c.stats.frames += 1;
        c.stats.total_frame_us += us;
        if us > 16_000 {
            c.stats.frames_over_16ms += 1;
        }
    });
}

pub fn stats() -> Stats {
    CLOCK.with(|c| c.borrow().stats)
}

// ---------- frame marks ----------

/// The most marks kept between two reads; older ones are dropped.
const MARKS_CAP: usize = 8192;

/// Marks are kept only while script control is on.
static MARKING: AtomicBool = AtomicBool::new(false);

thread_local! {
    static MARKS: RefCell<VecDeque<(&'static str, u64)>> = const { RefCell::new(VecDeque::new()) };
}

/// Script control turned on or off: marks start or stop being kept.
pub fn keep_marks(on: bool) {
    MARKING.store(on, Ordering::Relaxed);
    if !on {
        MARKS.with(|m| *m.borrow_mut() = VecDeque::new());
    }
}

/// `what` put a frame out now (a window painted a frame of its motion, a
/// fade set its opacity). Script control reads the marks to measure the
/// frame rate and the pacing of each animation.
pub fn mark(what: &'static str) {
    if MARKING.load(Ordering::Relaxed) {
        push_mark(what, crate::diag::now_us());
    }
}

fn push_mark(what: &'static str, us: u64) {
    MARKS.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() >= MARKS_CAP {
            m.pop_front();
        }
        m.push_back((what, us));
    });
}

/// The marks since the last call, oldest first (microseconds from start).
pub fn take_marks() -> Vec<(&'static str, u64)> {
    MARKS.with(|m| std::mem::take(&mut *m.borrow_mut()).into())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/anim.rs"]
#[rustfmt::skip]
mod tests;
