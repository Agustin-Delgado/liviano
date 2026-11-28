//! The space card: the active space's dot and name on a capsule at the
//! foot of the page, for a moment after a switch. The page is a WebView2
//! child window, so the strip cannot draw over it: the card is a popup of
//! its own, layered per pixel like the tooltip (round ends and a soft
//! shadow), that never takes the pointer or the keyboard. It rises in with
//! the toast's spring and sinks out with the toast's fade; when it is
//! gone its picture is let go, and nothing runs until the next switch.
//!
//! How long it stays is the caller's business (`app/spaces_ui.rs`); where
//! it goes and how big it is is pure (tested).

use std::{cell::RefCell, time::Instant};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

use crate::{
    anim::{self, TOAST_RISE, Tween, specs},
    design::{Palette, size},
    gfx::{self, Align, Color, Font, RectF, Surface},
    tabbar::Rect,
    tooltip::{Drawn, new_bitmap, release, update},
};

/// The capsule, before the DPI scale.
pub const HEIGHT: f32 = 32.0;
const PAD: f32 = 14.0;
const DOT: f32 = 8.0;
const GAP: f32 = 8.0;
/// A long name is cut here.
pub const NAME_MAX_W: f32 = 240.0;
/// From the capsule to the bottom of the page.
pub const BOTTOM: f32 = 32.0;
/// Room around the capsule for its shadow.
const SHADOW: f32 = 10.0;

/// The capsule's size for a name `name_w` px wide (already scaled).
pub fn capsule_size(name_w: f32, scale: f32) -> (f32, f32) {
    let w = (PAD + DOT + GAP) * scale + name_w.min(NAME_MAX_W * scale) + PAD * scale;
    (w.ceil(), (HEIGHT * scale).round())
}

/// Where the capsule goes (its top-left corner): centered on the page
/// area `page`, `BOTTOM` above its foot, and inside it when it is small.
pub fn place(page: Rect, capsule: (f32, f32), scale: f32) -> (i32, i32) {
    let (w, h) = (capsule.0 as i32, capsule.1 as i32);
    let x = page.left + (page.width() - w) / 2;
    let bottom = (BOTTOM * scale).round() as i32;
    let y = (page.bottom - bottom - h).max(page.top);
    (x, y)
}

struct State {
    drawn: Option<Drawn>,
    /// 0 gone, 1 shown; it rises from `TOAST_RISE` below as it goes to 1.
    level: Tween<f32>,
    /// Where the window rests (the shadow's room included).
    rest: POINT,
    rise: f32,
    surface: Surface,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        drawn: None,
        level: Tween::rest(0.0),
        rest: POINT::default(),
        rise: 0.0,
        surface: Surface::default(),
    });
}

pub struct SpaceCard {
    pub hwnd: HWND,
}

