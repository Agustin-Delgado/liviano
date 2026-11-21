//! The tab strip: where each tab sits (pure geometry, tested), how it is
//! drawn (Direct2D, through `gfx`) and how it moves (`Motion`).

use std::{collections::HashMap, time::Instant};

use windows::Win32::Foundation::RECT;

use crate::{
    anim::{Levels, Tween, specs},
    design::{Palette, Rgb, group_tone, motion, size},
    frame::{self, Frame, WindowButton},
    gfx::{Align, Color, Font, Painter, RectF},
    tab_groups::{Drop, GroupColor},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(u64),
    Close(u64),
    /// A tab group's label (top strip) or header (column).
    Group(u64),
    NewTab,
    /// The extensions button (the puzzle piece).
    Extensions,
    /// A pinned extension next to it, by its place among the pinned ones.
    PinnedExtension(usize),
    /// Minimize, maximize or close: the window's own buttons.
    Window(WindowButton),
    /// The gear: Ajustes, always there.
    Settings,
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
    pub fn height(&self) -> i32 {
        self.bottom - self.top
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
    /// The site's icon, when one is known; the initial stands in otherwise.
    pub icon: Option<std::rc::Rc<crate::favicon::Icon>>,
    /// Its tab group, if any (see `GroupInfo`).
    pub group: Option<u64>,
}

/// What the strip needs to know about one tab group.
pub struct GroupInfo<'a> {
    pub id: u64,
    pub name: &'a str,
    pub color: GroupColor,
    /// Folded: only its label shows.
    pub collapsed: bool,
    /// How many tabs it has.
    pub count: usize,
    /// The label's width on a top strip, in pixels (`label_width`).
    pub label_w: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: u64,
    pub rect: Rect,
    pub close: Option<Rect>,
    pub pinned: bool,
    pub group: Option<u64>,
    /// In a folded group: its rect is folded to nothing at the label's end
    /// (so folding and unfolding animate as a resize) and it is not hit.
    pub hidden: bool,
}

/// A tab group's label: before its tabs on a top strip, a row over them in
/// the column.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub id: u64,
    pub rect: Rect,
    pub collapsed: bool,
}

/// The label of a top strip: its name with room either side, capped, or
/// the coloured dot alone. `text_w` is the name measured in the label's
/// font (0 for an unnamed group).
pub fn label_width(text_w: f32, scale: f32) -> i32 {
    let m = Metrics { scale };
    if text_w <= 0.0 { m.px(LABEL_DOT_W) } else { (text_w.ceil() as i32 + m.px(2 * LABEL_PAD)).min(m.px(LABEL_MAX_W)) }
}

/// The label's font: the strip's small step, medium.
pub fn label_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::MEDIUM, Align::Center)
}

const LABEL_DOT_W: i32 = 22;
const LABEL_PAD: i32 = 8;
const LABEL_MAX_W: i32 = 140;
/// The coloured pill inside a label, and the dot of an unnamed one.
const PILL_H: f32 = 20.0;
const DOT: f32 = 10.0;
/// How far a group's tabs sit in from the column's edge, under its header.
const GROUP_INDENT: i32 = 10;
/// Air between a group and what is next to it in the column.
const GROUP_AIR: i32 = 4;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The whole strip (top row or left column). Empty when collapsed.
    pub strip: Rect,
    /// Where the page goes.
    pub content: Rect,
    pub items: Vec<Item>,
    /// One per tab group, in strip order.
    pub labels: Vec<Label>,
    pub new_tab: Rect,
    pub vertical: bool,
    /// The extensions button; empty when no extension is installed.
    pub extensions: Rect,
    /// The pinned extensions beside it, in order.
    pub extension_marks: Vec<Rect>,
    /// The gear that opens Ajustes: next to the window's buttons on a top
    /// strip and on the caption row, at the foot of the column.
    pub settings: Rect,
    /// The window's caption band and buttons (there is no title bar).
    pub frame: Frame,
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
#[allow(clippy::too_many_arguments)]
pub fn layout_with(
    tabs: &[TabInfo],
    groups: &[GroupInfo],
    width: i32,
    height: i32,
    vertical: bool,
    collapsed: bool,
    m: &Metrics,
    extensions: Option<usize>,
) -> Layout {
    if collapsed {
        // No tabs, but the window still needs its buttons and somewhere to
        // be dragged from: a slim caption row over the page.
        let row = m.px(frame::ROW);
        let caption = Rect::new(0, 0, width, row);
        let frame = Frame::new(caption, width, m.scale);
        let right = frame.buttons[0].left - m.px(4);
        let settings = Rect::new(right - m.px(SETTINGS_W), m.px(2), right, row - m.px(2));
        return Layout { content: Rect::new(0, row, width, height.max(row)), vertical, frame, settings, ..Default::default() };
    }
    let mut layout = if vertical {
        layout_vertical(tabs, groups, width, height, m)
    } else {
        layout_horizontal(tabs, groups, width, height, m, extensions)
    };
    if let Some(pinned) = extensions {
        place_extensions(&mut layout, pinned, m);
    }
    layout
}

/// The gear's width (the extensions button's).
const SETTINGS_W: i32 = 30;

