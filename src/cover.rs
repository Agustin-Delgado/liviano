//! Tells whether a person can see some part of a window: a window of a
//! different program can be on top of all of it, or the window can be on a
//! different virtual desktop.
//!
//! The engine draws a page for as long as its view is shown. It does not
//! know that a different window covers the view. Thus a covered window
//! with an animation in its page uses the processor for nothing. The
//! window asks here, and hides its views while the answer is yes (refer to
//! `app::cover_ui`).
//!
//! Only one window that contains all of this one counts. Two windows that
//! cover it together do not: the result is then that the window is in
//! sight, and the cost is only that the page continues to draw.

use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn, RGN_ERROR},
    },
    UI::WindowsAndMessaging::{
        GW_HWNDPREV, GWL_EXSTYLE, GetWindow, GetWindowLongW, IsIconic, IsWindowVisible, IsZoomed, WS_EX_LAYERED,
        WS_EX_TRANSPARENT,
    },
};

/// The radius of the round corners of a window in Windows 11, at 96 dpi.
const CORNER_RADIUS: i32 = 8;
/// No more windows than this are examined: a loop in the list of windows
/// must not hold the window.
const MAX_WINDOWS: usize = 400;

/// A window that is higher in the order of windows.
#[derive(Clone, Copy, Debug)]
pub struct Above {
    pub rect: RECT,
    /// Its corners are not round: it is maximized.
    pub square: bool,
}

/// Whether `above` hides all of `own`. `corner`: the radius of a round
/// corner in pixels. A round corner of `above` shows a small part of what
/// is below it, but not when the corner of `own` is at the same point and
/// is round too.
pub fn hides(own: RECT, own_square: bool, above: Above, corner: i32) -> bool {
    let a = above.rect;
    if a.left > own.left || a.top > own.top || a.right < own.right || a.bottom < own.bottom {
        return false;
    }
    if above.square {
        return true;
    }
    let corners = [
        (own.left - a.left, own.top - a.top),
        (a.right - own.right, own.top - a.top),
        (own.left - a.left, a.bottom - own.bottom),
        (a.right - own.right, a.bottom - own.bottom),
    ];
    corners.iter().all(|&(dx, dy)| dx >= corner || dy >= corner || (dx == 0 && dy == 0 && !own_square))
}

fn frame_bounds(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
    }
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// Windows keeps the window out of sight: it is on a different virtual
/// desktop, or its program is stopped.
fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut value as *mut u32 as *mut _, 4).is_ok() && value != 0 }
}

/// Whether the pixels of the window hide what is below it. A layered
/// window can be transparent, and a window with a region has holes.
fn opaque(hwnd: HWND) -> bool {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) != 0 {
            return false;
        }
        let region = CreateRectRgn(0, 0, 0, 0);
        let has_region = GetWindowRgn(hwnd, region) != RGN_ERROR;
        let _ = DeleteObject(region.into());
        !has_region
    }
}

/// Whether nobody can see `hwnd`: one window on top of it hides all of
/// it, or it is on a different virtual desktop. `dpi`: of `hwnd`.
pub fn out_of_sight(hwnd: HWND, dpi: u32) -> bool {
    if cloaked(hwnd) {
        return true;
    }
    let Some(own) = frame_bounds(hwnd) else { return false };
    let own_square = unsafe { IsZoomed(hwnd) }.as_bool();
    let corner = (CORNER_RADIUS as u32 * dpi.max(96)).div_ceil(96) as i32;
    let mut at = hwnd;
    for _ in 0..MAX_WINDOWS {
        let Ok(next) = (unsafe { GetWindow(at, GW_HWNDPREV) }) else { return false };
        at = next;
        unsafe {
            if !IsWindowVisible(at).as_bool() || IsIconic(at).as_bool() {
                continue;
            }
        }
        let Some(rect) = frame_bounds(at) else { continue };
        let above = Above { rect, square: unsafe { IsZoomed(at) }.as_bool() };
        if hides(own, own_square, above, corner) && !cloaked(at) && opaque(at) {
            return true;
        }
    }
    false
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/cover.rs"]
#[rustfmt::skip]
mod tests;
