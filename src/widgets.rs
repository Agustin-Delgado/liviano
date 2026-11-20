//! The few controls the app draws itself, so they look the same in every
//! window and in both themes: buttons (owner-drawn, with Direct2D) and the
//! soft "well" a native edit or list sits in instead of a sunken 3D edge.
//!
//! Native controls are kept wherever they bring behaviour for free (edits,
//! combo boxes, check boxes, list boxes); only how they are framed changes.

use std::cell::Cell;

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::InvalidateRect,
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, WM_MOUSELEAVE},
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW, WM_MOUSEMOVE},
        },
    },
};

use crate::{
    design::{Palette, size},
    gfx::{Align, Color, Font, Painter, RectF, Surface},
};

thread_local! {
    /// The owner-drawn button under the pointer.
    static HOT: Cell<isize> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one action a window leads with: ink on the ground, like Search's
    /// black pill.
    Primary,
    /// Everything else: a soft grey, darker under the pointer.
    Secondary,
}

/// Lets an owner-drawn button know when the pointer is over it, so it can
/// draw its hover. Call once after creating it.
pub fn track_hover(button: HWND) {
    unsafe {
        let _ = SetWindowSubclass(button, Some(hover_proc), 0x4C56, 0);
    }
}

unsafe extern "system" fn hover_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_MOUSEMOVE if HOT.with(|h| h.get()) != hwnd.0 as isize => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            let before = HOT.with(|h| h.replace(hwnd.0 as isize));
            unsafe {
                if before != 0 {
                    let _ = InvalidateRect(Some(HWND(before as _)), None, false);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        WM_MOUSELEAVE => {
            if HOT.with(|h| h.get()) == hwnd.0 as isize {
                HOT.with(|h| h.set(0));
            }
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Draws an owner-drawn button from its WM_DRAWITEM. `ground` is what the
/// button sits on (its corners show it).
pub fn draw_button_item(surface: &mut Surface, item: &DRAWITEMSTRUCT, kind: ButtonKind, ground: Color, pal: &Palette, scale: f32) {
    let text = window_text(item.hwndItem);
    let state = ButtonState {
        pressed: item.itemState.0 & ODS_SELECTED.0 != 0,
        hot: HOT.with(|h| h.get()) == item.hwndItem.0 as isize,
        focused: item.itemState.0 & ODS_FOCUS.0 != 0,
        disabled: item.itemState.0 & ODS_DISABLED.0 != 0,
    };
    surface.paint(item.hDC, item.rcItem, |g| {
        g.fill(item.rcItem.into(), ground);
        draw_button(g, item.rcItem.into(), &text, kind, state, pal, scale);
    });
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonState {
    pub pressed: bool,
    pub hot: bool,
    pub focused: bool,
    pub disabled: bool,
}

pub fn draw_button(g: &Painter, rect: RectF, text: &str, kind: ButtonKind, state: ButtonState, pal: &Palette, scale: f32) {
    let radius = size::RADIUS * scale;
    let (fill, ink) = match kind {
        ButtonKind::Primary => {
            let fill = if state.pressed {
                pal.ink.mix(pal.ink_muted, 0.35)
            } else if state.hot {
                pal.ink.mix(pal.ink_muted, 0.18)
            } else {
                pal.ink
            };
            (fill, pal.field)
        }
        ButtonKind::Secondary => {
            let fill = if state.pressed {
                pal.pin_live
            } else if state.hot {
                pal.tab_active
            } else {
                pal.selection
            };
            (fill, pal.ink)
        }
    };
    let alpha = if state.disabled { 0.5 } else { 1.0 };
    g.round(rect, radius, Color::from(fill).with_alpha(alpha));
    if state.focused {
        // Keyboard focus: a ring just inside the edge, in the accent.
        g.outline(rect, radius, 1.5 * scale, pal.accent);
    }
    let font = Font::new(size::TEXT * scale, size::MEDIUM, Align::Center);
    g.text(text, rect.inset(8.0 * scale, 0.0), font, Color::from(ink).with_alpha(alpha));
}

/// The soft rounded ground a native edit or list sits in, drawn by its
/// parent around the child (which is placed `inset` inside it).
pub fn draw_well(g: &Painter, rect: RectF, pal: &Palette, scale: f32) {
    g.round(rect, size::RADIUS * scale * 0.75, pal.selection);
}

pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buffer = vec![0u16; len + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer) as usize;
        String::from_utf16_lossy(&buffer[..copied])
    }
}