/// Room always left free between the last tab (or +) and what sits at the
/// right of the strip, so the window can be grabbed however many tabs there are.
const DRAG_ROOM: i32 = 32;

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
        let room = layout.settings.left - gap - layout.extensions.right;
        let mut x = layout.extensions.right + gap;
        for _ in 0..shown_marks(pinned, room, m) {
            layout.extension_marks.push(Rect::new(x, top, x + mark_w, top + row));
            x += mark_w + gap;
        }
    } else {
        let (top, bottom) = (m.px(6), strip.bottom - m.px(6));
        let right = layout.settings.left - gap;
        layout.extensions = Rect::new(right - button_w, top, right, bottom);
        let mut x = layout.extensions.left - gap;
        for _ in 0..shown_marks(pinned, strip.width() / 4, m) {
            layout.extension_marks.insert(0, Rect::new(x - mark_w, top, x, bottom));
            x -= mark_w + gap;
        }
    }
}

/// The group a tab is drawn in: none for a pinned tab or an unknown group.
fn group_of<'a>(tab: &TabInfo, groups: &'a [GroupInfo<'a>]) -> Option<&'a GroupInfo<'a>> {
    if tab.pinned {
        return None;
    }
    tab.group.and_then(|g| groups.iter().find(|i| i.id == g))
}

fn layout_horizontal(
    tabs: &[TabInfo],
    groups: &[GroupInfo],
    width: i32,
    height: i32,
    m: &Metrics,
    extensions: Option<usize>,
) -> Layout {
    let bar = m.px(BAR);
    let pad = m.px(8);
    let gap = m.px(2);
    let pinned_w = m.px(30);
    let plus_w = m.px(30);
    let item_top = m.px(6);
    let item_bottom = bar - m.px(6);

    let pinned = tabs.iter().filter(|t| t.pinned).count() as i32;
    let folded = |t: &TabInfo| group_of(t, groups).is_some_and(|g| g.collapsed);
    let normal = tabs.iter().filter(|t| !t.pinned && !folded(t)).count() as i32;
    // Each group's label, where its first tab is.
    let mut labels_w = 0;
    let mut previous = None;
    for tab in tabs {
        let group = group_of(tab, groups);
        if group.map(|g| g.id) != previous {
            labels_w += group.map_or(0, |g| g.label_w + gap);
            previous = group.map(|g| g.id);
        }
    }
    let frame = Frame::new(Rect::new(0, 0, width, bar), width, m.scale);
    let settings_right = frame.buttons[0].left - m.px(4);
    let settings = Rect::new(settings_right - m.px(SETTINGS_W), item_top, settings_right, item_bottom);
    let reserved = extensions.map_or(0, |n| extensions_width(n, width, m))
        + frame.width()
        + m.px(4 + SETTINGS_W + 2)
        + m.px(DRAG_ROOM);
    let available = width - 2 * pad - plus_w - pinned * (pinned_w + gap) - normal * gap - labels_w - reserved;
    let normal_w = if normal > 0 { (available / normal).clamp(m.px(44), m.px(200)) } else { 0 };

    let mut x = pad;
    let mut items = Vec::with_capacity(tabs.len());
    let mut labels = Vec::new();
    let mut previous = None;
    for tab in tabs {
        let group = group_of(tab, groups);
        let id = group.map(|g| g.id);
        if id != previous {
            if let Some(g) = group {
                labels.push(Label { id: g.id, rect: Rect::new(x, item_top, x + g.label_w, item_bottom), collapsed: g.collapsed });
                x += g.label_w + gap;
            }
            previous = id;
        }
        if group.is_some_and(|g| g.collapsed) {
            let rect = Rect::new(x, item_top, x, item_bottom);
            items.push(Item { id: tab.id, rect, close: None, pinned: false, group: id, hidden: true });
            continue;
        }
        let w = if tab.pinned { pinned_w } else { normal_w };
        let rect = Rect::new(x, item_top, x + w, item_bottom);
        let close = (!tab.pinned && w >= m.px(80)).then(|| {
            let size = m.px(20);
            let cy = (rect.top + rect.bottom) / 2;
            Rect::new(rect.right - m.px(4) - size, cy - size / 2, rect.right - m.px(4), cy + size / 2)
        });
        items.push(Item { id: tab.id, rect, close, pinned: tab.pinned, group: id, hidden: false });
        x += w + gap;
    }
    let new_tab = Rect::new(x, item_top, (x + plus_w).min(width), item_bottom);
    Layout {
        strip: Rect::new(0, 0, width, bar),
        content: Rect::new(0, bar, width, height.max(bar)),
        items,
        labels,
        new_tab,
        vertical: false,
        frame,
        settings,
        ..Default::default()
    }
}

