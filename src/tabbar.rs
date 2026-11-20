//! The tab strip: where each tab sits (pure geometry, tested), how it is
//! drawn (Direct2D, through `gfx`) and how it moves (`Motion`).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use windows::Win32::Foundation::RECT;

use crate::{
    design::{Palette, Rgb, motion, size},
    gfx::{Align, Color, Font, Painter, RectF},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(u64),
    Close(u64),
    NewTab,
    /// The extensions button (the puzzle piece).
    Extensions,
    /// A pinned extension next to it, by its place among the pinned ones.
    PinnedExtension(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn win(&self) -> RECT {
        RECT { left: self.left, top: self.top, right: self.right, bottom: self.bottom }
    }
}

/// What the strip needs to know about one tab.
pub struct TabInfo<'a> {
    pub id: u64,
    pub label: &'a str,
    pub initial: char,
    pub pinned: bool,
    pub asleep: bool,
    pub loading: bool,
    pub private: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: u64,
    pub rect: Rect,
    pub close: Option<Rect>,
    pub pinned: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The whole strip (top row or left column). Empty when collapsed.
    pub strip: Rect,
    /// Where the page goes.
    pub content: Rect,
    pub items: Vec<Item>,
    pub new_tab: Rect,
    pub vertical: bool,
    /// The extensions button; empty when no extension is installed.
    pub extensions: Rect,
    /// The pinned extensions beside it, in order.
    pub extension_marks: Vec<Rect>,
}

impl Default for Rect {
    fn default() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

pub struct Metrics {
    pub scale: f32,
}

/// Height of the strip across the top: 28 px tabs with 6 above and below,
/// a strip rather than a toolbar.
const BAR: i32 = 40;

impl Metrics {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
}

/// `extensions`: with `Some(pinned)`, the strip also holds the extensions
/// button and that many pinned extensions (as many as fit).
pub fn layout_with(
    tabs: &[TabInfo],
    width: i32,
    height: i32,
    vertical: bool,
    collapsed: bool,
    m: &Metrics,
    extensions: Option<usize>,
) -> Layout {
    if collapsed {
        return Layout { content: Rect::new(0, 0, width, height), vertical, ..Default::default() };
    }
    let mut layout =
        if vertical { layout_vertical(tabs, width, height, m) } else { layout_horizontal(tabs, width, height, m, extensions) };
    if let Some(pinned) = extensions {
        place_extensions(&mut layout, pinned, m);
    }
    layout
}

/// Width the extensions block takes at the right of a horizontal strip.
fn extensions_width(pinned: usize, width: i32, m: &Metrics) -> i32 {
    m.px(30) + m.px(8) + shown_marks(pinned, width / 4, m) as i32 * (m.px(28) + m.px(2))
}

/// Pinned extensions that fit in `room` pixels.
fn shown_marks(pinned: usize, room: i32, m: &Metrics) -> usize {
    pinned.min((room / (m.px(28) + m.px(2))).max(0) as usize)
}

/// Horizontal: the button at the right end, pinned ones to its left.
/// Vertical: the button at the bottom left, pinned ones to its right.
fn place_extensions(layout: &mut Layout, pinned: usize, m: &Metrics) {
    let strip = layout.strip;
    let (button_w, mark_w, gap) = (m.px(30), m.px(28), m.px(2));
    if layout.vertical {
        let pad = m.px(8);
        let row = m.px(32);
        let top = strip.bottom - pad - row;
        layout.extensions = Rect::new(pad, top, pad + button_w, top + row);
        let room = strip.right - pad - layout.extensions.right;
        let mut x = layout.extensions.right + gap;
        for _ in 0..shown_marks(pinned, room, m) {
            layout.extension_marks.push(Rect::new(x, top, x + mark_w, top + row));
            x += mark_w + gap;
        }
    } else {
        let (top, bottom) = (m.px(6), strip.bottom - m.px(6));
        let right = strip.right - m.px(8);
        layout.extensions = Rect::new(right - button_w, top, right, bottom);
        let mut x = layout.extensions.left - gap;
        for _ in 0..shown_marks(pinned, strip.width() / 4, m) {
            layout.extension_marks.insert(0, Rect::new(x - mark_w, top, x, bottom));
            x -= mark_w + gap;
        }
    }
}

fn layout_horizontal(tabs: &[TabInfo], width: i32, height: i32, m: &Metrics, extensions: Option<usize>) -> Layout {
    let bar = m.px(BAR);
    let pad = m.px(8);
    let gap = m.px(2);
    let pinned_w = m.px(30);
    let plus_w = m.px(30);
    let item_top = m.px(6);
    let item_bottom = bar - m.px(6);

    let pinned = tabs.iter().filter(|t| t.pinned).count() as i32;
    let normal = tabs.len() as i32 - pinned;
    let reserved = extensions.map_or(0, |n| extensions_width(n, width, m));
    let available = width - 2 * pad - plus_w - pinned * (pinned_w + gap) - normal * gap - reserved;
    let normal_w = if normal > 0 { (available / normal).clamp(m.px(44), m.px(200)) } else { 0 };

    let mut x = pad;
    let mut items = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let w = if tab.pinned { pinned_w } else { normal_w };
        let rect = Rect::new(x, item_top, x + w, item_bottom);
        let close = (!tab.pinned && w >= m.px(80)).then(|| {
            let size = m.px(20);
            let cy = (rect.top + rect.bottom) / 2;
            Rect::new(rect.right - m.px(4) - size, cy - size / 2, rect.right - m.px(4), cy + size / 2)
        });
        items.push(Item { id: tab.id, rect, close, pinned: tab.pinned });
        x += w + gap;
    }
    let new_tab = Rect::new(x, item_top, (x + plus_w).min(width), item_bottom);
    Layout {
        strip: Rect::new(0, 0, width, bar),
        content: Rect::new(0, bar, width, height.max(bar)),
        items,
        new_tab,
        vertical: false,
        ..Default::default()
    }
}

