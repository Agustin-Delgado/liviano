//! The window's own frame: no title bar, the tab strip in its place and the
//! three window buttons drawn here, at the strip's left end as macOS puts
//! its traffic lights. This is the geometry (where the buttons and the
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
    /// Left to right, as macOS orders them: close, minimize, zoom.
    pub const ALL: [WindowButton; 3] = [WindowButton::Close, WindowButton::Minimize, WindowButton::Maximize];
}

/// Width each button answers to: the 12 px light with 4 px either side, so
/// the three touch and a near miss still lands on one.
pub const BUTTON_W: i32 = 20;
/// The light itself (macOS draws its traffic lights 12 px across).
const LIGHT: f32 = 12.0;
/// From the window's left edge to the first button's area.
pub const LEAD: i32 = 8;
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
    /// Close, minimize and maximize, left to right.
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

/// One traffic light: its fill, its rim and the glyph that shows on it
/// while the pointer is over the three (macOS's own colours).
struct Light {
    fill: Rgb,
    rim: Rgb,
    glyph: Rgb,
}

fn light(button: WindowButton) -> Light {
    match button {
        WindowButton::Close => Light { fill: Rgb(0xFF, 0x5F, 0x57), rim: Rgb(0xE2, 0x46, 0x3F), glyph: Rgb(0x4D, 0x00, 0x00) },
        WindowButton::Minimize => Light { fill: Rgb(0xFE, 0xBC, 0x2E), rim: Rgb(0xE1, 0xA1, 0x16), glyph: Rgb(0x99, 0x57, 0x00) },
        WindowButton::Maximize => Light { fill: Rgb(0x28, 0xC8, 0x40), rim: Rgb(0x14, 0xAE, 0x2E), glyph: Rgb(0x00, 0x65, 0x00) },
    }
}

/// Draws the three lights. `hover` gives each one's fade (0..1): over any
/// of them all three show their glyphs, as on macOS. In a window that is
/// not the active one they go grey, and get their colour back under the
/// pointer.
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
    let over = WindowButton::ALL.iter().map(|b| hover(*b)).fold(0.0f32, f32::max);
    let colour = if active { 1.0 } else { over };
    let (idle_fill, idle_rim) = if pal.dark { (Rgb::grey(0x4A), Rgb::grey(0x40)) } else { (Rgb::grey(0xD6), Rgb::grey(0xC6)) };
    for button in WindowButton::ALL {
        let area: RectF = frame.rect_of(button).into();
        if area.width() <= 0.0 {
            continue;
        }
        let (cx, cy) = area.center();
        let r = LIGHT * scale / 2.0;
        let disc = RectF::new(cx - r, cy - r, cx + r, cy + r);
        let l = light(button);
        let mut fill = idle_fill.mix(l.fill, colour);
        if pressed == Some(button) && hover(button) > 0.0 {
            fill = fill.mix(Rgb::grey(0x00), 0.2);
        }
        g.round(disc, r, idle_rim.mix(l.rim, colour));
        let rim = 0.5 * scale.max(1.0);
        g.round(disc.inset(rim, rim), r - rim, fill);
        if over > 0.0 {
            draw_glyph(g, (cx, cy), button, maximized, Color::from(l.glyph).with_alpha(over), scale);
        }
    }
}

/// The glyph a light shows under the pointer: a cross, a dash, and two
/// arrowheads pointing out (maximize) or in (restore).
fn draw_glyph(g: &Painter, (cx, cy): (f32, f32), button: WindowButton, maximized: bool, ink: Color, scale: f32) {
    let s = |v: f32| v * scale;
    let w = s(1.1);
    match button {
        WindowButton::Close => {
            let a = s(2.5);
            g.line((cx - a, cy - a), (cx + a, cy + a), w, ink);
            g.line((cx - a, cy + a), (cx + a, cy - a), w, ink);
        }
        WindowButton::Minimize => g.line((cx - s(3.0), cy), (cx + s(3.0), cy), w, ink),
        WindowButton::Maximize => {
            // Two right triangles on the diagonal, their square corners
            // away from the centre to grow and toward it to shrink back.
            let leg = s(3.6);
            if maximized {
                let n = s(0.7);
                g.polygon(&[(cx - n, cy - n), (cx - n - leg, cy - n), (cx - n, cy - n - leg)], ink);
                g.polygon(&[(cx + n, cy + n), (cx + n + leg, cy + n), (cx + n, cy + n + leg)], ink);
            } else {
                let f = s(3.4);
                g.polygon(&[(cx - f, cy - f), (cx - f + leg, cy - f), (cx - f, cy - f + leg)], ink);
                g.polygon(&[(cx + f, cy + f), (cx + f - leg, cy + f), (cx + f, cy + f - leg)], ink);
            }
        }
    }
}