fn layout_vertical(tabs: &[TabInfo], groups: &[GroupInfo], width: i32, height: i32, m: &Metrics) -> Layout {
    let side = m.px(232).min(width / 2);
    let pad = m.px(8);
    let row = m.px(32);
    let gap = m.px(2);
    let cell = m.px(40);

    let mut items = Vec::with_capacity(tabs.len());
    // The top row is the caption: the window's buttons at its right, the
    // rest of it moves the window (as the traffic lights above Search's column).
    let caption = Rect::new(0, 0, side, m.px(BAR));
    let frame = Frame::new(caption, side - 1, m.scale);
    // Pinned tabs: a grid of squares under it.
    let per_row = ((side - 2 * pad + gap) / (cell + gap)).max(1);
    let start = caption.bottom + m.px(2);
    let mut y = start;
    let mut col = 0;
    for tab in tabs.iter().filter(|t| t.pinned) {
        let x = pad + col * (cell + gap);
        items.push(Item {
            id: tab.id,
            rect: Rect::new(x, y, x + cell, y + cell),
            close: None,
            pinned: true,
            group: None,
            hidden: false,
        });
        col += 1;
        if col == per_row {
            col = 0;
            y += cell + gap;
        }
    }
    if col > 0 {
        y += cell + gap;
    }
    if y > start {
        y += m.px(6);
    }
    // Rows; a group is a header with its tabs under it, set in a little
    // (Edge's vertical tabs), with some air around it.
    let mut labels = Vec::new();
    let mut previous = None;
    let mut first = true;
    for tab in tabs.iter().filter(|t| !t.pinned) {
        let group = group_of(tab, groups);
        let id = group.map(|g| g.id);
        if id != previous {
            if !first {
                y += m.px(GROUP_AIR);
            }
            if let Some(g) = group {
                labels.push(Label { id: g.id, rect: Rect::new(pad, y, side - pad, y + row), collapsed: g.collapsed });
                y += row + gap;
            }
            previous = id;
        }
        first = false;
        let left = if group.is_some() { pad + m.px(GROUP_INDENT) } else { pad };
        if group.is_some_and(|g| g.collapsed) {
            let rect = Rect::new(left, y, side - pad, y);
            items.push(Item { id: tab.id, rect, close: None, pinned: false, group: id, hidden: true });
            continue;
        }
        let rect = Rect::new(left, y, side - pad, y + row);
        let size = m.px(20);
        let cy = (rect.top + rect.bottom) / 2;
        let close = Some(Rect::new(rect.right - m.px(6) - size, cy - size / 2, rect.right - m.px(6), cy + size / 2));
        items.push(Item { id: tab.id, rect, close, pinned: false, group: id, hidden: false });
        y += row + gap;
    }
    if previous.is_some() {
        y += m.px(GROUP_AIR);
    }
    let new_tab = Rect::new(pad, y, side - pad, y + row);
    let foot = height - pad - row;
    let settings = Rect::new(side - pad - m.px(SETTINGS_W), foot, side - pad, foot + row);
    Layout {
        settings,
        strip: Rect::new(0, 0, side, height),
        content: Rect::new(side, 0, width.max(side), height),
        items,
        labels,
        new_tab,
        vertical: true,
        frame,
        ..Default::default()
    }
}

impl Layout {
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some(button) = self.frame.button_at(x, y) {
            return Some(Hit::Window(button));
        }
        if self.settings.contains(x, y) {
            return Some(Hit::Settings);
        }
        if !self.strip.contains(x, y) {
            return None;
        }
        if self.extensions.contains(x, y) {
            return Some(Hit::Extensions);
        }
        if let Some(i) = self.extension_marks.iter().position(|r| r.contains(x, y)) {
            return Some(Hit::PinnedExtension(i));
        }
        if let Some(label) = self.labels.iter().find(|l| l.rect.contains(x, y)) {
            return Some(Hit::Group(label.id));
        }
        for item in self.items.iter().filter(|i| !i.hidden) {
            if item.rect.contains(x, y) {
                if item.close.is_some_and(|c| c.contains(x, y)) {
                    return Some(Hit::Close(item.id));
                }
                return Some(Hit::Tab(item.id));
            }
        }
        self.new_tab.contains(x, y).then_some(Hit::NewTab)
    }

    /// Where a tab dragged to (x, y) would land: in the place of the tab
    /// there (taking its group), right before a group when over its label,
    /// or at the end, out of any group, past the last tab.
    pub fn drop_at(&self, x: i32, y: i32) -> Option<Drop> {
        if let Some(label) = self.labels.iter().find(|l| l.rect.contains(x, y)) {
            return Some(Drop::BeforeGroup(label.id));
        }
        let over = self.items.iter().position(|item| {
            let r = item.rect;
            if item.hidden {
                false
            } else if self.vertical && !item.pinned {
                y < r.bottom
            } else {
                r.contains(x, y) || (y < r.bottom && x < r.right)
            }
        });
        if let Some(index) = over {
            return Some(Drop::Tab(index));
        }
        let last = self.items.iter().rev().find(|i| !i.hidden)?;
        let past = if self.vertical { y >= last.rect.bottom } else { x >= last.rect.right };
        (past && self.strip.contains(x, y)).then_some(Drop::End)
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
    pub pairs: &'a [(u64, u64)],
    /// Tab groups: their labels and colours.
    pub tab_groups: &'a [GroupInfo<'a>],
    /// Extra flat areas over the content (the split divider and focus line).
    pub fills: &'a [(Rect, Rgb)],
    /// Initials of the pinned extensions, in `layout.extension_marks` order.
    pub extension_initials: &'a [char],
    pub motion: &'a Motion,
    /// The window is maximized: the middle button draws "restore".
    pub maximized: bool,
    /// A window button held down.
    pub pressed: Option<WindowButton>,
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

    g.fill(RectF::new(0.0, 0.0, width as f32, height as f32), pal.chrome);
    // The page's ground: what an empty tab (or the space behind a page that
    // has not drawn yet) shows. The field floats on it, as on a page.
    g.fill(p.layout.content.into(), pal.field);

    // The column coming back slides in from the window's edge.
    let dx = -(1.0 - p.motion.rail()) * p.layout.strip.width() as f32;
    g.offset(dx, 0.0, |g| paint_strip(g, p));

    // Over the page: the split's divider, and its focus line sliding to
    // the page that has the keyboard (as the tabs' indicator does).
    for &(rect, color) in p.fills {
        if color == pal.accent {
            g.fill(p.motion.indicator_rect(rect), color);
        } else {
            g.fill(rect.into(), color);
        }
    }

    if let Some(hint) = p.hint {
        let font = Font::new(px(size::TEXT), size::REGULAR, Align::Center);
        g.text(hint, p.layout.content.into(), font, pal.ink_asleep);
    }
}