fn layout_vertical(tabs: &[TabInfo], width: i32, height: i32, m: &Metrics) -> Layout {
    let side = m.px(232).min(width / 2);
    let pad = m.px(8);
    let row = m.px(32);
    let gap = m.px(2);
    let cell = m.px(40);

    let mut items = Vec::with_capacity(tabs.len());
    // Pinned tabs: a grid of squares at the top.
    let per_row = ((side - 2 * pad + gap) / (cell + gap)).max(1);
    let mut y = pad;
    let mut col = 0;
    for tab in tabs.iter().filter(|t| t.pinned) {
        let x = pad + col * (cell + gap);
        items.push(Item { id: tab.id, rect: Rect::new(x, y, x + cell, y + cell), close: None, pinned: true });
        col += 1;
        if col == per_row {
            col = 0;
            y += cell + gap;
        }
    }
    if col > 0 {
        y += cell + gap;
    }
    if y > pad {
        y += m.px(6);
    }
    for tab in tabs.iter().filter(|t| !t.pinned) {
        let rect = Rect::new(pad, y, side - pad, y + row);
        let size = m.px(20);
        let cy = (rect.top + rect.bottom) / 2;
        let close = Some(Rect::new(rect.right - m.px(6) - size, cy - size / 2, rect.right - m.px(6), cy + size / 2));
        items.push(Item { id: tab.id, rect, close, pinned: false });
        y += row + gap;
    }
    let new_tab = Rect::new(pad, y, side - pad, y + row);
    Layout {
        strip: Rect::new(0, 0, side, height),
        content: Rect::new(side, 0, width.max(side), height),
        items,
        new_tab,
        vertical: true,
        ..Default::default()
    }
}

impl Layout {
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.strip.contains(x, y) {
            return None;
        }
        if self.extensions.contains(x, y) {
            return Some(Hit::Extensions);
        }
        if let Some(i) = self.extension_marks.iter().position(|r| r.contains(x, y)) {
            return Some(Hit::PinnedExtension(i));
        }
        for item in &self.items {
            if item.rect.contains(x, y) {
                if item.close.is_some_and(|c| c.contains(x, y)) {
                    return Some(Hit::Close(item.id));
                }
                return Some(Hit::Tab(item.id));
            }
        }
        self.new_tab.contains(x, y).then_some(Hit::NewTab)
    }

    /// The position a dragged tab would take if dropped at (x, y).
    pub fn drop_index(&self, x: i32, y: i32) -> Option<usize> {
        self.items.iter().position(|item| {
            let r = item.rect;
            if self.vertical && !item.pinned { y < r.bottom } else { r.contains(x, y) || (y < r.bottom && x < r.right) }
        })
    }
}

