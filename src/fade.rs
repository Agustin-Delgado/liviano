//! Real fades for the app's own top-level popups (the field, the hover
//! card, the password bar, the split hint, Ajustes): the window is layered
//! and its whole-window opacity moves, children included, so a native edit
//! fades with the panel it sits in. A fade can also carry a short slide (a
//! toast's rise).
//!
//! A popup closing stops taking the pointer at once (clicks go through to
//! what is under it) and hides when its fade is over; shown again while it
//! fades out, it turns around from the opacity it has.

use std::{cell::RefCell, collections::HashMap, time::Instant};

use windows::Win32::{
    Foundation::{COLORREF, HWND, RECT},
    UI::{
        Input::KeyboardAndMouse::{GetActiveWindow, SetActiveWindow},
        WindowsAndMessaging::{
            GA_ROOTOWNER, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetLayeredWindowAttributes, GetWindow, GetWindowLongW,
            GetWindowRect, HWND_TOP, IsWindowVisible, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, SW_HIDE,
            SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowLongW, SetWindowPos,
            ShowWindow, WS_EX_LAYERED, WS_EX_TRANSPARENT,
        },
    },
};

use crate::anim::{self, Spec, Tween};

struct Fading {
    level: Tween<f32>,
    /// Opacity at level 1 (the split hint is translucent at rest).
    max: u8,
    /// Hide (and run `then`) when the level reaches 0.
    closing: bool,
    /// The extended style before the fade made the window click-through.
    ex_before: i32,
    /// Slide: the window's resting place and how far off it starts/ends.
    slide: Option<(i32, i32, i32)>,
    /// Remove WS_EX_LAYERED again once fully shown (a main window).
    unlayer: bool,
    then: Option<fn(HWND)>,
}

thread_local! {
    static FADING: RefCell<HashMap<isize, Fading>> = RefCell::new(HashMap::new());
}

fn ex_style(hwnd: HWND) -> i32 {
    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }
}

/// Makes a popup layered for good, fully opaque. Called once after it is
/// created; a layered window that never fades costs nothing extra.
pub fn prepare(hwnd: HWND, max: u8) {
    unsafe {
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style(hwnd) | WS_EX_LAYERED.0 as i32);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), max, LWA_ALPHA);
    }
}

fn set_alpha(hwnd: HWND, alpha: u8) {
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

fn current_alpha(hwnd: HWND) -> Option<u8> {
    let mut alpha = 0u8;
    let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
    unsafe { GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).ok()? };
    (flags.0 & LWA_ALPHA.0 != 0).then_some(alpha)
}

fn visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

fn origin(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.left, r.top)
}

/// Readies `hwnd` to appear: call it right before the code that shows it
/// (`ShowWindow`, `SWP_SHOWWINDOW`), with the window already in its place.
/// `rise` px: it comes up from that far below (negative: from above).
/// Already on screen and not leaving: nothing to do.
pub fn fade_in(hwnd: HWND, spec: Spec, max: u8, rise: i32) {
    let now = Instant::now();
    let shown = visible(hwnd);
    let was = FADING.with(|f| f.borrow_mut().remove(&(hwnd.0 as isize)));
    if shown && !was.as_ref().is_some_and(|w| w.closing) {
        if let Some(was) = was {
            // Still coming in: keep going.
            FADING.with(|f| f.borrow_mut().insert(hwnd.0 as isize, was));
        }
        return;
    }
    let mut ex_before = ex_style(hwnd);
    let mut slide = None;
    let level = match was {
        Some(was) => {
            // Turning around mid-exit: from the opacity it has, back in place.
            ex_before = was.ex_before;
            if let Some((x, y, dy)) = was.slide {
                slide = Some((x, y, dy));
            }
            was.level.value(now)
        }
        None => 0.0,
    };
    unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, ex_before | WS_EX_LAYERED.0 as i32) };
    if anim::reduced() {
        set_alpha(hwnd, max);
        restore_place(hwnd, slide);
        return;
    }
    if slide.is_none() && rise != 0 {
        let (x, y) = origin(hwnd);
        slide = Some((x, y, rise));
    }
    let fading = Fading {
        level: Tween::new(level, 1.0, now, spec),
        max,
        closing: false,
        ex_before,
        slide,
        unlayer: false,
        then: None,
    };
    apply(hwnd, &fading, now);
    FADING.with(|f| f.borrow_mut().insert(hwnd.0 as isize, fading));
    anim::drive(step);
}

/// Like `fade_in`, for a main window (Ajustes): layered only while it
/// fades, as any other window afterwards.
pub fn fade_in_window(hwnd: HWND, spec: Spec) {
    fade_in(hwnd, spec, 255, 0);
    FADING.with(|f| {
        if let Some(fading) = f.borrow_mut().get_mut(&(hwnd.0 as isize)) {
            fading.unlayer = true;
        }
    });
    if anim::reduced() {
        unlayer(hwnd);
    }
}