fn paint_strip(g: &Painter, p: &Paint) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);

    // A pair in split view reads as one thing: one faint ground under both.
    for &(left, right) in p.pairs {
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
        p.pairs.iter().find_map(|&(l, r)| if l == a { Some(r) } else if r == a { Some(l) } else { None })
    });

    paint_group_lines(g, p);

    // The live tab's card first, on its own: it slides from the tab that
    // was live to the one that is now, under the titles.
    let tabs = order_like(p.layout, p.tabs);
    if let Some((item, tab)) = p.layout.items.iter().zip(&tabs).find(|(i, _)| p.active == Some(i.id)) {
        let ground = if tab.private { pal.private_tab } else { pal.tab_active };
        let opacity = p.motion.opacity_of(item.id);
        let rect = p.motion.live_rect(p.motion.rect_of(item));
        g.card(rect, radius, Color::from(ground).with_alpha(opacity), pal.dark, opacity, p.scale);
    }

    for (item, tab) in p.layout.items.iter().zip(&tabs) {
        let rect = p.motion.rect_of(item);
        // Folded into its group's label, or on its way there: cut to what
        // is left of it.
        if rect.width() < 1.0 || rect.height() < 1.0 {
            continue;
        }
        let folding = item.hidden || (rect.width() - item.rect.width() as f32).abs() > 0.5 || (rect.height() - item.rect.height() as f32).abs() > 0.5;
        if folding && item.group.is_some() {
            // In the column the row keeps its height under the shrinking
            // clip, so its title slides away instead of squashing.
            let whole = if p.layout.vertical { RectF::new(rect.left, rect.top, rect.right, rect.top + 32.0 * p.scale) } else { rect };
            g.clip(rect, |g| paint_tab(g, p, item, tab, whole, shown_partner));
        } else {
            paint_tab(g, p, item, tab, rect, shown_partner);
        }
    }
    paint_labels(g, p);

    paint_controls(g, p);
}

/// One tab: its ground, its mark or icon, its title and its cross.
fn paint_tab(g: &Painter, p: &Paint, item: &Item, tab: &TabInfo, rect: RectF, shown_partner: Option<u64>) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);
    let vertical = p.layout.vertical;
    {
        let active = p.active == Some(item.id);
        let hover = p.motion.hover(HoverKey::Tab(item.id));
        let opacity = p.motion.opacity_of(item.id);
        if !active {
            if item.pinned {
                g.round(rect, radius, Color::from(pal.tab_tile.mix(pal.tab_hover, hover)).with_alpha(opacity));
            } else if hover > 0.0 {
                g.round(rect, radius, Color::from(pal.tab_hover).with_alpha(hover));
            }
        }
        let ink = if shown_partner == Some(item.id) || active {
            pal.ink
        } else if tab.asleep {
            pal.ink_asleep
        } else {
            // A row of the column reads as a sidebar item (ink-quiet).
            let rest = if vertical { pal.ink_quiet } else { pal.ink_muted };
            rest.mix(pal.ink, hover)
        };
        let ink = Color::from(ink).with_alpha(opacity);

        let mark = px(size::MARK);
        let (cx, cy) = rect.center();
        let mark_x = if item.pinned { cx - mark / 2.0 } else { rect.left + px(8.0) };
        let mark_rect = RectF::new(mark_x, cy - mark / 2.0, mark_x + mark, cy + mark / 2.0);
        if tab.loading {
            let r = px(5.5);
            let (mx, my) = mark_rect.center();
            g.arc(mx, my, r, 0.0, std::f32::consts::TAU - 0.01, px(1.5), Color::from(pal.ink_muted).with_alpha(0.25));
            g.arc(mx, my, r, p.motion.spin(), 1.9, px(1.5), pal.accent);
        } else if let Some(icon) = &tab.icon {
            let opacity = opacity * if tab.asleep && !active { 0.55 } else { 1.0 };
            let px = &icon.pixels;
            g.image(&px.data, px.width, px.height, snap(mark_rect), opacity);
        } else {
            draw_mark(g, mark_rect, tab.initial, tab.private, tab.asleep && !active, pal, p.scale);
        }

        if !item.pinned && rect.width() >= px(56.0) {
            let mut text = RectF::new(mark_rect.right + px(8.0), rect.top, rect.right - px(8.0), rect.bottom);
            let close_shown = item.close.is_some() && (active || hover > 0.0);
            if close_shown {
                text.right -= px(18.0);
            }
            // One weight for every tab: a heavier active title is wider and
            // shifts as the selection moves. The ink alone says which is live.
            let text_size = if vertical { size::TEXT } else { size::TEXT_SMALL };
            g.text(tab.label, text, Font::new(px(text_size), size::REGULAR, Align::Leading), ink);
            if let Some(close) = item.close.filter(|_| close_shown) {
                // The cross rides with its tab while the tab glides.
                let dx = rect.left - item.rect.left as f32;
                let dy = rect.top - item.rect.top as f32;
                let close = RectF::from(close);
                let close = RectF::new(close.left + dx, close.top + dy, close.right + dx, close.bottom + dy);
                let over = p.motion.hover(HoverKey::Close(item.id));
                if over > 0.0 {
                    let under = if active { pal.tab_hover } else { pal.pin_live };
                    g.round(close, px(size::RADIUS_CONTROL), Color::from(under).with_alpha(over));
                }
                let strength = if active { 1.0 } else { hover };
                let ink = Color::from(pal.ink_muted.mix(pal.ink, over)).with_alpha(strength);
                draw_cross(g, close, px(3.5), ink, p.scale);
            }
        }
    }
}