impl SpaceCard {
    pub fn create(owner: HWND) -> windows::core::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: w!("NeaSpaceCard"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("NeaSpaceCard"),
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

    /// Shows `name` with its `dot` at the foot of `page` (screen pixels).
    /// Already up: drawn again in place, with no new rise.
    pub fn show(&self, name: &str, dot: Color, page: Rect, palette: &Palette, scale: f32) {
        let font = Font::new(size::TEXT * scale, size::MEDIUM, Align::Leading);
        let name_w = gfx::measure(name, font).ceil();
        let (cw, ch) = capsule_size(name_w, scale);
        let margin = (SHADOW * scale).ceil();
        let (x, y) = place(page, (cw, ch), scale);
        let size = SIZE { cx: (cw + 2.0 * margin) as i32, cy: (ch + 2.0 * margin) as i32 };
        let rest = POINT { x: x - margin as i32, y: y - margin as i32 };
        let capsule = RectF::new(margin, margin, margin + cw, margin + ch);
        let px = |v: f32| v * scale;
        let (ground, ink, dark) = (Color::from(palette.tab_active), Color::from(palette.ink), palette.dark);
        let shown = unsafe { IsWindowVisible(self.hwnd).as_bool() };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            release(&mut s.drawn);
            let Some(drawn) = new_bitmap(self.hwnd, size, rest) else { return };
            let full = RECT { left: 0, top: 0, right: size.cx, bottom: size.cy };
            s.surface.paint(drawn.dc, full, |g| {
                g.clear(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 });
                // A soft drop under the card's own ring: it sits on a page.
                let shade = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
                for (grow, alpha) in [(0.0, 0.08), (2.0, 0.05), (4.0, 0.03)] {
                    let around = capsule.inset(-px(grow), -px(grow));
                    let drop = RectF::new(around.left, around.top + px(3.0), around.right, around.bottom + px(3.0));
                    g.round(drop, drop.height() / 2.0, shade.with_alpha(if dark { alpha * 2.0 } else { alpha }));
                }
                g.ring_card(capsule, ch / 2.0, ground, dark, 1.0, scale);
                let cy = capsule.top + ch / 2.0;
                let dot_x = capsule.left + px(PAD);
                g.round(RectF::new(dot_x, cy - px(DOT) / 2.0, dot_x + px(DOT), cy + px(DOT) / 2.0), px(DOT) / 2.0, dot);
                let left = dot_x + px(DOT + GAP);
                g.text(
                    name,
                    RectF::new(left, capsule.top, capsule.right - px(PAD) + px(1.0), capsule.bottom),
                    font,
                    ink,
                );
            });
            let now = Instant::now();
            let arriving = !shown || s.level.target() == 0.0;
            if arriving {
                let from = if shown { s.level.value(now) } else { 0.0 };
                s.level = if anim::reduced() { Tween::rest(1.0) } else { Tween::new(from, 1.0, now, specs::TOAST_IN) };
            }
            s.rest = rest;
            s.rise = (TOAST_RISE * scale).round();
            s.drawn = Some(drawn);
            put(&mut s, now);
        });
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        anim::drive(step);
    }

    /// Keeps it at the foot of `page` while the window moves or changes
    /// size.
    pub fn follow(&self, page: Rect, scale: f32) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let Some(drawn) = &s.drawn else { return };
            let margin = (SHADOW * scale).ceil();
            let capsule = (drawn.size.cx as f32 - 2.0 * margin, drawn.size.cy as f32 - 2.0 * margin);
            let (x, y) = place(page, capsule, scale);
            s.rest = POINT { x: x - margin as i32, y: y - margin as i32 };
            put(&mut s, Instant::now());
        });
    }

    /// Sinks it out with the toast's fade.
    pub fn hide(&self) {
        if !self.up() {
            return;
        }
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let now = Instant::now();
            let from = s.level.value(now).clamp(0.0, 1.0);
            s.level = Tween::new(from, 0.0, now, specs::TOAST_OUT);
        });
        if anim::reduced() {
            self.hide_now();
        } else {
            anim::drive(step);
        }
    }

    /// Gone at once (the window was minimized, a video went full screen).
    pub fn hide_now(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.level = Tween::rest(0.0);
            release(&mut s.drawn);
        });
    }

    /// On screen and not on its way out.
    pub fn up(&self) -> bool {
        STATE.with(|s| {
            let s = s.borrow();
            s.drawn.is_some() && s.level.target() > 0.0
        })
    }
}

/// Puts the card where its level says: `rise` px lower at level 0.
fn put(s: &mut State, now: Instant) {
    let level = s.level.value(now);
    let (rest, rise) = (s.rest, s.rise);
    let Some(drawn) = &mut s.drawn else { return };
    let alpha = (level.clamp(0.0, 1.0) * 255.0).round() as u8;
    drawn.origin = POINT { x: rest.x, y: rest.y + (rise * (1.0 - level)).round() as i32 };
    update(drawn, Some(alpha), true);
}

fn step() -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let now = Instant::now();
        let moving = s.level.moving(now);
        if s.drawn.is_none() {
            return false;
        }
        put(&mut s, now);
        anim::mark("space card");
        if !moving && s.level.target() == 0.0 {
            if let Some(drawn) = &s.drawn {
                unsafe {
                    let _ = ShowWindow(drawn.hwnd, SW_HIDE);
                }
            }
            release(&mut s.drawn);
        }
        moving
    })
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// For the script control: whether it is up, how far in, and where.
pub fn state() -> (bool, f32, [i32; 2]) {
    STATE.with(|s| {
        let s = s.borrow();
        (s.drawn.is_some(), s.level.value(Instant::now()), [s.rest.x, s.rest.y])
    })
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/space_card.rs"]
#[rustfmt::skip]
mod tests;