pub struct Paint<'a> {
    pub layout: &'a Layout,
    pub tabs: &'a [TabInfo<'a>],
    pub active: Option<u64>,
    pub palette: &'a Palette,
    pub hint: Option<&'a str>,
    pub scale: f32,
    /// Split pairs (left, right), drawn on one shared ground.
    pub groups: &'a [(u64, u64)],
    /// Extra flat areas over the content (the split divider and focus line).
    pub fills: &'a [(Rect, Rgb)],
    /// Initials of the pinned extensions, in `layout.extension_marks` order.
    pub extension_initials: &'a [char],
    pub motion: &'a Motion,
}

impl From<Rect> for RectF {
    fn from(r: Rect) -> Self {
        RectF::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
    }
}

/// Draws the strip and, when there is no page on screen, the empty page
/// with its hint.
pub fn paint(g: &Painter, width: i32, height: i32, p: &Paint) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);
    let vertical = p.layout.vertical;

    g.fill(RectF::new(0.0, 0.0, width as f32, height as f32), pal.chrome);
    // The page's ground: what an empty tab (or the space behind a page that
    // has not drawn yet) shows. The field floats on it, as on a page.
    g.fill(p.layout.content.into(), pal.field);

    // A pair in split view reads as one thing: one faint ground under both.
    for &(left, right) in p.groups {
        let find = |id: u64| p.layout.items.iter().find(|i| i.id == id).map(|i| p.motion.rect_of(i));
        let (Some(a), Some(b)) = (find(left), find(right)) else { continue };
        let shapes = if a.top == b.top || a.left == b.left {
            vec![RectF::new(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))]
        } else {
            vec![a, b]
        };
        for shape in shapes {
            g.round(shape.inset(-px(2.0), -px(2.0)), radius + px(2.0), pal.tab_tile);
        }
    }
    // The partner of the active tab is on screen too: its label reads as such.
    let shown_partner = p.active.and_then(|a| {
        p.groups.iter().find_map(|&(l, r)| if l == a { Some(r) } else if r == a { Some(l) } else { None })
    });

    // The live tab's ground first, on its own: it slides from the tab that
    // was live to the one that is now, under the titles.
    let tabs = order_like(p.layout, p.tabs);
    if let Some((item, tab)) = p.layout.items.iter().zip(&tabs).find(|(i, _)| p.active == Some(i.id)) {
        let ground = if tab.private {
            pal.private_tab
        } else if item.pinned {
            pal.pin_live
        } else {
            pal.tab_active
        };
        g.round(p.motion.live_rect(p.motion.rect_of(item)), radius, ground);
    }

    for (item, tab) in p.layout.items.iter().zip(&tabs) {
        let rect = p.motion.rect_of(item);
        let active = p.active == Some(item.id);
        let hover = p.motion.hover(HoverKey::Tab(item.id));
        if !active {
            if item.pinned {
                g.round(rect, radius, pal.tab_tile.mix(pal.tab_hover, hover));
            } else if hover > 0.0 {
                g.round(rect, radius, Color::from(pal.tab_hover).with_alpha(hover));
            }
        }
        let ink = if shown_partner == Some(item.id) || active {
            pal.ink
        } else if tab.asleep {
            pal.ink_asleep
        } else {
            pal.ink_muted.mix(pal.ink, hover * 0.6)
        };

        let mark = px(size::MARK);
        let (cx, cy) = rect.center();
        let mark_x = if item.pinned { cx - mark / 2.0 } else { rect.left + px(8.0) };
        let mark_rect = RectF::new(mark_x, cy - mark / 2.0, mark_x + mark, cy + mark / 2.0);
        if tab.loading {
            let r = px(5.5);
            let (mx, my) = mark_rect.center();
            g.arc(mx, my, r, 0.0, std::f32::consts::TAU - 0.01, px(1.5), Color::from(pal.ink_muted).with_alpha(0.25));
            g.arc(mx, my, r, p.motion.spin(), 1.9, px(1.5), pal.accent);
        } else {
            draw_mark(g, mark_rect, tab.initial, tab.private, tab.asleep && !active, pal, p.scale);
        }

        if !item.pinned && rect.width() >= px(56.0) {
            let mut text = RectF::new(mark_rect.right + px(8.0), rect.top, rect.right - px(8.0), rect.bottom);
            let close_shown = item.close.is_some() && (active || hover > 0.0);
            if close_shown {
                text.right -= px(18.0);
            }
            let weight = if active { size::MEDIUM } else { size::REGULAR };
            let text_size = if vertical { size::TEXT } else { size::TEXT_SMALL + 0.5 };
            g.text(tab.label, text, Font::new(px(text_size), weight, Align::Leading), ink);
            if let Some(close) = item.close.filter(|_| close_shown) {
                // The cross rides with its tab while the tab glides.
                let dx = rect.left - item.rect.left as f32;
                let dy = rect.top - item.rect.top as f32;
                let close = RectF::from(close);
                let close = RectF::new(close.left + dx, close.top + dy, close.right + dx, close.bottom + dy);
                let over = p.motion.hover(HoverKey::Close(item.id));
                if over > 0.0 {
                    let under = if active { pal.pin_live } else { pal.tab_active };
                    g.round(close, px(6.0), Color::from(under).with_alpha(over));
                }
                let strength = if active { 1.0 } else { hover };
                let ink = Color::from(pal.ink_muted.mix(pal.ink, over)).with_alpha(strength);
                draw_cross(g, close, px(3.5), ink, p.scale);
            }
        }
    }

    if p.layout.strip.width() > 0 {
        let plus: RectF = p.layout.new_tab.into();
        let over = p.motion.hover(HoverKey::NewTab);
        if over > 0.0 {
            g.round(plus, radius, Color::from(pal.tab_hover).with_alpha(over));
        }
        let ink = pal.ink_asleep.mix(pal.ink_muted, over);
        if vertical {
            let mark_x = plus.left + px(8.0);
            let (_, cy) = plus.center();
            draw_plus(g, RectF::new(mark_x, cy - px(8.0), mark_x + px(16.0), cy + px(8.0)), px(4.5), ink, p.scale);
            let text = RectF::new(mark_x + px(24.0), plus.top, plus.right - px(8.0), plus.bottom);
            g.text("Nueva pestaña", text, Font::new(px(size::TEXT), size::REGULAR, Align::Leading), ink);
        } else {
            draw_plus(g, plus, px(5.0), ink, p.scale);
        }

        let s = p.layout.strip;
        let line = if vertical {
            RectF::new(s.right as f32 - 1.0, 0.0, s.right as f32, s.bottom as f32)
        } else {
            RectF::new(0.0, s.bottom as f32 - 1.0, s.right as f32, s.bottom as f32)
        };
        g.fill(line, pal.line);
    }

    for &(rect, color) in p.fills {
        g.fill(rect.into(), color);
    }

    if p.layout.extensions.width() > 0 {
        let button: RectF = p.layout.extensions.into();
        let over = p.motion.hover(HoverKey::Extensions);
        if over > 0.0 {
            g.round(button, radius, Color::from(pal.tab_hover).with_alpha(over));
        }
        draw_puzzle(g, button, pal.ink_muted.mix(pal.ink, over), p.scale);
        for (i, (mark, initial)) in p.layout.extension_marks.iter().zip(p.extension_initials).enumerate() {
            let mark: RectF = (*mark).into();
            let over = p.motion.hover(HoverKey::PinnedExtension(i));
            if over > 0.0 {
                g.round(mark, radius, Color::from(pal.tab_hover).with_alpha(over));
            }
            let (cx, cy) = mark.center();
            let m = px(size::MARK);
            let square = RectF::new(cx - m / 2.0, cy - m / 2.0, cx + m / 2.0, cy + m / 2.0);
            draw_mark(g, square, *initial, false, false, pal, p.scale);
        }
    }

    if let Some(hint) = p.hint {
        let font = Font::new(px(size::TEXT), size::REGULAR, Align::Center);
        g.text(hint, p.layout.content.into(), font, pal.ink_asleep);
    }
}

