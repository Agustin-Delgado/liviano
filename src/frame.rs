//! The window's own frame: no title bar, the tab strip in its place and the
//! three window buttons drawn here: Windows' own glyphs, smaller, in one
//! faint capsule at the strip's left end. This is the geometry (where the
//! buttons and the caption are, and what the pointer is over: tested) and
//! how the buttons are drawn.

use crate::{
    design::{Palette, Rgb},
    gfx::{Color, Painter, RectF},
    tabbar::{Layout, Rect},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowButton {
    Close,
    Minimize,
    Maximize,
}

impl WindowButton {
    /// Left to right, in Windows' order.
    pub const ALL: [WindowButton; 3] = [WindowButton::Minimize, WindowButton::Maximize, WindowButton::Close];
}

/// Width of each segment, which is also what each button answers to:
/// Windows 11's are 46, these 28.
pub const BUTTON_W: i32 = 28;
/// From the window's edges to the capsule on a top strip (and the
/// collapsed column's row): the air above and under the strip's 28 px
/// controls, so the capsule has the same margin on its three outer sides.
pub const LEAD: i32 = 6;
/// From the capsule to back: more than between the segments, so the window's
/// buttons and the page's read as two groups.
pub const NAV_LEAD: i32 = 12;
/// The capsule's height (a tab's) and its corner.
const CAPSULE_H: f32 = 28.0;
const CAPSULE_RADIUS: f32 = 7.0;
/// The glyphs' outer box: Windows draws 10 px, these 8.
const GLYPH: f32 = 8.0;
/// How far the capsule's ground goes from the strip towards the ink, and
/// how much further a segment goes under the pointer and held down.
const CAPSULE_TINT: f32 = 0.05;
const SEGMENT_HOVER: f32 = 0.06;
const SEGMENT_PRESSED: f32 = 0.11;

/// The band along the top edge that resizes the window while it is not
/// maximized: the frame Windows had there is gone with the title bar.
pub const RESIZE_BAND: i32 = 5;
/// The top corners resize diagonally this far in from each side.
const CORNER: i32 = 16;

/// Where the window's own controls are: the caption (the band that moves
/// the window where nothing else is) and the three buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub caption: Rect,
    /// Minimize, maximize and close, left to right.
    pub buttons: [Rect; 3],
}

impl Frame {
    /// The buttons from `lead` (device pixels) in from the caption's left
    /// end, across its full height.
    pub fn new(caption: Rect, lead: i32, scale: f32) -> Self {
        let w = px(BUTTON_W, scale);
        let left = caption.left + lead;
        let mut buttons = [Rect::default(); 3];
        for (i, slot) in buttons.iter_mut().enumerate() {
            let l = left + i as i32 * w;
            *slot = Rect::new(l, caption.top, l + w, caption.bottom);
        }
        Self { caption, buttons }
    }

    /// Where the buttons end: what comes after them starts past here.
    pub fn right(&self) -> i32 {
        self.buttons[2].right
    }

    pub fn button_at(&self, x: i32, y: i32) -> Option<WindowButton> {
        self.buttons.iter().position(|r| r.contains(x, y)).map(|i| WindowButton::ALL[i])
    }

    pub fn rect_of(&self, button: WindowButton) -> Rect {
        self.buttons[WindowButton::ALL.iter().position(|b| *b == button).unwrap_or(0)]
    }
}

pub fn px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

/// What a point of the client area is to Windows (`WM_NCHITTEST`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameHit {
    /// Ours: tabs, buttons drawn as client, the page.
    Client,
    /// Moves the window (drag), maximizes it (double click), opens the
    /// system menu (right click).
    Caption,
    Top,
    TopLeft,
    TopRight,
    /// `HTMAXBUTTON`: Windows 11 shows its Snap Layouts over it.
    Maximize,
    Minimize,
    Close,
}

/// What (x, y), in client pixels, is: the top edge resizes (unless
/// maximized), the buttons are buttons, the caption's empty space moves the
/// window, and everything else is the app's.
pub fn hit(layout: &Layout, width: i32, x: i32, y: i32, maximized: bool, scale: f32) -> FrameHit {
    if !maximized && y >= 0 && y < px(RESIZE_BAND, scale) {
        let corner = px(CORNER, scale);
        return if x < corner {
            FrameHit::TopLeft
        } else if x >= width - corner {
            FrameHit::TopRight
        } else {
            FrameHit::Top
        };
    }
    match layout.frame.button_at(x, y) {
        Some(WindowButton::Minimize) => return FrameHit::Minimize,
        Some(WindowButton::Maximize) => return FrameHit::Maximize,
        Some(WindowButton::Close) => return FrameHit::Close,
        None => {}
    }
    if layout.frame.caption.contains(x, y) && layout.hit(x, y).is_none() {
        return FrameHit::Caption;
    }
    FrameHit::Client
}

/// The client area's top while the window is maximized: Windows makes a
/// maximized window larger than the monitor by its frame on every side, so
/// without the title bar the top would be cut by that much.
pub fn client_top(window_top: i32, maximized: bool, frame: i32) -> i32 {
    if maximized { window_top + frame } else { window_top }
}

/// Windows' red under the close button, and its ink there.
const CLOSE_HOVER: Rgb = Rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED: Rgb = Rgb(0xC8, 0x40, 0x31);
const CLOSE_INK: Rgb = Rgb(0xFF, 0xFF, 0xFF);

