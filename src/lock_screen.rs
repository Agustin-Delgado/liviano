//! What a locked window draws over its whole client area: the profile's
//! round mark, its name, why it is locked, the button that unlocks it and,
//! with two profiles or more, the link to the others. Where each piece goes
//! is pure (`place`); the window's buttons stay over it, drawn by `frame`.

use crate::{
    design::{Palette, Rgb, group_tone, size},
    gfx::{Align, Color, Font, Painter, RectF},
    tab_groups::GroupColor,
    widgets::{self, ButtonKind, ButtonState},
};

/// The round mark, in DIPs.
const AVATAR: f32 = 64.0;
/// Its initial: the display step.
const AVATAR_TEXT: f32 = 28.0;
const NAME_H: f32 = 20.0;
const LINE_H: f32 = 20.0;
const BUTTON_H: f32 = 32.0;
const BUTTON_PAD: f32 = 16.0;
const BUTTON_MIN_W: f32 = 200.0;
const LINK_H: f32 = 24.0;
const LINK_PAD: f32 = 6.0;
const NOTE_H: f32 = 18.0;
const GAP_AVATAR: f32 = 16.0;
const GAP_NAME: f32 = 4.0;
const GAP_BUTTON: f32 = 24.0;
const GAP_LINK: f32 = 8.0;
const GAP_NOTE: f32 = 8.0;
/// The widest the line under the name goes, and its margin from the edges.
const LINE_MAX_W: f32 = 480.0;
const SIDE: f32 = 16.0;

pub const UNLOCK_LABEL: &str = "Unlock with Windows Hello";
pub const OTHER_LABEL: &str = "Use another profile";

pub fn name_font(scale: f32) -> Font {
    Font::new(size::TEXT_FIELD * scale, size::MEDIUM, Align::Center)
}

pub fn line_font(scale: f32) -> Font {
    Font::new(size::TEXT * scale, size::REGULAR, Align::Center)
}

/// The button's text, as `widgets::draw_button` draws it.
pub fn button_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::MEDIUM, Align::Center)
}

pub fn link_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Center)
}

/// Where each piece goes, in client pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Places {
    pub avatar: RectF,
    pub name: RectF,
    pub line: RectF,
    pub button: RectF,
    /// "Use another profile", with two profiles or more.
    pub other: Option<RectF>,
    /// A line from the last try ("Windows Hello did not verify you.").
    pub note: RectF,
}

/// What a point of the lock screen is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockHit {
    Unlock,
    Other,
}

impl Places {
    pub fn hit(&self, x: i32, y: i32) -> Option<LockHit> {
        let inside =
            |r: RectF| (x as f32) >= r.left && (x as f32) < r.right && (y as f32) >= r.top && (y as f32) < r.bottom;
        if inside(self.button) {
            return Some(LockHit::Unlock);
        }
        self.other.filter(|r| inside(*r)).map(|_| LockHit::Other)
    }
}

/// The pieces, one under the other, centred in a `width` × `height` client
/// area below `top` (the caption row). `button_text` and `other_text` are
/// the widths of the two labels in pixels (`other_text` None: one profile).
pub fn place(width: i32, height: i32, top: i32, scale: f32, button_text: f32, other_text: Option<f32>) -> Places {
    let px = |v: f32| (v * scale).round();
    let (w, h) = (width.max(0) as f32, height.max(0) as f32);
    let link = other_text.map_or(0.0, |_| px(GAP_LINK) + px(LINK_H));
    let total = px(AVATAR)
        + px(GAP_AVATAR)
        + px(NAME_H)
        + px(GAP_NAME)
        + px(LINE_H)
        + px(GAP_BUTTON)
        + px(BUTTON_H)
        + link
        + px(GAP_NOTE)
        + px(NOTE_H);
    let top = top as f32;
    // Centred in the whole window, as long as it stays under the caption.
    let mut y = ((h - total) / 2.0).round().max(top);
    let cx = (w / 2.0).round();
    let centred = |width: f32, y: f32, height: f32| {
        let half = (width / 2.0).round();
        RectF::new(cx - half, y, cx - half + width, y + height)
    };
    let text_w = (w - 2.0 * px(SIDE)).clamp(0.0, px(LINE_MAX_W));
    let avatar = centred(px(AVATAR), y, px(AVATAR));
    y += px(AVATAR) + px(GAP_AVATAR);
    let name = centred(text_w, y, px(NAME_H));
    y += px(NAME_H) + px(GAP_NAME);
    let line = centred(text_w, y, px(LINE_H));
    y += px(LINE_H) + px(GAP_BUTTON);
    let button_w = (button_text.ceil() + 2.0 * px(BUTTON_PAD)).max(px(BUTTON_MIN_W)).min(text_w.max(px(BUTTON_MIN_W)));
    let button = centred(button_w, y, px(BUTTON_H));
    y += px(BUTTON_H);
    let other = other_text.map(|text| {
        y += px(GAP_LINK);
        let r = centred(text.ceil() + 2.0 * px(LINK_PAD), y, px(LINK_H));
        y += px(LINK_H);
        r
    });
    y += px(GAP_NOTE);
    let note = centred(text_w, y, px(NOTE_H));
    Places { avatar, name, line, button, other, note }
}

/// What the lock screen shows.
pub struct LockPaint<'a> {
    pub palette: &'a Palette,
    pub scale: f32,
    pub places: Places,
    pub name: &'a str,
    pub initial: char,
    pub color: GroupColor,
    pub line: &'a str,
    pub note: Option<&'a str>,
    pub hover: Option<LockHit>,
    pub pressed: Option<LockHit>,
    /// A prompt is up: the button waits.
    pub waiting: bool,
}

/// Draws the lock screen over the whole `width` × `height` client area.
pub fn paint(g: &Painter, width: i32, height: i32, p: &LockPaint) {
    let pal = p.palette;
    let s = p.scale;
    g.fill(RectF::new(0.0, 0.0, width as f32, height as f32), pal.chrome);
    let disc = p.places.avatar;
    g.round(disc, disc.width() / 2.0, group_tone(p.color, pal.dark).fill);
    // The dark palette's tones are light: dark ink reads on them (as the
    // strip's profile button).
    let ink = if pal.dark { Rgb::grey(0x17) } else { Rgb::grey(0xFF) };
    let mut buffer = [0u8; 4];
    let letter: &str = p.initial.encode_utf8(&mut buffer);
    g.text(letter, disc, Font::new(AVATAR_TEXT * s, size::MEDIUM, Align::Center), ink);
    g.text(p.name, p.places.name, name_font(s), pal.ink);
    g.text(p.line, p.places.line, line_font(s), pal.ink_muted);
    let state = ButtonState {
        pressed: if p.pressed == Some(LockHit::Unlock) { 1.0 } else { 0.0 },
        hot: if p.hover == Some(LockHit::Unlock) { 1.0 } else { 0.0 },
        focused: false,
        disabled: p.waiting,
    };
    widgets::draw_button(g, p.places.button, UNLOCK_LABEL, ButtonKind::Primary, state, pal, s);
    if let Some(other) = p.places.other {
        let over = p.hover == Some(LockHit::Other);
        let ink = if over { pal.ink } else { pal.ink_muted };
        g.text(OTHER_LABEL, other, link_font(s), Color::from(ink));
    }
    if let Some(note) = p.note {
        g.text(note, p.places.note, link_font(s), pal.ink_muted);
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/lock_screen.rs"]
#[rustfmt::skip]
mod tests;