/// A site's stand-in: its first letter on a small rounded square, as the
/// marks in Search's rows. Private tabs wear theirs in violet.
pub fn draw_mark(g: &Painter, rect: RectF, initial: char, private: bool, dim: bool, pal: &Palette, scale: f32) {
    let (tile, letter) = if private {
        (Color::from(pal.private_mark).with_alpha(0.18), Color::from(pal.private_mark))
    } else {
        (Color::from(pal.ink).with_alpha(if pal.dark { 0.10 } else { 0.07 }), Color::from(pal.ink_muted))
    };
    let alpha = if dim { 0.55 } else { 1.0 };
    g.round(rect, size::MARK_RADIUS * scale, tile.with_alpha(alpha));
    let mut buffer = [0u8; 4];
    let upper = initial.to_uppercase().next().unwrap_or(initial);
    let text: &str = upper.encode_utf8(&mut buffer);
    let font = Font::new(size::TEXT_MARK * scale, size::SEMIBOLD, Align::Center);
    g.text(text, rect, font, letter.with_alpha(alpha));
}

fn draw_cross(g: &Painter, rect: RectF, arm: f32, ink: Color, scale: f32) {
    let (cx, cy) = rect.center();
    let w = 1.25 * scale;
    g.line((cx - arm, cy - arm), (cx + arm, cy + arm), w, ink);
    g.line((cx - arm, cy + arm), (cx + arm, cy - arm), w, ink);
}

