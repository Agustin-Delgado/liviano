//! The window's own frame, as Chrome and Edge draw theirs: no title bar,
//! the tab strip in its place and the three window buttons drawn here. This
//! is the geometry (where the buttons and the caption are, and what the
//! pointer is over: tested) and how the buttons are drawn.

use crate::{
    design::{Palette, Rgb},
    gfx::{Color, Painter, RectF},
    tabbar::{Layout, Rect},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

impl WindowButton {
    pub const ALL: [WindowButton; 3] = [WindowButton::Minimize, WindowButton::Maximize, WindowButton::Close];
}

/// Width of one window button: Windows 11's own.
pub const BUTTON_W: i32 = 46;
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
    /// The buttons right-aligned so the last one ends at `right`, across
    /// the caption's full height.
    pub fn new(caption: Rect, right: i32, scale: f32) -> Self {
        let w = px(BUTTON_W, scale);
        let mut buttons = [Rect::default(); 3];
        for (i, slot) in buttons.iter_mut().enumerate() {
            let r = right - (2 - i as i32) * w;
            *slot = Rect::new(r - w, caption.top, r, caption.bottom);
        }
        Self { caption, buttons }
    }

    pub fn width(&self) -> i32 {
        self.buttons[2].right - self.buttons[0].left
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

/// Windows 11's red under the close button, and its ink there.
const CLOSE_HOVER: Rgb = Rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED: Rgb = Rgb(0xC8, 0x40, 0x31);
const CLOSE_INK: Rgb = Rgb(0xFF, 0xFF, 0xFF);

/// Draws the three buttons. `hover` gives each one's fade (0..1).
pub fn paint(
    g: &Painter,
    frame: &Frame,
    pal: &Palette,
    scale: f32,
    maximized: bool,
    pressed: Option<WindowButton>,
    hover: impl Fn(WindowButton) -> f32,
) {
    for button in WindowButton::ALL {
        let full: RectF = frame.rect_of(button).into();
        if full.width() <= 0.0 {
            continue;
        }
        // Clear of whatever ran under it (tabs past their smallest width
        // overflow the strip), short of the strip's bottom line.
        let rect = RectF::new(full.left, full.top, full.right, full.bottom - 1.0);
        g.fill(rect, pal.chrome);
        let over = hover(button);
        let down = pressed == Some(button) && over > 0.0;
        let ink = if button == WindowButton::Close {
            if down {
                g.fill(rect, CLOSE_PRESSED);
            } else if over > 0.0 {
                g.fill(rect, Color::from(CLOSE_HOVER).with_alpha(over));
            }
            pal.ink_muted.mix(CLOSE_INK, over)
        } else {
            if down {
                g.fill(rect, pal.tab_active);
            } else if over > 0.0 {
                g.fill(rect, Color::from(pal.tab_hover).with_alpha(over));
            }
            pal.ink_muted.mix(pal.ink, over)
        };
        draw_glyph(g, full, button, maximized, ink, scale);
    }
}

/// The glyphs Windows 11 draws: a dash, a square (two for restore) and a
/// cross, 10 px wide, in hairlines.
fn draw_glyph(g: &Painter, rect: RectF, button: WindowButton, maximized: bool, ink: Rgb, scale: f32) {
    let s = |v: f32| v * scale;
    let (cx, cy) = rect.center();
    // Snapped to the pixel grid so the hairlines stay crisp.
    let (cx, cy) = (cx.round() + 0.5, cy.round() + 0.5);
    let w = s(1.0).max(1.0);
    let half = s(5.0).round();
    match button {
        WindowButton::Minimize => g.line((cx - half, cy), (cx + half, cy), w, ink),
        WindowButton::Maximize if maximized => {
            let front = RectF::new(cx - half, cy - half + s(2.0), cx + half - s(2.0), cy + half);
            g.outline(front.inset(-w / 2.0, -w / 2.0), s(1.5), w, ink);
            // The window behind: only its top and right edges show.
            let (l, t, r, b) = (cx - half + s(2.0), cy - half, cx + half, cy + half - s(2.0));
            g.polyline(&[(l, t + s(0.5)), (l + s(1.0), t), (r - s(1.0), t), (r, t + s(1.0)), (r, b - s(1.0)), (r - s(0.5), b)], w, ink);
        }
        WindowButton::Maximize => {
            let square = RectF::new(cx - half, cy - half, cx + half, cy + half);
            g.outline(square.inset(-w / 2.0, -w / 2.0), s(1.5), w, ink);
        }
        WindowButton::Close => {
            g.line((cx - half, cy - half), (cx + half, cy + half), w, ink);
            g.line((cx - half, cy + half), (cx + half, cy - half), w, ink);
        }
    }
}

