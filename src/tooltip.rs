//! The tooltip: a small pill in ink with the ground's colour for text,
//! caption size, as the design system draws its tooltips. It never takes the pointer or
//! the keyboard. The window is layered per pixel, so the pill's round ends
//! and its soft shadow are real, and it fades in (100 ms) and out (80 ms)
//! by its whole-window opacity.
//!
//! When it opens is the caller's business (`app/tooltip_ui.rs` keeps the
//! delays); this only draws, places and fades.

use std::{cell::RefCell, time::Instant};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ,
            MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

use crate::{
    anim::{self, Spec, Tween, specs},
    design::{Palette, size},
    gfx::{self, Align, Color, Font, RectF, Surface},
    tabbar::Rect,
};

/// Room around the pill for its shadow (0 4px 10px -2px).
const SHADOW: f32 = 6.0;

/// A per-pixel layered popup's picture: a bitmap and where it goes. The
/// space card draws the same way.
pub(crate) struct Drawn {
    pub hwnd: HWND,
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pub size: SIZE,
    pub origin: POINT,
}

struct State {
    drawn: Option<Drawn>,
    level: Tween<f32>,
    surface: Surface,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { drawn: None, level: Tween::rest(0.0), surface: Surface::default() });
}

pub struct Tooltip {
    pub hwnd: HWND,
}

impl Tooltip {
    pub fn create(owner: HWND) -> windows::core::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: w!("NeaTooltip"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("NeaTooltip"),
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            Ok(Self { hwnd })
        }
    }

    /// Shows `text` next to `anchor` (screen pixels): above it when there is
    /// room inside `inside` (the owner's client area), below otherwise, as
    /// a popover flips. Already up: redrawn there at once, no new fade.
    pub fn show(&self, text: &str, anchor: Rect, inside: Rect, palette: &Palette, scale: f32) {
        let font = Font::new(size::CAPTION * scale, size::MEDIUM, Align::Center);
        let pad = (size::TOOLTIP_PAD_X * scale, size::TOOLTIP_PAD_Y * scale);
        let pill_w = (gfx::measure(text, font) + 2.0 * pad.0).ceil();
        let pill_h = (16.0 * scale + 2.0 * pad.1).round();
        let margin = (SHADOW * scale).ceil();
        let (w, h) = ((pill_w + 2.0 * margin) as i32, (pill_h + 2.0 * margin) as i32);
        let offset = (size::OFFSET * scale).round() as i32;
        let cx = (anchor.left + anchor.right) / 2;
        let above = anchor.top - offset - pill_h as i32;
        let pill_top = if above >= inside.top { above } else { anchor.bottom + offset };
        let work = work_area(anchor);
        let left = (cx - pill_w as i32 / 2).clamp(work.left, (work.right - pill_w as i32).max(work.left));
        let origin = POINT { x: left - margin as i32, y: pill_top - margin as i32 };

        let pill = RectF::new(margin, margin, margin + pill_w, margin + pill_h);
        let ink = Color::from(palette.tooltip_ink);
        let ground = Color::from(palette.tooltip);
        let dark = palette.dark;
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let shown = unsafe { IsWindowVisible(self.hwnd).as_bool() };
            release(&mut s.drawn);
            let Some(drawn) = new_bitmap(self.hwnd, SIZE { cx: w, cy: h }, origin) else { return };
            let full = RECT { left: 0, top: 0, right: w, bottom: h };
            s.surface.paint(drawn.dc, full, |g| {
                g.clear(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 });
                // The drop, a few fainter layers 4 px below.
                let shade = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
                for (grow, alpha) in [(0.0, 0.09), (1.5, 0.05), (3.0, 0.03)] {
                    let g2 = grow * scale;
                    let drop = RectF::new(
                        pill.left - g2 + 2.0 * scale,
                        pill.top + 3.0 * scale,
                        pill.right + g2 - 2.0 * scale,
                        pill.bottom + (2.0 + grow) * scale,
                    );
                    g.round(drop, drop.height() / 2.0, shade.with_alpha(if dark { alpha * 2.0 } else { alpha }));
                }
                g.round(pill, pill_h / 2.0, ground);
                // The inset highlight along its top edge (white at 0.14).
                let edge = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }.with_alpha(if dark { 0.0 } else { 0.14 });
                g.outline(pill, pill_h / 2.0, 1.0, edge.with_alpha(0.5));
                g.text(text, pill, font, ink);
            });
            let now = Instant::now();
            let from = if shown { s.level.value(now) } else { 0.0 };
            s.level = Tween::new(from, 1.0, now, specs::TOOLTIP_IN);
            let alpha = level_alpha(s.level.value(now));
            update(&drawn, Some(alpha), true);
            s.drawn = Some(drawn);
        });
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        anim::drive(step);
    }

    /// Fades it out (80 ms) and hides it.
    pub fn hide(&self) {
        let visible = unsafe { IsWindowVisible(self.hwnd).as_bool() };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let now = Instant::now();
            if !visible {
                return;
            }
            let from = s.level.value(now);
            s.level = Tween::new(
                from,
                0.0,
                now,
                Spec { ms: (specs::TOOLTIP_OUT.ms as f32 * from) as u32, ..specs::TOOLTIP_OUT },
            );
        });
        if visible {
            anim::drive(step);
        }
    }
}

fn level_alpha(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn step() -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let now = Instant::now();
        let level = s.level.value(now);
        let moving = s.level.moving(now);
        let Some(drawn) = s.drawn.as_ref() else { return false };
        update(drawn, Some(level_alpha(level)), false);
        anim::mark("tooltip");
        if !moving && s.level.target() == 0.0 {
            unsafe {
                let _ = ShowWindow(drawn.hwnd, SW_HIDE);
            }
            release(&mut s.drawn);
        }
        moving
    })
}

pub(crate) fn new_bitmap(hwnd: HWND, size: SIZE, origin: POINT) -> Option<Drawn> {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size.cx,
                biHeight: -size.cy,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, bitmap.into());
        Some(Drawn { hwnd, dc, bitmap, old, size, origin })
    }
}

pub(crate) fn release(drawn: &mut Option<Drawn>) {
    if let Some(d) = drawn.take() {
        unsafe {
            SelectObject(d.dc, d.old);
            let _ = DeleteObject(d.bitmap.into());
            let _ = DeleteDC(d.dc);
        }
    }
}

/// Puts the bitmap on screen at its place, at `alpha`; with `content`
/// false only the opacity changes.
pub(crate) fn update(d: &Drawn, alpha: Option<u8>, content: bool) {
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha.unwrap_or(255),
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let zero = POINT { x: 0, y: 0 };
    unsafe {
        let _ = if content {
            UpdateLayeredWindow(
                d.hwnd,
                None,
                Some(&d.origin),
                Some(&d.size),
                Some(d.dc),
                Some(&zero),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        } else {
            UpdateLayeredWindow(d.hwnd, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA)
        };
    }
}

fn work_area(near: Rect) -> Rect {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: near.left, y: near.top }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            return Rect::new(r.left, r.top, r.right, r.bottom);
        }
    }
    Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// For the script control: whether it is up and how opaque.
pub fn state() -> (bool, f32) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.drawn.is_some(), s.level.value(Instant::now()))
    })
}
