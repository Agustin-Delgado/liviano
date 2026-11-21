//! The few controls the app draws itself, so they look the same in every
//! window and in both themes: buttons (owner-drawn, with Direct2D), check
//! boxes (native, drawn by us through custom draw) and the soft "well" a
//! native edit or list sits in instead of a sunken 3D edge.
//!
//! They follow the design system's recipes: the primary button is the brand's (`default`
//! variant), the rest are `outline` (a surface standing off the page on a
//! 1 px ring), a check box is a 4 px-cornered box that fills with the brand
//! when checked. Hover and press fade in 150 ms on ease-out, a check fills
//! in 75 ms, as their transitions say.
//!
//! Native controls are kept wherever they bring behaviour for free (edits,
//! combo boxes, check boxes, list boxes); only how they are framed changes.

use std::{
    cell::{Cell, RefCell},
    time::Instant,
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::InvalidateRect,
    UI::{
        Controls::{
            BST_CHECKED, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT,
            CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, NMCUSTOMDRAW, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED, WM_MOUSELEAVE,
        },
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::{BM_GETCHECK, GetWindowTextLengthW, GetWindowTextW, SendMessageW, WM_MOUSEMOVE},
    },
};

use crate::{
    anim::{self, Levels, specs},
    design::{Palette, lift, size},
    gfx::{Align, Color, Font, Painter, RectF, Surface},
};

thread_local! {
    /// The owner-drawn button under the pointer.
    static HOT: Cell<isize> = const { Cell::new(0) };
    /// Each control's hover, press and check, fading (keyed by its handle).
    static LEVELS: RefCell<(Levels<isize>, Levels<isize>, Levels<isize>)> = RefCell::new(Default::default());
    static CHECK_SURFACE: RefCell<Surface> = RefCell::new(Surface::default());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// The one action a window leads with: the brand's (the `default` variant).
    Primary,
    /// Everything else: a surface on a ring (the `outline` variant).
    Secondary,
}

/// Lets an owner-drawn button know when the pointer is over it, so it can
/// draw its hover. Call once after creating it.
pub fn track_hover(button: HWND) {
    unsafe {
        let _ = SetWindowSubclass(button, Some(hover_proc), 0x4C56, 0);
    }
}

unsafe extern "system" fn hover_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
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

/// Moves a control's hover and press toward what they are now and says
/// where they are this frame; asks for another frame while they move.
fn follow(hwnd: HWND, hot: bool, pressed: bool) -> (f32, f32) {
    let key = hwnd.0 as isize;
    let now = Instant::now();
    let (h, p, moving) = LEVELS.with(|l| {
        let (hover, press, _) = &mut *l.borrow_mut();
        hover.set(key, if hot { 1.0 } else { 0.0 }, now, specs::HOVER);
        press.set(key, if pressed { 1.0 } else { 0.0 }, now, specs::HOVER);
        (hover.get(key, now), press.get(key, now), hover.moving(now) || press.moving(now))
    });
    if moving {
        anim::mark("widget");
        anim::repaint(hwnd, None);
    }
    (h, p)
}

/// Draws an owner-drawn button from its WM_DRAWITEM. `ground` is what the
/// button sits on (its corners show it).
pub fn draw_button_item(
    surface: &mut Surface,
    item: &DRAWITEMSTRUCT,
    kind: ButtonKind,
    ground: Color,
    pal: &Palette,
    scale: f32,
) {
    let text = window_text(item.hwndItem);
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let hot = HOT.with(|h| h.get()) == item.hwndItem.0 as isize;
    let (hot, pressed) = follow(item.hwndItem, hot, pressed);
    let state = ButtonState {
        pressed,
        hot,
        focused: item.itemState.0 & ODS_FOCUS.0 != 0,
        disabled: item.itemState.0 & ODS_DISABLED.0 != 0,
    };
    surface.paint(item.hDC, item.rcItem, |g| {
        g.fill(item.rcItem.into(), ground);
        draw_button(g, item.rcItem.into(), &text, kind, state, pal, scale);
    });
}

/// A button's state this frame: hover and press as 0..1 levels.
#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonState {
    pub pressed: f32,
    pub hot: f32,
    pub focused: bool,
    pub disabled: bool,
}

pub fn draw_button(
    g: &Painter,
    rect: RectF,
    text: &str,
    kind: ButtonKind,
    state: ButtonState,
    pal: &Palette,
    scale: f32,
) {
    let radius = size::RADIUS_FIELD * scale;
    // Disabled: 45 % opacity, as the recipe's `disabled:opacity-45`.
    let alpha = if state.disabled { 0.45 } else { 1.0 };
    let black = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    let (fill, ink) = match kind {
        ButtonKind::Primary => {
            let fill = pal.accent.mix(pal.accent_hover, state.hot.max(state.pressed));
            (fill, pal.on_accent)
        }
        ButtonKind::Secondary => {
            // The surface, a step into the hover grey while pressed.
            let fill = pal.field.mix(pal.tab_hover, state.pressed);
            (fill, pal.ink_muted.mix(pal.ink, state.hot.max(state.pressed)))
        }
    };
    if kind == ButtonKind::Secondary {
        // shadow-button: a ring and a small drop, both a little stronger
        // under the pointer, and the drop gone while it is held down.
        let drop = (0.08 + 0.04 * state.hot) * (1.0 - state.pressed) * if pal.dark { 2.5 } else { 1.0 };
        let below = RectF::new(rect.left + scale, rect.top + scale, rect.right - scale, rect.bottom + scale);
        g.round(below, radius, black.with_alpha(drop * alpha));
    }
    g.round(rect, radius, Color::from(fill).with_alpha(alpha));
    if kind == ButtonKind::Secondary {
        let ring = if pal.dark { Color::from(pal.line_strong) } else { black.with_alpha(0.10 + 0.04 * state.hot) };
        g.outline(rect, radius, 1.0, ring.with_alpha(alpha));
    }
    if state.focused {
        focus_ring(g, rect, radius, pal, scale);
    }
    let font = Font::new(size::TEXT_SMALL * scale, size::MEDIUM, Align::Center);
    g.text(text, rect.inset(8.0 * scale, 0.0), font, Color::from(ink).with_alpha(alpha));
}