/// Draws the three buttons: one faint capsule under them, split in three
/// segments that darken under the pointer (red under close, as Windows
/// does, the capsule's rounded end kept), and Windows' glyphs on whole
/// pixels. `hover` gives each one's fade (0..1). In a window that is not
/// the active one the glyphs go quieter.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    g: &Painter,
    frame: &Frame,
    pal: &Palette,
    scale: f32,
    maximized: bool,
    active: bool,
    pressed: Option<WindowButton>,
    hover: impl Fn(WindowButton) -> f32,
) {
    if frame.buttons[0].width() <= 0 {
        return;
    }
    let rest = if active { pal.ink_muted } else { pal.ink_asleep };
    let s = |v: f32| (v * scale).round();
    let stroke = s(1.0).max(1.0);
    let radius = s(CAPSULE_RADIUS);
    let capsule = capsule(frame, scale);
    g.round(capsule, radius, tint(pal, CAPSULE_TINT));
    for (i, button) in WindowButton::ALL.into_iter().enumerate() {
        let area: RectF = frame.rect_of(button).into();
        let segment = RectF::new(area.left, capsule.top, area.right, capsule.bottom);
        let over = hover(button);
        if over > 0.0 {
            let down = pressed == Some(button);
            let fill = match (button, down) {
                (WindowButton::Close, false) => CLOSE_HOVER,
                (WindowButton::Close, true) => CLOSE_PRESSED,
                (_, false) => tint(pal, CAPSULE_TINT + SEGMENT_HOVER),
                (_, true) => tint(pal, CAPSULE_TINT + SEGMENT_PRESSED),
            };
            // The end segments keep the capsule's rounded end: a rounded
            // shape running past the inner edge, cut there.
            let shape = match i {
                0 => RectF::new(segment.left, segment.top, segment.right + radius, segment.bottom),
                2 => RectF::new(segment.left - radius, segment.top, segment.right, segment.bottom),
                _ => segment,
            };
            let corner = if i == 1 { 0.0 } else { radius };
            g.clip(segment, |g| g.round(shape, corner, Color::from(fill).with_alpha(over)));
        }
        let lit = if button == WindowButton::Close { CLOSE_INK } else { pal.ink };
        let ink = Color::from(rest.mix(lit, over));
        draw_glyph(g, area.center(), s(GLYPH), stroke, button, maximized, ink, scale);
    }
}

/// The capsule: the three buttons' width, a tab's height, centred in the
/// caption on whole pixels.
fn capsule(frame: &Frame, scale: f32) -> RectF {
    let first: RectF = frame.buttons[0].into();
    let h = (CAPSULE_H * scale).round().min(first.height() - (4.0 * scale).round());
    let top = (first.center().1 - h / 2.0).round();
    RectF::new(first.left, top, frame.right() as f32, top + h)
}

/// A step of the ink over the strip.
fn tint(pal: &Palette, t: f32) -> Rgb {
    pal.chrome.mix(pal.ink, t)
}

/// Windows' glyphs on whole pixels: every straight edge a filled run of
/// pixels, the cross two bands cut square at the box's corners. `size` is
/// the outer box and `stroke` the line, both in device pixels.
#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    g: &Painter,
    (cx, cy): (f32, f32),
    size: f32,
    stroke: f32,
    button: WindowButton,
    maximized: bool,
    ink: Color,
    scale: f32,
) {
    // The box's outer edges on whole pixels, as close to the centre as they get.
    let l = (cx - size / 2.0).round();
    let t = (cy - size / 2.0).round();
    let (r, b) = (l + size, t + size);
    let w = stroke;
    let frame_box = |l: f32, t: f32, r: f32, b: f32| {
        g.fill(RectF::new(l, t, r, t + w), ink);
        g.fill(RectF::new(l, b - w, r, b), ink);
        g.fill(RectF::new(l, t + w, l + w, b - w), ink);
        g.fill(RectF::new(r - w, t + w, r, b - w), ink);
    };
    match button {
        WindowButton::Minimize => {
            let y = (t + (size - w) / 2.0).round();
            g.fill(RectF::new(l, y, r, y + w), ink);
        }
        WindowButton::Maximize if maximized => {
            let o = (2.0 * scale).round().max(2.0);
            frame_box(l, t + o, r - o, b);
            // The window behind: its top and right edges, and the stubs
            // that reach down to the front one.
            g.fill(RectF::new(l + o, t, r, t + w), ink);
            g.fill(RectF::new(r - w, t + w, r, b - o), ink);
            g.fill(RectF::new(l + o, t + w, l + o + w, t + o), ink);
            g.fill(RectF::new(r - o, b - o - w, r - w, b - o), ink);
        }
        WindowButton::Maximize => frame_box(l, t, r, b),
        WindowButton::Close => {
            // A band of width `w` along each diagonal, its ends cut by the box.
            let a = w / std::f32::consts::SQRT_2;
            g.polygon(&[(l, t), (l + a, t), (r, b - a), (r, b), (r - a, b), (l, t + a)], ink);
            g.polygon(&[(r, t), (r, t + a), (l + a, b), (l, b), (l, b - a), (r - a, t)], ink);
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/frame.rs"]
#[rustfmt::skip]
mod tests;