/// What joins a group's tabs: a line under them on a top strip (from the
/// label to the last tab, as Chrome's), a bar down their side in the
/// column (Edge's). It shortens with the tabs as the group folds.
fn paint_group_lines(g: &Painter, p: &Paint) {
    let px = |v: f32| v * p.scale;
    for label in &p.layout.labels {
        let Some(info) = p.tab_groups.iter().find(|i| i.id == label.id) else { continue };
        let fill = Color::from(group_tone(info.color, p.palette.dark).fill).with_alpha(p.motion.opacity_of(label.id));
        let at = p.motion.rect_of_label(label);
        let members = p.layout.items.iter().filter(|i| i.group == Some(label.id)).map(|i| p.motion.rect_of(i));
        if p.layout.vertical {
            let bottom = members.map(|r| r.bottom).fold(at.bottom, f32::max);
            if bottom - at.bottom > px(4.0) {
                let x = at.left + px(5.0);
                g.round(RectF::new(x, at.bottom + px(1.0), x + px(2.0), bottom - px(2.0)), px(1.0), fill);
            }
        } else {
            let right = members.map(|r| r.right).fold(at.right, f32::max);
            if right - at.right > px(1.0) {
                let top = at.bottom + px(2.0);
                g.round(RectF::new(at.left + px(2.0), top, right - px(2.0), top + px(2.0)), px(1.0), fill);
            }
        }
    }
}

/// Each group's label: a pill of its colour with its name (or a dot when
/// it has none); in the column, a header row with a chevron that says
/// whether it is folded and, folded or unnamed, how many tabs it holds.
fn paint_labels(g: &Painter, p: &Paint) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);
    for label in &p.layout.labels {
        let Some(info) = p.tab_groups.iter().find(|i| i.id == label.id) else { continue };
        let tone = group_tone(info.color, pal.dark);
        let rect = p.motion.rect_of_label(label);
        if rect.width() < 1.0 {
            continue;
        }
        let opacity = p.motion.opacity_of(label.id);
        let over = p.motion.hover(HoverKey::Group(label.id));
        ghost_ground(g, rect, radius, over, 0.0, pal);
        let fill = Color::from(tone.fill).with_alpha(opacity);
        let on = Color::from(tone.on).with_alpha(opacity);
        let (_, cy) = rect.center();
        let h = px(PILL_H);
        let dot = px(DOT);
        let named = !info.name.trim().is_empty();
        if p.layout.vertical {
            let left = rect.left + px(4.0);
            let chevron_x = rect.right - px(14.0);
            let room = chevron_x - px(12.0);
            let mut after = left;
            if named {
                let pill = RectF::new(left, cy - h / 2.0, (left + info.label_w as f32).min(room), cy + h / 2.0);
                g.round(pill, px(size::RADIUS_CONTROL), fill);
                g.text(info.name, pill.inset(px(LABEL_PAD as f32), 0.0), label_font(p.scale), on);
                after = pill.right;
            } else {
                g.round(RectF::new(left + px(2.0), cy - dot / 2.0, left + px(2.0) + dot, cy + dot / 2.0), dot / 2.0, fill);
                after = after + px(2.0) + dot;
            }
            if label.collapsed || !named {
                let count = if info.count == 1 { "1 pestaña".to_string() } else { format!("{} pestañas", info.count) };
                let text = RectF::new(after + px(8.0), rect.top, room, rect.bottom);
                let ink = Color::from(pal.ink_muted.mix(pal.ink, over)).with_alpha(opacity);
                g.text(&count, text, Font::new(px(size::TEXT_SMALL), size::REGULAR, Align::Leading), ink);
            }
            let ink = Color::from(pal.ink_muted.mix(pal.ink, over)).with_alpha(opacity);
            draw_chevron(g, (chevron_x, cy), label.collapsed, ink, p.scale);
        } else if named {
            let pill = RectF::new(rect.left, cy - h / 2.0, rect.right, cy + h / 2.0);
            g.round(pill, px(size::RADIUS_CONTROL), fill);
            if pill.width() > px(2.0 * LABEL_PAD as f32) {
                g.text(info.name, pill.inset(px(LABEL_PAD as f32), 0.0), label_font(p.scale), on);
            }
        } else {
            let (cx, _) = rect.center();
            g.round(RectF::new(cx - dot / 2.0, cy - dot / 2.0, cx + dot / 2.0, cy + dot / 2.0), dot / 2.0, fill);
        }
    }
}