/// shadow-focus: 1 px of brand and a soft halo around it.
fn focus_ring(g: &Painter, rect: RectF, radius: f32, pal: &Palette, scale: f32) {
    g.outline(
        rect.inset(-2.0 * scale, -2.0 * scale),
        radius + 2.0 * scale,
        3.0 * scale,
        Color::from(pal.accent).with_alpha(lift::FOCUS_HALO),
    );
    g.outline(rect, radius, 1.0, pal.accent);
}

/// Draws a native check box (BS_AUTOCHECKBOX) from its parent's
/// NM_CUSTOMDRAW: the control keeps its behaviour (click, Space, its state),
/// only how it looks is ours. `ground` is what it sits on. Returns the
/// value WM_NOTIFY answers with.
pub fn draw_check(nm: &NMCUSTOMDRAW, ground: Color, pal: &Palette, scale: f32) -> LRESULT {
    if nm.dwDrawStage != CDDS_PREPAINT {
        return LRESULT(CDRF_DODEFAULT as isize);
    }
    let hwnd = nm.hdr.hwndFrom;
    let state = nm.uItemState.0;
    let checked = unsafe { SendMessageW(hwnd, BM_GETCHECK, None, None).0 } == BST_CHECKED.0 as isize;
    let (hot, pressed) = follow(hwnd, state & CDIS_HOT.0 != 0, state & CDIS_SELECTED.0 != 0);
    let key = hwnd.0 as isize;
    let now = Instant::now();
    let (on, moving) = LEVELS.with(|l| {
        let checks = &mut l.borrow_mut().2;
        checks.set(key, if checked { 1.0 } else { 0.0 }, now, specs::CHECK);
        (checks.get(key, now), checks.moving(now))
    });
    if moving {
        anim::repaint(hwnd, None);
    }
    let disabled = state & CDIS_DISABLED.0 != 0;
    let focused = state & CDIS_FOCUS.0 != 0;
    let full: RectF = nm.rc.into();
    CHECK_SURFACE.with(|s| {
        s.borrow_mut().paint(nm.hdc, nm.rc, |g| {
            g.fill(full, ground);
            let side = (16.0 * scale).min(full.width()).min(full.height()).round();
            let (cx, cy) = full.center();
            let box_ = RectF::new(
                (cx - side / 2.0).round(),
                (cy - side / 2.0).round(),
                (cx - side / 2.0).round() + side,
                (cy - side / 2.0).round() + side,
            );
            let radius = size::RADIUS_CHIP * scale;
            let alpha = if disabled { 0.5 } else { 1.0 };
            let black = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
            // shadow-chip under the box.
            let drop = RectF::new(box_.left, box_.top + scale, box_.right, box_.bottom + scale);
            g.round(drop, radius, black.with_alpha(0.08 * alpha * (1.0 - on)));
            let surface = pal.field.mix(pal.tab_hover, pressed);
            g.round(box_, radius, Color::from(surface.mix(pal.accent, on)).with_alpha(alpha));
            // The border: the line, toward brand/40 under the pointer, brand when checked.
            let edge = pal.line_strong.mix(pal.line_strong.mix(pal.accent, 0.4), hot).mix(pal.accent, on);
            g.outline(box_, radius, 1.0, Color::from(edge).with_alpha(alpha));
            if on > 0.0 {
                let s = |v: f32| v * side / 16.0;
                let tick = [
                    (box_.left + s(4.0), box_.top + s(8.3)),
                    (box_.left + s(6.8), box_.top + s(11.0)),
                    (box_.left + s(12.0), box_.top + s(5.2)),
                ];
                g.polyline(&tick, 1.6 * scale, Color::from(pal.on_accent).with_alpha(on.clamp(0.0, 1.0) * alpha));
            }
            if focused {
                focus_ring(g, box_, radius, pal, scale);
            }
        })
    });
    LRESULT(CDRF_SKIPDEFAULT as isize)
}

/// The soft rounded ground a native edit or list sits in, drawn by its
/// parent around the child (which is placed `inset` inside it): the
/// hover grey with a field's corner (the design system's filled field).
pub fn draw_well(g: &Painter, rect: RectF, pal: &Palette, scale: f32) {
    let radius = size::RADIUS_FIELD * scale;
    g.round(rect, radius, pal.selection);
}

pub fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buffer = vec![0u16; len + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer) as usize;
        String::from_utf16_lossy(&buffer[..copied])
    }
}