fn draw_plus(g: &Painter, rect: RectF, arm: f32, ink: Rgb, scale: f32) {
    let (cx, cy) = rect.center();
    let w = 1.4 * scale;
    g.line((cx - arm, cy), (cx + arm, cy), w, ink);
    g.line((cx, cy - arm), (cx, cy + arm), w, ink);
}

/// Items are laid out pinned-first in both orientations, as the tab list
/// keeps them; this pairs each item with its tab by id.
fn order_like<'a>(layout: &Layout, tabs: &'a [TabInfo<'a>]) -> Vec<&'a TabInfo<'a>> {
    layout.items.iter().filter_map(|item| tabs.iter().find(|t| t.id == item.id)).collect()
}

/// A puzzle piece, the usual mark for extensions, drawn as an outline:
/// a square with a knob on top and one on the right, about 14 px.
fn draw_puzzle(g: &Painter, rect: RectF, color: Rgb, scale: f32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    let s = |v: f32| v * scale;
    let (cx, cy) = rect.center();
    let (l, t, r, b) = (cx - s(6.0), cy - s(4.0), cx + s(4.0), cy + s(6.0));
    let w = s(1.3);
    let knob = s(2.0);
    let top_knob = cx - s(1.0);
    let side_knob = cy + s(1.0);
    g.polyline(&[(top_knob + knob, t), (r, t), (r, side_knob - knob)], w, color);
    g.arc(r, side_knob, knob, 0.0 - PI, PI, w, color);
    g.polyline(&[(r, side_knob + knob), (r, b), (l, b), (l, t), (top_knob - knob, t)], w, color);
    g.arc(top_knob, t, knob, -FRAC_PI_2, PI, w, color);
}

/// What a hover can rest on, for its fade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HoverKey {
    Tab(u64),
    Close(u64),
    NewTab,
    Extensions,
    PinnedExtension(usize),
}

impl HoverKey {
    fn of(hit: Hit) -> [Option<HoverKey>; 2] {
        match hit {
            Hit::Tab(id) => [Some(HoverKey::Tab(id)), None],
            Hit::Close(id) => [Some(HoverKey::Tab(id)), Some(HoverKey::Close(id))],
            Hit::NewTab => [Some(HoverKey::NewTab), None],
            Hit::Extensions => [Some(HoverKey::Extensions), None],
            Hit::PinnedExtension(i) => [Some(HoverKey::PinnedExtension(i)), None],
        }
    }
}

/// The strip's motion: hovers that fade, the live tab's ground that slides
/// to the newly chosen tab, tabs that glide to their place after a reorder
/// and the loading ring. All of it is worked out at paint time from what
/// changed since the last paint; `advance` says whether another frame is
/// wanted and how soon, so a timer runs only while something moves.
#[derive(Default)]
pub struct Motion {
    last: Option<Instant>,
    now: Option<Instant>,
    hovers: HashMap<HoverKey, f32>,
    /// The live tab's ground: from where, since when.
    live: Option<(RectF, Instant)>,
    live_drawn: Option<(u64, RectF)>,
    /// Tabs gliding to a new place after a reorder.
    moves: HashMap<u64, (RectF, Instant)>,
    drawn: HashMap<u64, RectF>,
    strip: Rect,
    spin: f32,
    settle: Duration,
    /// Windows' "Show animations in Windows" is off: everything lands at once.
    pub reduced: bool,
}

impl Motion {
    pub fn new(reduced: bool) -> Self {
        Self { reduced, ..Default::default() }
    }

