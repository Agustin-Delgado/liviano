//! The window's own frame: no title bar, the tab strip in its place and the
//! three window buttons drawn here: Windows' own glyphs, smaller, at the
//! strip's left end. This is the geometry (where the buttons and the
//! caption are, and what the pointer is over: tested) and how the buttons
//! are drawn.

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

/// Width each button answers to: Windows 11's are 46, these 30.
pub const BUTTON_W: i32 = 30;
/// The ground a button wears under the pointer, centred in its area.
const GROUND_W: f32 = 26.0;
const GROUND_H: f32 = 24.0;
/// The glyphs' box: Windows draws 10 px, these 9.
const GLYPH: f32 = 9.0;
/// From the window's left edge to the first button's area.
pub const LEAD: i32 = 4;
/// Height of the caption row when the tabs are hidden (collapsed column).
pub const ROW: i32 = 32;
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
    /// The buttons from the caption's left end, across its full height.
    pub fn new(caption: Rect, scale: f32) -> Self {
        let w = px(BUTTON_W, scale);
        let left = caption.left + px(LEAD, scale);
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

/// Draws the three buttons: thin glyphs on nothing, a soft rounded ground
/// under the pointer (red under close, as Windows does). `hover` gives each
/// one's fade (0..1). In a window that is not the active one the glyphs go
/// quieter.
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
    let rest = if active { pal.ink_muted } else { pal.ink_asleep };
    let radius = crate::design::size::RADIUS_CONTROL * scale;
    for button in WindowButton::ALL {
        let area: RectF = frame.rect_of(button).into();
        if area.width() <= 0.0 {
            continue;
        }
        let over = hover(button);
        let down = pressed == Some(button) && over > 0.0;
        let (cx, cy) = area.center();
        let (hw, hh) = (GROUND_W * scale / 2.0, GROUND_H * scale / 2.0);
        let ground = RectF::new(cx - hw, cy - hh, cx + hw, cy + hh);
        let (fill, lit) = match (button, down) {
            (WindowButton::Close, false) => (CLOSE_HOVER, CLOSE_INK),
            (WindowButton::Close, true) => (CLOSE_PRESSED, CLOSE_INK),
            (_, false) => (pal.tab_hover, pal.ink),
            (_, true) => (pal.pin_live, pal.ink),
        };
        if over > 0.0 {
            g.round(ground, radius, Color::from(fill).with_alpha(over));
        }
        draw_glyph(g, (cx, cy), button, maximized, Color::from(rest.mix(lit, over)), scale);
    }
}

/// Windows' glyphs, hairline: a dash, a square (two offset ones once
/// maximized) and a cross.
fn draw_glyph(g: &Painter, (cx, cy): (f32, f32), button: WindowButton, maximized: bool, ink: Color, scale: f32) {
    let w = scale.max(1.0);
    let h = (GLYPH * scale / 2.0).round();
    // Hairlines on pixel centres stay crisp.
    let (cx, cy) = (cx.floor() + 0.5, cy.floor() + 0.5);
    match button {
        WindowButton::Minimize => g.line((cx - h, cy), (cx + h, cy), w, ink),
        WindowButton::Maximize if maximized => {
            let o = (2.0 * scale).round();
            // The window behind: only its top and right edges show.
            g.line((cx - h + o, cy - h + o), (cx - h + o, cy - h), w, ink);
            g.line((cx - h + o, cy - h), (cx + h, cy - h), w, ink);
            g.line((cx + h, cy - h), (cx + h, cy + h - o), w, ink);
            g.line((cx + h, cy + h - o), (cx + h - o, cy + h - o), w, ink);
            stroke_box(g, RectF::new(cx - h, cy - h + o, cx + h - o, cy + h), w, ink);
        }
        WindowButton::Maximize => stroke_box(g, RectF::new(cx - h, cy - h, cx + h, cy + h), w, ink),
        WindowButton::Close => {
            g.line((cx - h, cy - h), (cx + h, cy + h), w, ink);
            g.line((cx - h, cy + h), (cx + h, cy - h), w, ink);
        }
    }
}

fn stroke_box(g: &Painter, r: RectF, w: f32, ink: Color) {
    g.line((r.left, r.top), (r.right, r.top), w, ink);
    g.line((r.right, r.top), (r.right, r.bottom), w, ink);
    g.line((r.right, r.bottom), (r.left, r.bottom), w, ink);
    g.line((r.left, r.bottom), (r.left, r.top), w, ink);
}

