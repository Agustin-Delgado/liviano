//! The tab strip: where each tab sits (pure geometry, tested), how it is
//! drawn (Direct2D, through `gfx`) and how it moves (`Motion`).

use std::{collections::HashMap, time::Instant};

use windows::Win32::Foundation::RECT;

use crate::{
    anim::{Levels, Tween, specs},
    design::{Palette, Rgb, group_tint, group_tone, motion, size},
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
    /// Back, forward or reload, beside them.
    Nav(NavButton),
    /// The gear: Settings, always there.
    Settings,
}

/// The page's own buttons, after the window's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NavButton {
    Back,
    Forward,
    Reload,
}

impl NavButton {
    pub const ALL: [NavButton; 3] = [NavButton::Back, NavButton::Forward, NavButton::Reload];
}

/// Which of the page's buttons can act: back and forward follow the live
/// tab's history, reload needs a page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NavState {
    pub back: bool,
    pub forward: bool,
    pub reload: bool,
}

impl NavState {
    pub fn enabled(&self, button: NavButton) -> bool {
        match button {
            NavButton::Back => self.back,
            NavButton::Forward => self.forward,
            NavButton::Reload => self.reload,
        }
    }
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
    /// What its header says in the column: the name, or for an unnamed
    /// group the site of its first tab ("Group" when there is none).
    pub title: String,
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
/// a small dot alone. `text_w` is the name measured in the label's font
/// (0 for an unnamed group).
pub fn label_width(text_w: f32, scale: f32) -> i32 {
    let m = Metrics { scale };
    if text_w <= 0.0 { m.px(LABEL_DOT_W) } else { (text_w.ceil() as i32 + m.px(2 * LABEL_PAD)).min(m.px(LABEL_MAX_W)) }
}

/// The label's font: the strip's small step, medium.
pub fn label_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::MEDIUM, Align::Center)
}

const LABEL_DOT_W: i32 = 20;
const LABEL_PAD: i32 = 8;
const LABEL_MAX_W: i32 = 140;
/// The tinted chip of a label on a top strip.
const CHIP_H: f32 = 22.0;
/// The group's mark: a dot a little over half a mark's letter.
const DOT: f32 = 6.0;
/// The header of a group in the column: shorter than a tab's row (32), so
/// it reads as a heading over the tabs rather than one more row.
const GROUP_ROW: i32 = 22;
/// The header's dot: a little under the name's x-height.
const HEADER_DOT: f32 = 5.0;

/// The header's name: the caption step, regular (medium looks too heavy at
/// 11 px). Its ink is muted; only the dot and the guide line carry the
/// group's colour.
fn header_font(scale: f32) -> Font {
    Font::new(size::CAPTION * scale, size::REGULAR, Align::Leading)
}

/// The y of the middle of the name's lowercase letters in a header: the dot
/// sits on the letters, not on the middle of the row (the font's ascent
/// keeps room over the capitals, so the two are about 2 px apart).
fn header_mark_y(rect: RectF, scale: f32) -> f32 {
    crate::gfx::line_marks(header_font(scale)).x_middle_in(rect.top, rect.bottom)
}

/// The guide line's left edge and width: one device pixel per 100 %, on
/// whole pixels, so it stays sharp at every scale. The dot is centred on it.
fn guide_line(left: f32, scale: f32) -> (f32, f32) {
    let w = scale.round().max(1.0);
    ((left + GUIDE_X * scale - w / 2.0).round(), w)
}

/// The centre x of a header's chevron: the centre of the close buttons of
/// the tabs under it, so the two marks share one axis.
fn chevron_center_x(right: f32, scale: f32) -> f32 {
    let m = Metrics { scale };
    let size = m.px(CLOSE_SIZE);
    right - (m.px(CLOSE_INSET) + size) as f32 + size as f32 / 2.0
}