/// A small chevron: pointing down while the group is open, right when folded.
fn draw_chevron(g: &Painter, (cx, cy): (f32, f32), folded: bool, ink: Color, scale: f32) {
    let s = |v: f32| v * scale;
    let points = if folded {
        [(cx - s(1.5), cy - s(3.5)), (cx + s(2.0), cy), (cx - s(1.5), cy + s(3.5))]
    } else {
        [(cx - s(3.5), cy - s(1.5)), (cx, cy + s(2.0)), (cx + s(3.5), cy - s(1.5))]
    };
    g.polyline(&points, s(1.4), ink);
}

/// The + button, the strip's edge line, the extensions, the gear and the
/// window's buttons.
fn paint_controls(g: &Painter, p: &Paint) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);
    let vertical = p.layout.vertical;

    if p.layout.strip.width() > 0 {
        let plus = p.motion.rect_of_new_tab(p.layout);
        let over = p.motion.hover(HoverKey::NewTab);
        ghost_ground(g, plus, radius, over, p.motion.pressed(HoverKey::NewTab), pal);
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

    if p.layout.extensions.width() > 0 {
        let button: RectF = p.layout.extensions.into();
        let over = p.motion.hover(HoverKey::Extensions);
        ghost_ground(g, button, radius, over, p.motion.pressed(HoverKey::Extensions), pal);
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

    // Collapsed: the caption row stands alone over the page, with its line.
    if p.layout.strip.width() == 0 && p.layout.frame.caption.width() > 0 {
        let c = p.layout.frame.caption;
        g.fill(RectF::new(0.0, c.bottom as f32 - 1.0, c.right as f32, c.bottom as f32), pal.line);
    }
    if p.layout.settings.width() > 0 {
        let gear: RectF = p.layout.settings.into();
        let over = p.motion.hover(HoverKey::Settings);
        ghost_ground(g, gear, radius, over, p.motion.pressed(HoverKey::Settings), pal);
        draw_gear(g, gear, pal.ink_muted.mix(pal.ink, over), p.scale);
    }
    frame::paint(g, &p.layout.frame, pal, p.scale, p.maximized, p.pressed, |b| p.motion.hover(HoverKey::Window(b)));
}

/// A ghost button's ground (HKUI's ghost variant): nothing at rest, the
/// hover grey under the pointer, a step deeper while a press is answered.
fn ghost_ground(g: &Painter, rect: RectF, radius: f32, over: f32, pressed: f32, pal: &Palette) {
    if over > 0.0 || pressed > 0.0 {
        let fill = pal.tab_hover.mix(pal.pin_live, pressed);
        g.round(rect, radius, Color::from(fill).with_alpha(over.max(pressed)));
    }
}

/// A gear, about 16 px: a ring with eight teeth around a hub, in outline
/// like the puzzle piece beside it.
pub fn draw_gear(g: &Painter, rect: RectF, color: Rgb, scale: f32) {
    let s = |v: f32| v * scale;
    let (cx, cy) = rect.center();
    let w = s(1.3);
    g.arc(cx, cy, s(4.6), 0.0, std::f32::consts::TAU - 0.001, w, color);
    g.arc(cx, cy, s(1.9), 0.0, std::f32::consts::TAU - 0.001, w, color);
    for i in 0..8 {
        let (sin, cos) = (i as f32 * std::f32::consts::FRAC_PI_4).sin_cos();
        g.line((cx + s(5.0) * sin, cy - s(5.0) * cos), (cx + s(6.6) * sin, cy - s(6.6) * cos), s(2.1), color);
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
    let font = Font::new(size::TEXT_MARK * scale, size::MEDIUM, Align::Center);
    g.text(text, rect, font, letter.with_alpha(alpha));
}

/// A rectangle on whole pixels: an icon drawn at its own size stays sharp.
fn snap(r: RectF) -> RectF {
    let (l, t) = (r.left.round(), r.top.round());
    RectF::new(l, t, l + r.width().round(), t + r.height().round())
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
    Group(u64),
    NewTab,
    Extensions,
    PinnedExtension(usize),
    Window(WindowButton),
    Settings,
}

impl HoverKey {
    fn of(hit: Hit) -> [Option<HoverKey>; 2] {
        match hit {
            Hit::Tab(id) => [Some(HoverKey::Tab(id)), None],
            Hit::Close(id) => [Some(HoverKey::Tab(id)), Some(HoverKey::Close(id))],
            Hit::Group(id) => [Some(HoverKey::Group(id)), None],
            Hit::NewTab => [Some(HoverKey::NewTab), None],
            Hit::Extensions => [Some(HoverKey::Extensions), None],
            Hit::PinnedExtension(i) => [Some(HoverKey::PinnedExtension(i)), None],
            Hit::Window(b) => [Some(HoverKey::Window(b)), None],
            Hit::Settings => [Some(HoverKey::Settings), None],
        }
    }
}

/// The strip's motion, worked out at paint time from what changed since the
/// last paint: hovers and presses that fade (a button's 150 ms ease-out),
/// the live tab's card sliding to the newly chosen tab (the tabs'
/// indicator), tabs gliding to their places (follow-shift) and growing or
/// shrinking when one opens, closes or is pinned (morph), a new tab
/// unfolding from nothing, the column sliding in from the edge when it is
/// shown again (the rail), the split's focus line moving between pages and
/// the loading ring. `advance` says whether another frame is wanted and how
/// soon, so the frame clock runs only while something moves. Whatever is
/// moving when its target changes heads to the new one from where it is.
#[derive(Default)]
pub struct Motion {
    last: Option<Instant>,
    now: Option<Instant>,
    hovers: Levels<HoverKey>,
    presses: Levels<HoverKey>,
    /// The live tab's card: where it set off from, and how far along.
    live: Option<(RectF, Tween<f32>)>,
    live_id: Option<u64>,
    live_drawn: Option<RectF>,
    /// Tabs and group labels (their ids never clash) gliding or resizing
    /// toward their place in the layout.
    moves: HashMap<u64, Tween<RectF>>,
    /// The tabs folded into a group's label in the last layout.
    hidden: Vec<u64>,
    /// Tabs that just opened, fading in as they unfold.
    appear: HashMap<u64, Tween<f32>>,
    targets: HashMap<u64, RectF>,
    drawn: HashMap<u64, RectF>,
    strip: Rect,
    /// The column (or the strip) coming back in from its edge, 0..1.
    rail: Option<Tween<f32>>,
    /// The split's focus line: its target and its slide.
    indicator: Option<Rect>,
    indicator_prev: Option<Rect>,
    indicator_slide: Option<(RectF, Tween<f32>)>,
    indicator_drawn: Option<RectF>,
    spin: f32,
    /// Windows' "Show animations in Windows" is off: the ring stands still
    /// (the rest lands at once through `anim::reduced`).
    pub reduced: bool,
}

/// Everything of the strip that glides: tabs, then group labels, with
/// whether each is folded away.
fn placed(layout: &Layout) -> impl Iterator<Item = (u64, Rect, bool)> + '_ {
    layout
        .items
        .iter()
        .map(|i| (i.id, i.rect, i.hidden))
        .chain(layout.labels.iter().map(|l| (l.id, l.rect, false)))
        .chain(std::iter::once((NEW_TAB_KEY, layout.new_tab, false)))
}

/// The + button's key among the moves: no tab or group ever gets it.
const NEW_TAB_KEY: u64 = u64::MAX;

/// A tab that opens unfolds from its leading edge.
fn folded(r: RectF, vertical: bool) -> RectF {
    if vertical { RectF::new(r.left, r.top, r.right, r.top) } else { RectF::new(r.left, r.top, r.left, r.bottom) }
}

impl Motion {
    pub fn new(reduced: bool) -> Self {
        Self { reduced, ..Default::default() }
    }

    /// A press on `key` (the gear, the + button): a flash that fades out.
    pub fn press(&mut self, key: HoverKey) {
        self.presses.pulse(key, Instant::now(), specs::HOVER);
    }

    /// Where the split's focus line should be; it slides there.
    pub fn set_indicator(&mut self, target: Option<Rect>) {
        self.indicator = target;
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
        let first = self.last.is_none();
        self.last = Some(now);
        self.now = Some(now);

        // Hovers head to 1 under the pointer and back to 0 elsewhere.
        let wanted: Vec<HoverKey> = hover.map(HoverKey::of).unwrap_or([None, None]).into_iter().flatten().collect();
        for key in &wanted {
            self.hovers.set(*key, 1.0, now, specs::HOVER);
        }
        self.hovers.release_except(&wanted, now, specs::HOVER);
        self.hovers.prune(now);
        self.presses.prune(now);

        // Shown again after being hidden: it slides in from its edge.
        if !first && self.strip.width() == 0 && layout.strip.width() > 0 && layout.vertical {
            self.rail = Some(Tween::new(0.0, 1.0, now, specs::RAIL));
        }
        if self.rail.as_ref().is_some_and(|r| !r.moving(now)) || layout.strip.width() == 0 {
            self.rail = None;
        }

        let same_strip = self.strip == layout.strip && self.strip.width() > 0;
        let hidden: Vec<u64> = layout.items.iter().filter(|i| i.hidden).map(|i| i.id).collect();
        if same_strip {
            // A group folding or unfolding moves everything on the
            // collapsible's curve, so its tabs and their neighbours keep pace.
            let folding = hidden != self.hidden;
            for (id, rect, gone) in placed(layout) {
                let target = RectF::from(rect);
                match self.drawn.get(&id) {
                    Some(&drawn) => {
                        if self.targets.get(&id) != Some(&target) && drawn != target {
                            let resized = (drawn.width() - target.width()).abs() > 0.5
                                || (drawn.height() - target.height()).abs() > 0.5;
                            let spec = if folding {
                                specs::COLLAPSE
                            } else if resized {
                                specs::MORPH
                            } else {
                                specs::SHIFT
                            };
                            self.moves.insert(id, Tween::new(drawn, target, now, spec));
                        }
                    }
                    None if !self.drawn.is_empty() && !gone => {
                        self.moves.insert(id, Tween::new(folded(target, layout.vertical), target, now, specs::MORPH));
                        self.appear.insert(id, Tween::new(0.0, 1.0, now, specs::MORPH));
                    }
                    None => {}
                }
            }
        } else {
            self.moves.clear();
            self.appear.clear();
        }
        self.hidden = hidden;
        self.moves.retain(|_, t| t.moving(now));
        self.appear.retain(|_, t| t.moving(now));
        self.targets = placed(layout).map(|(id, rect, _)| (id, RectF::from(rect))).collect();
        self.strip = layout.strip;
        self.drawn = placed(layout).map(|(id, rect, _)| (id, self.rect_at(id, rect))).collect();

        // The live card slides when another tab becomes live in the same strip.
        let now_live = active.and_then(|id| layout.items.iter().find(|i| i.id == id));
        match (now_live, self.live_drawn) {
            (Some(item), Some(from)) if self.live_id != Some(item.id) && same_strip => {
                self.live = Some((from, Tween::new(0.0, 1.0, now, specs::LIVE_CARD)));
            }
            (None, _) => self.live = None,
            _ => {}
        }
        if !same_strip && self.live_id == now_live.map(|i| i.id) {
            self.live = None;
        }
        if self.live.as_ref().is_some_and(|(_, t)| !t.moving(now)) {
            self.live = None;
        }
        self.live_id = now_live.map(|i| i.id);
        self.live_drawn = now_live.map(|item| self.live_rect(self.rect_of(item)));

        // The split's focus line, the same way: a new target sets it off
        // from where it is drawn.
        if self.indicator != self.indicator_prev {
            self.indicator_slide = match (self.indicator, self.indicator_drawn) {
                (Some(_), Some(from)) if same_strip => Some((from, Tween::new(0.0, 1.0, now, specs::INDICATOR))),
                _ => None,
            };
            self.indicator_prev = self.indicator;
        }
        if self.indicator_slide.as_ref().is_some_and(|(_, t)| !t.moving(now)) {
            self.indicator_slide = None;
        }
        self.indicator_drawn = self.indicator.map(|to| self.indicator_rect(to));

        if loading && !self.reduced {
            self.spin =
                (self.spin + dt.min(0.1) * std::f32::consts::TAU / motion::SPIN_TURN_S) % std::f32::consts::TAU;
        }
        let moving = self.hovers.moving(now)
            || self.presses.moving(now)
            || self.live.is_some()
            || !self.moves.is_empty()
            || !self.appear.is_empty()
            || self.rail.is_some()
            || self.indicator_slide.is_some();
        if moving {
            Some(crate::anim::FRAME_MS)
        } else if loading && !self.reduced {
            Some(motion::SPIN_FRAME_MS)
        } else {
            None
        }
    }

    fn at(&self) -> Instant {
        self.now.unwrap_or_else(Instant::now)
    }

    pub fn hover(&self, key: HoverKey) -> f32 {
        self.hovers.get(key, self.at()).clamp(0.0, 1.0)
    }

    pub fn pressed(&self, key: HoverKey) -> f32 {
        self.presses.get(key, self.at()).clamp(0.0, 1.0)
    }

    /// Where an item is drawn this frame.
    pub fn rect_of(&self, item: &Item) -> RectF {
        self.rect_at(item.id, item.rect)
    }

    /// Where the + button is drawn this frame: it follows the last tab.
    pub fn rect_of_new_tab(&self, layout: &Layout) -> RectF {
        self.rect_at(NEW_TAB_KEY, layout.new_tab)
    }

    /// Where a group's label is drawn this frame.
    pub fn rect_of_label(&self, label: &Label) -> RectF {
        self.rect_at(label.id, label.rect)
    }

    fn rect_at(&self, id: u64, target: Rect) -> RectF {
        match self.moves.get(&id) {
            Some(tween) => tween.value(self.at()),
            None => RectF::from(target),
        }
    }

    /// How opaque an item is drawn (a tab that just opened fades in).
    pub fn opacity_of(&self, id: u64) -> f32 {
        self.appear.get(&id).map_or(1.0, |t| t.value(self.at()).clamp(0.0, 1.0))
    }

    /// Where the live tab's card is drawn this frame, heading to `to`.
    pub fn live_rect(&self, to: RectF) -> RectF {
        match &self.live {
            Some((from, t)) => from.lerp(&to, t.value(self.at())),
            None => to,
        }
    }

    /// Where the split's focus line is drawn this frame.
    pub fn indicator_rect(&self, to: Rect) -> RectF {
        match &self.indicator_slide {
            Some((from, t)) => from.lerp(&RectF::from(to), t.value(self.at())),
            None => RectF::from(to),
        }
    }

    /// How far the column has come back in, 0..1 (1 at rest).
    pub fn rail(&self) -> f32 {
        self.rail.as_ref().map_or(1.0, |t| t.value(self.at()))
    }

    pub fn spin(&self) -> f32 {
        self.spin
    }
}