fn unlayer(hwnd: HWND) {
    unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, ex_style(hwnd) & !(WS_EX_LAYERED.0 as i32)) };
}

/// Hides `hwnd` with a fade instead of at once; `then` runs once it is
/// hidden (to destroy it, to let go of what it showed). `sink` px: it
/// drifts that far down while it goes.
pub fn fade_out(hwnd: HWND, spec: Spec, sink: i32, then: Option<fn(HWND)>) {
    let now = Instant::now();
    if !visible(hwnd) || anim::reduced() {
        FADING.with(|f| f.borrow_mut().remove(&(hwnd.0 as isize)));
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        if let Some(then) = then {
            then(hwnd);
        }
        return;
    }
    let was = FADING.with(|f| f.borrow_mut().remove(&(hwnd.0 as isize)));
    if was.as_ref().is_some_and(|w| w.closing) {
        FADING.with(|f| f.borrow_mut().insert(hwnd.0 as isize, was.unwrap()));
        return;
    }
    let ex_before = was.as_ref().map_or_else(|| ex_style(hwnd), |w| w.ex_before);
    let max = was.as_ref().map_or_else(|| current_alpha(hwnd).unwrap_or(255), |w| w.max);
    let level = was.as_ref().map_or(1.0, |w| w.level.value(now));
    let slide = match was.as_ref().and_then(|w| w.slide) {
        Some(s) => Some(s),
        None if sink != 0 => {
            let (x, y) = origin(hwnd);
            Some((x, y, sink))
        }
        None => None,
    };
    // Hand the keyboard back before it goes, as hiding it would.
    unsafe {
        if GetActiveWindow() == hwnd {
            let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_else(|_| GetAncestor(hwnd, GA_ROOTOWNER));
            if !owner.is_invalid() {
                let _ = SetActiveWindow(owner);
            }
        }
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex_before | (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) as i32);
    }
    let fading = Fading {
        level: Tween::new(level, 0.0, now, spec),
        max,
        closing: true,
        ex_before,
        slide,
        unlayer: false,
        then,
    };
    apply(hwnd, &fading, now);
    FADING.with(|f| f.borrow_mut().insert(hwnd.0 as isize, fading));
    anim::drive(step);
}

/// Whether `hwnd` is on its way out (still drawn, no longer there).
pub fn closing(hwnd: HWND) -> bool {
    FADING.with(|f| f.borrow().get(&(hwnd.0 as isize)).is_some_and(|w| w.closing))
}

fn restore_place(hwnd: HWND, slide: Option<(i32, i32, i32)>) {
    if let Some((x, y, _)) = slide {
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

fn apply(hwnd: HWND, fading: &Fading, now: Instant) {
    let level = fading.level.value(now);
    // The level may overshoot on a spring; opacity can't.
    set_alpha(hwnd, (level.clamp(0.0, 1.0) * fading.max as f32).round() as u8);
    if let Some((x, y, dy)) = fading.slide {
        let off = (dy as f32 * (1.0 - level)).round() as i32;
        unsafe {
            let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y + off, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// One frame for every fading popup; false once none is left.
fn step() -> bool {
    let now = Instant::now();
    let mut done: Vec<(HWND, Fading)> = Vec::new();
    FADING.with(|f| {
        let mut f = f.borrow_mut();
        let keys: Vec<isize> = f.keys().copied().collect();
        for key in keys {
            let hwnd = HWND(key as _);
            let fading = &f[&key];
            apply(hwnd, fading, now);
            if !fading.level.moving(now) {
                done.push((hwnd, f.remove(&key).expect("key listed")));
            }
        }
    });
    for (hwnd, fading) in done {
        if fading.closing {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                SetWindowLongW(hwnd, GWL_EXSTYLE, fading.ex_before | WS_EX_LAYERED.0 as i32);
            }
            set_alpha(hwnd, fading.max);
            restore_place(hwnd, fading.slide);
            if let Some(then) = fading.then {
                then(hwnd);
            }
        } else {
            set_alpha(hwnd, fading.max);
            restore_place(hwnd, fading.slide);
            if fading.unlayer {
                unlayer(hwnd);
            }
        }
    }
    FADING.with(|f| !f.borrow().is_empty())
}

/// For the script control: what is fading and how far along.
pub fn snapshot() -> Vec<(isize, f32, bool)> {
    let now = Instant::now();
    FADING.with(|f| f.borrow().iter().map(|(k, w)| (*k, w.level.value(now), w.closing)).collect())
}