/// The close button of a row in the column: its size and its inset from
/// the row's right end.
const CLOSE_SIZE: i32 = 20;
const CLOSE_INSET: i32 = 6;
/// How far a group's tabs sit in from the column's edge, under its header:
/// clear of the guide line that hangs from the header's dot.
const GROUP_INDENT: i32 = 12;
/// Where the dot and the guide line sit, in from the header's left.
const GUIDE_X: f32 = 6.0;
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
    /// Back, forward and reload, after the window's buttons.
    pub nav: [Rect; 3],
    /// The extensions button, beside the gear; empty only on the slim caption.
    pub extensions: Rect,
    /// The pinned extensions beside it, in order.
    pub extension_marks: Vec<Rect>,
    /// The gear that opens Settings: at the right end of a top strip and of
    /// the caption row, at the foot of the column.
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

impl Metrics {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
}

#[cfg(all(test, nea_tests))]
pub fn layout(tabs: &[TabInfo], width: i32, height: i32, vertical: bool, collapsed: bool, m: &Metrics) -> Layout {
    layout_with(tabs, &[], width, height, vertical, collapsed, m, None)
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
        // One margin on every side of its controls: the window's edges,
        // above and under them.
        let edge = m.px(frame::LEAD);
        let row = 2 * edge + m.px(CONTROL_H);
        let caption = Rect::new(0, 0, width, row);
        let frame = Frame::new(caption, edge, m.scale);
        let nav = nav_row(frame.right() + m.px(frame::NAV_LEAD), edge, row - edge, m);
        let right = width - edge;
        let settings = Rect::new(right - m.px(SETTINGS_W), edge, right, row - edge);
        return Layout {
            content: Rect::new(0, row, width, height.max(row)),
            vertical,
            frame,
            nav,
            settings,
            ..Default::default()
        };
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
/// The height of the controls in a caption row: the window's capsule,
/// back, forward, reload, the gear (a tab's height).
const CONTROL_H: i32 = 28;
/// Back, forward and reload: a pinned tab's width, a hair narrower.
const NAV_W: i32 = 28;

/// Back, forward and reload from `left`, two pixels apart.
fn nav_row(left: i32, top: i32, bottom: i32, m: &Metrics) -> [Rect; 3] {
    let (w, gap) = (m.px(NAV_W), m.px(2));
    std::array::from_fn(|i| {
        let l = left + i as i32 * (w + gap);
        Rect::new(l, top, l + w, bottom)
    })
}

/// The width of the three, gaps included.
fn nav_width(m: &Metrics) -> i32 {
    3 * m.px(NAV_W) + 2 * m.px(2)
}

/// Room always left free between the last tab (or +) and what sits at the
/// right of the strip, so the window can be grabbed however many tabs there are.
/// (Between the window's buttons and back there is always a little more.)
const DRAG_ROOM: i32 = 32;

/// Width the extensions block takes at the right of a horizontal strip.
fn extensions_width(pinned: usize, width: i32, m: &Metrics) -> i32 {
    m.px(30) + m.px(8) + shown_marks(pinned, width / 4, m) as i32 * (m.px(28) + m.px(2))
}

/// Pinned extensions that fit in `room` pixels.
fn shown_marks(pinned: usize, room: i32, m: &Metrics) -> usize {
    pinned.min((room / (m.px(28) + m.px(2))).max(0) as usize)
}

/// The button just left of the gear, its twin: the same size, on the same
/// row, with the gap that separates back, forward and reload. Pinned ones go
/// to its left. In the column they stop at its pad.
fn place_extensions(layout: &mut Layout, pinned: usize, m: &Metrics) {
    let (mark_w, gap) = (m.px(28), m.px(2));
    let gear = layout.settings;
    let right = gear.left - gap;
    layout.extensions = Rect::new(right - gear.width(), gear.top, right, gear.bottom);
    let room = if layout.vertical { layout.extensions.left - m.px(8) } else { layout.strip.width() / 4 };
    let mut x = layout.extensions.left - gap;
    for _ in 0..shown_marks(pinned, room, m) {
        layout.extension_marks.insert(0, Rect::new(x - mark_w, gear.top, x, gear.bottom));
        x -= mark_w + gap;
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
    // The strip's edge margin: the air above and under its controls, also
    // left of the capsule and right of the gear. The strip is the controls
    // and that air (40 at 100 %), so it stays even at every scale.
    let edge = m.px(frame::LEAD);
    let bar = 2 * edge + m.px(CONTROL_H);
    let pad = m.px(8);
    let gap = m.px(2);
    let pinned_w = m.px(30);
    let plus_w = m.px(30);
    let item_top = edge;
    let item_bottom = bar - edge;

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
    // The window's buttons lead, the page's follow, then the tabs; the gear
    // (and the extensions) close the strip at the right.
    let frame = Frame::new(Rect::new(0, 0, width, bar), edge, m.scale);
    let nav = nav_row(frame.right() + m.px(frame::NAV_LEAD), item_top, item_bottom, m);
    let start = nav[2].right + pad;
    let settings = Rect::new(width - edge - m.px(SETTINGS_W), item_top, width - edge, item_bottom);
    let reserved =
        extensions.map_or(0, |n| extensions_width(n, width, m)) + m.px(SETTINGS_W + 2) + edge + m.px(DRAG_ROOM);
    let available = width - start - plus_w - pinned * (pinned_w + gap) - normal * gap - labels_w - reserved;
    let normal_w = if normal > 0 { (available / normal).clamp(m.px(44), m.px(200)) } else { 0 };

    let mut x = start;
    let mut items = Vec::with_capacity(tabs.len());
    let mut labels = Vec::new();
    let mut previous = None;
    for tab in tabs {
        let group = group_of(tab, groups);
        let id = group.map(|g| g.id);
        if id != previous {
            if let Some(g) = group {
                labels.push(Label {
                    id: g.id,
                    rect: Rect::new(x, item_top, x + g.label_w, item_bottom),
                    collapsed: g.collapsed,
                });
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
        nav,
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
    // The top row is the caption: the window's buttons at its left, back,
    // forward and reload at its right (Arc's column), the rest of it moves
    // the window.
    // The column's pad is its margin on every side of that row: left of the
    // capsule, right of reload, above the row and under it (the first row
    // starts at the caption's bottom).
    let caption = Rect::new(0, 0, side, 2 * pad + m.px(CONTROL_H));
    let frame = Frame::new(caption, pad, m.scale);
    let nav = nav_row(side - pad - nav_width(m), pad, caption.bottom - pad, m);
    // Pinned tabs: a grid of squares under it.
    let per_row = ((side - 2 * pad + gap) / (cell + gap)).max(1);
    let start = caption.bottom;
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
                let header = m.px(GROUP_ROW);
                labels.push(Label {
                    id: g.id,
                    rect: Rect::new(pad, y, side - pad, y + header),
                    collapsed: g.collapsed,
                });
                y += header + gap;
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
        let (size, inset) = (m.px(CLOSE_SIZE), m.px(CLOSE_INSET));
        let cy = (rect.top + rect.bottom) / 2;
        let close = Some(Rect::new(rect.right - inset - size, cy - size / 2, rect.right - inset, cy + size / 2));
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
        nav,
        ..Default::default()
    }
}

impl Layout {
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some(button) = self.frame.button_at(x, y) {
            return Some(Hit::Window(button));
        }
        if let Some(i) = self.nav.iter().position(|r| r.contains(x, y)) {
            return Some(Hit::Nav(NavButton::ALL[i]));
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
    /// The window is the active one: its buttons wear their full ink.
    pub window_active: bool,
    /// Which of back, forward and reload can act.
    pub nav: NavState,
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
        p.pairs.iter().find_map(|&(l, r)| {
            if l == a {
                Some(r)
            } else if r == a {
                Some(l)
            } else {
                None
            }
        })
    });

    // On a top strip the tabs, past their smallest width, run on under
    // the extensions and the gear: they are cut short of them.
    let tabs_area = if p.layout.vertical || p.layout.strip.width() == 0 {
        RectF::from(p.layout.strip)
    } else {
        let end = [p.layout.extension_marks.first().copied(), Some(p.layout.extensions), Some(p.layout.settings)]
            .into_iter()
            .flatten()
            .filter(|r| r.width() > 0)
            .map(|r| r.left)
            .min()
            .unwrap_or(p.layout.strip.right);
        RectF::new(0.0, 0.0, (end - frame::px(2, p.scale)) as f32, p.layout.strip.bottom as f32)
    };
    g.clip(tabs_area, |g| paint_tabs(g, p, shown_partner));

    paint_controls(g, p);
}

/// The group lines, the live tab's card, the tabs and the group labels.
fn paint_tabs(g: &Painter, p: &Paint, shown_partner: Option<u64>) {
    let pal = p.palette;
    let radius = size::RADIUS * p.scale;
    paint_group_lines(g, p);

    // The live tab's card first, on its own: it slides from the tab that
    // was live to the one that is now, under the titles. When a group
    // folds over the live tab it fades in on the next one instead of
    // sliding across the label.
    let tabs = order_like(p.layout, p.tabs);
    if let Some((item, tab)) = p.layout.items.iter().zip(&tabs).find(|(i, _)| p.active == Some(i.id)) {
        let ground = if tab.private { pal.private_tab } else { pal.tab_active };
        let opacity = p.motion.opacity_of(item.id) * p.motion.live_opacity();
        let rect = p.motion.live_rect(p.motion.rect_of(item));
        if opacity > 0.0 && !item.hidden {
            let fill = Color::from(ground).with_alpha(opacity);
            g.ring_card(rect, radius, fill, pal.dark, opacity, p.scale);
        }
    }

    let row = 32.0 * p.scale;
    for (item, tab) in p.layout.items.iter().zip(&tabs) {
        let rect = p.motion.rect_of(item);
        // Folded into its group's label, or on its way there: cut to what
        // is left of it.
        if rect.width() < 1.0 || rect.height() < 1.0 || !g.visible(rect.inset(-row / 4.0, -row / 4.0)) {
            continue;
        }
        let folding = item.hidden
            || (rect.width() - item.rect.width() as f32).abs() > 0.5
            || (rect.height() - item.rect.height() as f32).abs() > 0.5;
        if folding && item.group.is_some() {
            // In the column the row keeps its height under the shrinking
            // clip, so its title slides away instead of squashing; either
            // way it fades with what is left of it.
            let (whole, fade) = if p.layout.vertical {
                (RectF::new(rect.left, rect.top, rect.right, rect.top + row), fold_fade(rect.height() / row))
            } else {
                (rect, fold_fade((rect.width() - 8.0 * p.scale) / (48.0 * p.scale)))
            };
            if fade > 0.0 {
                g.clip(rect, |g| paint_tab(g, p, item, tab, whole, shown_partner, fade));
            }
        } else {
            paint_tab(g, p, item, tab, rect, shown_partner, 1.0);
        }
    }
    paint_labels(g, p);
}

/// How opaque a folding tab is for how much of it is left (0..1): it is
/// gone a little before its room is, so nothing squeezes at the end.
fn fold_fade(left: f32) -> f32 {
    let t = ((left - 0.15) / 0.85).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One tab: its ground, its mark or icon, its title and its cross.
fn paint_tab(g: &Painter, p: &Paint, item: &Item, tab: &TabInfo, rect: RectF, shown_partner: Option<u64>, fade: f32) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let radius = px(size::RADIUS);
    let vertical = p.layout.vertical;
    {
        let active = p.active == Some(item.id);
        let hover = p.motion.hover(HoverKey::Tab(item.id)) * fade;
        let opacity = p.motion.opacity_of(item.id) * fade;
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

/// What joins a group's tabs: a fine line under them on a top strip (from
/// the label to the last tab, as Chrome's), a hairline guide down from the
/// header's dot in the column. It shortens with the tabs as the group folds.
fn paint_group_lines(g: &Painter, p: &Paint) {
    let px = |v: f32| v * p.scale;
    for label in &p.layout.labels {
        let Some(info) = p.tab_groups.iter().find(|i| i.id == label.id) else { continue };
        let tone = group_tone(info.color, p.palette.dark);
        let opacity = p.motion.opacity_of(label.id);
        let at = p.motion.rect_of_label(label);
        let members = p.layout.items.iter().filter(|i| i.group == Some(label.id)).map(|i| p.motion.rect_of(i));
        if p.layout.vertical {
            let bottom = members.map(|r| r.bottom).fold(at.bottom, f32::max);
            if bottom - at.bottom > px(6.0) {
                // The group's own colour, opaque (it fades only while the
                // group folds), square and on whole pixels.
                let (x, w) = guide_line(at.left, p.scale);
                let line = Color::from(tone.fill).with_alpha(opacity);
                let start = (header_mark_y(at, p.scale) + px(6.0)).round();
                g.fill(RectF::new(x, start, x + w, (bottom - px(4.0)).round()), line);
            }
        } else {
            let right = members.map(|r| r.right).fold(at.right, f32::max);
            if right - at.right > px(1.0) {
                let top = at.bottom + px(2.0);
                // Folded, the line would only underline the chip: it fades
                // out as the tabs fold in.
                let line = Color::from(tone.fill).with_alpha(0.8 * opacity * (1.0 - p.motion.fold_of(label.id)));
                g.round(RectF::new(at.left + px(4.0), top, right - px(4.0), top + px(1.5)), px(0.75), line);
            }
        }
    }
}

/// Each group's label. On a top strip, a chip of the group's colour at a
/// tint, with its name in that colour (or a small dot when it has none).
/// In the column, a heading over its tabs: the dot the guide line hangs
/// from, the name in the group's colour, the count while folded and a
/// chevron that turns as it folds.
fn paint_labels(g: &Painter, p: &Paint) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    for label in &p.layout.labels {
        let Some(info) = p.tab_groups.iter().find(|i| i.id == label.id) else { continue };
        let tone = group_tone(info.color, pal.dark);
        let rect = p.motion.rect_of_label(label);
        if rect.width() < 1.0 {
            continue;
        }
        let opacity = p.motion.opacity_of(label.id);
        let over = p.motion.hover(HoverKey::Group(label.id));
        let fold = p.motion.fold_of(label.id);
        let (_, cy) = rect.center();
        let dot = px(DOT);
        let named = !info.name.trim().is_empty();
        let name_ink = Color::from(tone.ink).with_alpha(opacity);
        if p.layout.vertical {
            paint_header(g, p, info, rect, opacity, over, fold);
        } else {
            let h = px(CHIP_H);
            let chip = RectF::new(rect.left, cy - h / 2.0, rect.right, cy + h / 2.0);
            let tint = group_tint(pal.dark) * (1.0 + 0.7 * over);
            g.round(chip, px(size::RADIUS_CONTROL), Color::from(tone.fill).with_alpha(tint * opacity));
            if named {
                if chip.width() > px(2.0 * LABEL_PAD as f32) {
                    g.text(info.name, chip.inset(px(LABEL_PAD as f32), 0.0), label_font(p.scale), name_ink);
                }
            } else {
                let (cx, _) = chip.center();
                g.round(
                    RectF::new(cx - dot / 2.0, cy - dot / 2.0, cx + dot / 2.0, cy + dot / 2.0),
                    dot / 2.0,
                    Color::from(tone.fill).with_alpha(opacity),
                );
            }
        }
    }
}

/// A group's header in the column: the dot the guide line hangs from, the
/// name in muted ink, the count while folded and a chevron that turns as it
/// folds. No ground, at rest or under the pointer: a hover only lifts the
/// ink. The dot, the count and the chevron are centred on the name's
/// lowercase letters (from the font's metrics), not on the row.
fn paint_header(g: &Painter, p: &Paint, info: &GroupInfo, rect: RectF, opacity: f32, over: f32, fold: f32) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let tone = group_tone(info.color, pal.dark);
    let font = header_font(p.scale);
    let mid = header_mark_y(rect, p.scale);
    let (line_x, line_w) = guide_line(rect.left, p.scale);
    let dx = line_x + line_w / 2.0;
    let chevron_x = chevron_center_x(rect.right, p.scale);
    let mut room = chevron_x - px(10.0);
    if fold > 0.0 {
        let count = info.count.to_string();
        let font = Font { align: Align::Trailing, ..font };
        let ink = Color::from(pal.ink_asleep).with_alpha(opacity * fold);
        g.text(&count, RectF::new(room - px(24.0), rect.top, room, rect.bottom), font, ink);
        room -= px(28.0) * fold;
    }
    let d = px(HEADER_DOT);
    let fill = Color::from(tone.fill).with_alpha(opacity);
    g.round(RectF::new(dx - d / 2.0, mid - d / 2.0, dx + d / 2.0, mid + d / 2.0), d / 2.0, fill);
    let name_ink = Color::from(pal.ink_muted.mix(pal.ink, over)).with_alpha(opacity);
    g.text(&info.title, RectF::new(dx + px(10.0), rect.top, room, rect.bottom), font, name_ink);
    let ink = Color::from(pal.ink_asleep.mix(pal.ink_muted, over)).with_alpha(opacity);
    draw_chevron(g, (chevron_x, mid), fold, ink, p.scale);
}

/// A small chevron, turning from pointing down (open) to pointing right
/// (`folded` 1) as the group folds.
fn draw_chevron(g: &Painter, (cx, cy): (f32, f32), folded: f32, ink: Color, scale: f32) {
    let s = |v: f32| v * scale;
    let (sin, cos) = ((1.0 - folded.clamp(0.0, 1.0)) * std::f32::consts::FRAC_PI_2).sin_cos();
    let turn = |(x, y): (f32, f32)| (cx + s(x * cos - y * sin), cy + s(x * sin + y * cos));
    let points = [turn((-1.5, -3.5)), turn((2.0, 0.0)), turn((-1.5, 3.5))];
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
            g.text("New tab", text, Font::new(px(size::TEXT), size::REGULAR, Align::Leading), ink);
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
    for (button, rect) in NavButton::ALL.into_iter().zip(p.layout.nav) {
        if rect.width() == 0 {
            continue;
        }
        let rect: RectF = rect.into();
        let ink = if p.nav.enabled(button) {
            let over = p.motion.hover(HoverKey::Nav(button));
            ghost_ground(g, rect, radius, over, p.motion.pressed(HoverKey::Nav(button)), pal);
            Color::from(pal.ink_muted.mix(pal.ink, over))
        } else {
            // Unavailable: faint, and nothing answers the pointer.
            Color::from(pal.ink_asleep).with_alpha(if pal.dark { 0.6 } else { 0.7 })
        };
        draw_nav(g, rect, button, ink, p.scale);
    }
    frame::paint(g, &p.layout.frame, pal, p.scale, p.maximized, p.window_active, p.pressed, |b| {
        p.motion.hover(HoverKey::Window(b))
    });
}

/// Back and forward as Safari draws them (an open chevron), reload as a
/// circular arrow.
fn draw_nav(g: &Painter, rect: RectF, button: NavButton, ink: Color, scale: f32) {
    let s = |v: f32| v * scale;
    let (cx, cy) = rect.center();
    let w = s(1.5);
    match button {
        NavButton::Back => {
            g.polyline(&[(cx + s(2.5), cy - s(5.5)), (cx - s(3.0), cy), (cx + s(2.5), cy + s(5.5))], w, ink)
        }
        NavButton::Forward => {
            g.polyline(&[(cx - s(2.5), cy - s(5.5)), (cx + s(3.0), cy), (cx - s(2.5), cy + s(5.5))], w, ink)
        }
        NavButton::Reload => {
            let r = s(5.5);
            let (start, sweep) = (0.35, 5.2);
            g.arc(cx, cy, r, start, sweep, s(1.4), ink);
            // The arrowhead where the arc ends, pointing on round.
            let a = start + sweep;
            let (sin, cos) = a.sin_cos();
            let (ex, ey) = (cx + r * sin, cy - r * cos);
            let (tx, ty) = (cos, sin);
            let (nx, ny) = (sin, -cos);
            g.polygon(
                &[
                    (ex + tx * s(2.6), ey + ty * s(2.6)),
                    (ex - tx * s(0.8) + nx * s(2.6), ey - ty * s(0.8) + ny * s(2.6)),
                    (ex - tx * s(0.8) - nx * s(2.6), ey - ty * s(0.8) - ny * s(2.6)),
                ],
                ink,
            );
        }
    }
}

/// A ghost button's ground (the design system's ghost variant): nothing at rest, the
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
    Nav(NavButton),
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
            Hit::Nav(b) => [Some(HoverKey::Nav(b)), None],
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
    /// The live card fading in on the tab that took over from one a group
    /// folded away.
    live_fade: Option<Tween<f32>>,
    /// Each group label's fold, 0 open to 1 folded (its chevron turns with it).
    folds: HashMap<u64, Tween<f32>>,
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
        // A group folding or unfolding moves everything on the
        // collapsible's curve, so its tabs and their neighbours keep pace.
        let folding = same_strip && hidden != self.hidden;
        for label in &layout.labels {
            let target = if label.collapsed { 1.0 } else { 0.0 };
            match self.folds.get_mut(&label.id) {
                Some(fold) if same_strip => fold.to(target, now, specs::COLLAPSE),
                _ => {
                    self.folds.insert(label.id, Tween::rest(target));
                }
            }
        }
        self.folds.retain(|id, _| layout.labels.iter().any(|l| l.id == *id));
        if same_strip {
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

        // The live card slides when another tab becomes live in the same
        // strip, unless a group folded the one it was on: then it would
        // cross the label, so it fades in where it lands.
        let now_live = active.and_then(|id| layout.items.iter().find(|i| i.id == id));
        match (now_live, self.live_drawn) {
            (Some(item), Some(_)) if self.live_id != Some(item.id) && same_strip && folding => {
                self.live = None;
                self.live_fade = Some(Tween::new(0.0, 1.0, now, specs::MORPH));
            }
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
        if self.live_fade.as_ref().is_some_and(|t| !t.moving(now)) || !same_strip {
            self.live_fade = None;
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
            self.spin = (self.spin + dt.min(0.1) * std::f32::consts::TAU / motion::SPIN_TURN_S) % std::f32::consts::TAU;
        }
        let moving = self.hovers.moving(now)
            || self.presses.moving(now)
            || self.live.is_some()
            || self.live_fade.is_some()
            || self.folds.values().any(|t| t.moving(now))
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

    /// How opaque the live card is (1 but while it fades in after a fold).
    pub fn live_opacity(&self) -> f32 {
        self.live_fade.as_ref().map_or(1.0, |t| t.value(self.at()).clamp(0.0, 1.0))
    }

    /// How far a group is folded, 0 (open) to 1.
    pub fn fold_of(&self, group: u64) -> f32 {
        self.folds.get(&group).map_or(0.0, |t| t.value(self.at()).clamp(0.0, 1.0))
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

    /// The split's focus line slides between the pages.
    pub fn indicator_moving(&self) -> bool {
        self.indicator_slide.is_some()
    }

    /// The column slides in from its edge: everything in it moves.
    pub fn rail_moving(&self) -> bool {
        self.rail.is_some()
    }

    /// Only the loading ring moves at `now`: it turns at its own rate.
    pub fn only_spinning(&self, now: Instant) -> bool {
        !self.settling(now) && !self.hovers.moving(now) && !self.presses.moving(now)
    }

    /// Something of the strip's layout still glides at `now`: a tab, a
    /// group's fold, the live card, the column. Hovers, presses and the
    /// loading ring do not count: they never end while a page loads.
    pub fn settling(&self, now: Instant) -> bool {
        self.live.as_ref().is_some_and(|(_, t)| t.moving(now))
            || self.live_fade.as_ref().is_some_and(|t| t.moving(now))
            || self.folds.values().any(|t| t.moving(now))
            || self.moves.values().any(|t| t.moving(now))
            || self.appear.values().any(|t| t.moving(now))
            || self.rail.as_ref().is_some_and(|t| t.moving(now))
            || self.indicator_slide.as_ref().is_some_and(|(_, t)| t.moving(now))
    }

    /// What this frame draws that motion can change, for `changed_area`.
    /// `loading` are the tabs that show the loading ring.
    pub fn sheet(&self, layout: &Layout, active: Option<u64>, loading: &[u64], scale: f32) -> Sheet {
        let mut sheet = Sheet::with_capacity(layout.items.len() + layout.labels.len() + 16);
        // A card's drop shadow and a ground's ring reach past their rect.
        let margin = (4.0 * scale).ceil();
        let grow = |r: RectF| RectF::new(r.left - margin, r.top - margin, r.right + margin, r.bottom + margin);
        for item in &layout.items {
            let rect = self.rect_of(item);
            let spin = if loading.contains(&item.id) { self.spin } else { -1.0 };
            let look = [
                self.opacity_of(item.id),
                self.hover(HoverKey::Tab(item.id)),
                self.hover(HoverKey::Close(item.id)),
                spin + if active == Some(item.id) { 10.0 } else { 0.0 },
            ];
            sheet.insert(SheetKey::Item(item.id), Piece { bounds: grow(rect), look });
        }
        if let Some(item) = active.and_then(|id| layout.items.iter().find(|i| i.id == id)) {
            let rect = self.live_rect(self.rect_of(item));
            let look = [self.live_opacity() * self.opacity_of(item.id), 0.0, 0.0, 0.0];
            sheet.insert(SheetKey::Live, Piece { bounds: grow(rect), look });
        }
        for label in &layout.labels {
            let rect = self.rect_of_label(label);
            let look = [self.opacity_of(label.id), self.hover(HoverKey::Group(label.id)), self.fold_of(label.id), 0.0];
            sheet.insert(SheetKey::Item(label.id), Piece { bounds: grow(rect), look });
            // The group's line runs from its label past its last tab.
            let mut line = rect;
            for member in layout.items.iter().filter(|i| i.group == Some(label.id)) {
                let r = self.rect_of(member);
                line = RectF::new(
                    line.left.min(r.left),
                    line.top.min(r.top),
                    line.right.max(r.right),
                    line.bottom.max(r.bottom),
                );
            }
            sheet.insert(SheetKey::Line(label.id), Piece { bounds: grow(line), look });
        }
        let plus = self.rect_of_new_tab(layout);
        let look = [self.hover(HoverKey::NewTab), self.pressed(HoverKey::NewTab), 0.0, 0.0];
        sheet.insert(SheetKey::Item(NEW_TAB_KEY), Piece { bounds: grow(plus), look });
        let mut control = |key: HoverKey, rect: Rect| {
            if rect.width() > 0 {
                let look = [self.hover(key), self.pressed(key), 0.0, 0.0];
                sheet.insert(SheetKey::Hover(key), Piece { bounds: grow(rect.into()), look });
            }
        };
        control(HoverKey::Extensions, layout.extensions);
        for (i, mark) in layout.extension_marks.iter().enumerate() {
            control(HoverKey::PinnedExtension(i), *mark);
        }
        control(HoverKey::Settings, layout.settings);
        for (button, rect) in NavButton::ALL.into_iter().zip(layout.nav) {
            control(HoverKey::Nav(button), rect);
        }
        let buttons = layout.frame.buttons;
        let all = buttons.iter().skip(1).fold(buttons[0], |a, b| {
            Rect::new(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))
        });
        for button in [WindowButton::Close, WindowButton::Minimize, WindowButton::Maximize] {
            control(HoverKey::Window(button), all);
        }
        sheet
    }
}

/// What a piece of the strip is in a sheet.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum SheetKey {
    /// A tab, a group label or the + button, by id.
    Item(u64),
    /// The live tab's card.
    Live,
    /// A group's line.
    Line(u64),
    /// A control of the strip and the caption row.
    Hover(HoverKey),
}

/// Where a piece is drawn (with the reach of its shadow) and the numbers
/// that decide its look this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub bounds: RectF,
    pub look: [f32; 4],
}

pub type Sheet = HashMap<SheetKey, Piece>;

/// The part of the strip a frame has to draw again: the old and the new
/// place of every piece that moved, changed its look, came or went. None
/// when nothing changed.
pub fn changed_area(old: &Sheet, new: &Sheet) -> Option<RectF> {
    let mut area: Option<RectF> = None;
    let mut add = |r: RectF| {
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        area = Some(match area {
            Some(a) => RectF::new(a.left.min(r.left), a.top.min(r.top), a.right.max(r.right), a.bottom.max(r.bottom)),
            None => r,
        });
    };
    for (key, piece) in new {
        match old.get(key) {
            Some(was) if was == piece => {}
            Some(was) => {
                add(was.bounds);
                add(piece.bounds);
            }
            None => add(piece.bounds),
        }
    }
    for (key, was) in old {
        if !new.contains_key(key) {
            add(was.bounds);
        }
    }
    area
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/tabbar.rs"]
#[rustfmt::skip]
mod tests;