    /// Brings the motion up to `now` for this layout. Returns the frame
    /// interval wanted next, in ms, or None when everything is at rest.
    pub fn advance(
        &mut self,
        layout: &Layout,
        active: Option<u64>,
        hover: Option<Hit>,
        loading: bool,
        now: Instant,
    ) -> Option<u32> {
        let dt = self.last.map_or(0.0, |l| now.duration_since(l).as_secs_f32());
        self.last = Some(now);
        self.now = Some(now);
        let quick = if self.reduced { 0.0 } else { motion::QUICK_MS as f32 / 1000.0 };
        self.settle = Duration::from_millis(if self.reduced { 0 } else { motion::SETTLE_MS });

        // Hovers fade toward 1 under the pointer and back to 0 elsewhere.
        let wanted = hover.map(HoverKey::of).unwrap_or([None, None]);
        let target = |key: &HoverKey| if wanted.contains(&Some(*key)) { 1.0 } else { 0.0 };
        for key in wanted.into_iter().flatten() {
            self.hovers.entry(key).or_insert(0.0);
        }
        // The first frame of a fade moves by one frame's worth, not by the
        // time since the last paint (which may be minutes ago).
        let step = if quick == 0.0 { 1.0 } else { dt.min(0.034) / quick };
        let step = if step <= 0.0 { motion::FRAME_MS as f32 / 1000.0 / quick.max(0.001) } else { step };
        self.hovers.retain(|key, value| {
            let goal = target(key);
            *value = if *value < goal { (*value + step).min(goal) } else { (*value - step).max(goal) };
            *value > 0.0 || goal > 0.0
        });
        let fading = self.hovers.iter().any(|(key, v)| *v != target(key));

        let same_strip = self.strip == layout.strip && self.strip.width() > 0;
        // The live ground slides when another tab becomes live in the same strip.
        let now_live =
            active.and_then(|id| layout.items.iter().find(|i| i.id == id)).map(|i| (i.id, RectF::from(i.rect)));
        match (self.live_drawn, now_live) {
            (Some((was, from)), Some((id, _))) if was != id && same_strip => self.live = Some((from, now)),
            (_, None) => self.live = None,
            _ => {}
        }
        // Tabs that moved while the strip kept its size and its set of tabs
        // (a reorder, not a resize) glide from where they were drawn.
        let same_set =
            self.drawn.len() == layout.items.len() && layout.items.iter().all(|i| self.drawn.contains_key(&i.id));
        if same_strip && same_set {
            for item in &layout.items {
                let from = self.drawn[&item.id];
                let to = RectF::from(item.rect);
                let resized = (from.width() - to.width()).abs() > 0.5 || (from.height() - to.height()).abs() > 0.5;
                if !resized && from != to && !self.moves.contains_key(&item.id) {
                    self.moves.insert(item.id, (from, now));
                }
            }
        } else {
            self.moves.clear();
        }
        let settle = self.settle;
        self.moves.retain(|_, (_, start)| now.duration_since(*start) < settle);
        if self.live.is_some_and(|(_, start)| now.duration_since(start) >= settle) {
            self.live = None;
        }
        self.strip = layout.strip;
        self.drawn = layout.items.iter().map(|i| (i.id, self.rect_of(i))).collect();
        self.live_drawn = now_live.map(|(id, _)| {
            let item = layout.items.iter().find(|i| i.id == id).expect("live item is in the layout");
            (id, self.live_rect(self.rect_of(item)))
        });

        if loading && !self.reduced {
            self.spin = (self.spin + dt.min(0.1) * std::f32::consts::TAU / 0.9) % std::f32::consts::TAU;
        }
        if fading || self.live.is_some() || !self.moves.is_empty() {
            Some(motion::FRAME_MS)
        } else if loading && !self.reduced {
            Some(motion::SPIN_FRAME_MS)
        } else {
            None
        }
    }

    pub fn hover(&self, key: HoverKey) -> f32 {
        motion::ease_out(self.hovers.get(&key).copied().unwrap_or(0.0))
    }

    fn progress(&self, start: Instant) -> f32 {
        let Some(now) = self.now else { return 1.0 };
        if self.settle.is_zero() {
            return 1.0;
        }
        motion::ease_out(now.duration_since(start).as_secs_f32() / self.settle.as_secs_f32())
    }

    /// Where an item is drawn this frame.
    pub fn rect_of(&self, item: &Item) -> RectF {
        let to = RectF::from(item.rect);
        match self.moves.get(&item.id) {
            Some((from, start)) => from.lerp(&to, self.progress(*start)),
            None => to,
        }
    }

    /// Where the live tab's ground is drawn this frame, heading to `to`.
    pub fn live_rect(&self, to: RectF) -> RectF {
        match self.live {
            Some((from, start)) => from.lerp(&to, self.progress(start)),
            None => to,
        }
    }

    pub fn spin(&self) -> f32 {
        self.spin
    }
}

