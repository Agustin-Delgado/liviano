//! The menus Nea draws itself: the menu of a tab, of a group, of a space,
//! of a bookmark, of the extensions button, the history of Back and Forward
//! and the lists of the other features. One component replaces the Windows
//! popup menus (`TrackPopupMenu`), in the colours, the type and the corners
//! of the rest of the interface.
//!
//! A call site builds a `Menu` (items, separators, submenus) and calls
//! `track`, which returns the id of the item that the person selected, or 0.
//! `track` runs a message loop of its own until then, as `TrackPopupMenu`
//! does: the thread continues to dispatch every message, so the pages, the
//! timers and the animations do not stop.
//!
//! Each open level is a small popup window with per-pixel alpha
//! (`UpdateLayeredWindow`): the round panel, its ring and its shadow are
//! real. The windows never take the activation or the keyboard focus, so
//! the page keeps its focus, as with a Windows menu. Two low-level hooks,
//! set only while a menu is open, give the keys to the menu and tell it
//! about a click outside it.
//!
//! Nothing stays alive when no menu is open: no window, no bitmap, no
//! drawing target, no timer and no hook. A menu shows at once and fades out
//! in 100 ms.
//!
//! The layout, the hit test, the placement on the screen and the keyboard
//! navigation are pure functions (tested).

use std::{
    cell::{Cell, RefCell},
    time::Instant,
};

use serde_json::{Value, json};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint, BitBlt,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint, GetMonitorInfoW,
            HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SRCCOPY,
            SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Accessibility::{HWINEVENTHOOK, NotifyWinEvent, SetWinEventHook, UnhookWinEvent},
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW, ReleaseCapture, VIRTUAL_KEY, VK_CONTROL, VK_DOWN,
                VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, w},
};

use crate::{
    anim::{self, Tween, specs},
    design::{self, Palette, Rgb, lift, size},
    gfx::{self, Align, Font, Painter, RectF, Surface},
    tabbar::Rect,
};

// ---------- the model ----------

/// One level of a menu: its items from top to bottom.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Menu {
    pub items: Vec<Item>,
}

/// One line of a menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    /// What `track` returns when the person selects the item.
    pub id: usize,
    pub label: String,
    /// The keys of the command, at the right end ("Ctrl+Shift+B").
    pub shortcut: String,
    pub separator: bool,
    pub enabled: bool,
    /// Text that only informs: smaller, and never selected.
    pub note: bool,
    /// `Some` for an item that is on or off.
    pub checked: Option<bool>,
    /// A dot in this colour before the label (a colour list).
    pub swatch: Option<Rgb>,
    pub submenu: Option<Menu>,
}

impl Menu {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, item: Item) -> &mut Item {
        self.items.push(item);
        let last = self.items.len() - 1;
        &mut self.items[last]
    }

    /// A command. A tab character in `label` starts its shortcut text.
    pub fn item(&mut self, id: usize, label: &str) -> &mut Item {
        let (label, shortcut) = label.split_once('\t').unwrap_or((label, ""));
        self.push(Item {
            id,
            label: label.to_string(),
            shortcut: shortcut.to_string(),
            enabled: true,
            ..Default::default()
        })
    }

    /// A command that is on or off.
    pub fn check(&mut self, id: usize, label: &str, on: bool) -> &mut Item {
        let item = self.item(id, label);
        item.checked = Some(on);
        item
    }

    /// A line that only informs.
    pub fn note(&mut self, label: &str) -> &mut Item {
        self.push(Item { label: label.to_string(), note: true, ..Default::default() })
    }

    pub fn separator(&mut self) {
        self.push(Item { separator: true, ..Default::default() });
    }

    /// An item that opens `menu` beside it.
    pub fn submenu(&mut self, label: &str, menu: Menu) -> &mut Item {
        let enabled = !menu.items.is_empty();
        self.push(Item { label: label.to_string(), enabled, submenu: Some(menu), ..Default::default() })
    }
}

impl Item {
    pub fn enabled(&mut self, on: bool) -> &mut Self {
        self.enabled = on;
        self
    }

    pub fn swatch(&mut self, color: Rgb) -> &mut Self {
        self.swatch = Some(color);
        self
    }

    /// Whether the pointer and the keys can stop on it.
    pub fn selectable(&self) -> bool {
        !self.separator && !self.note && self.enabled
    }
}

// ---------- layout ----------

/// Measures before the DPI scale.
const ROW: f32 = 28.0;
/// Between the panel's edge and its rows.
const PAD: f32 = 4.0;
/// The height a separator takes: the line and the space around it.
const SEPARATOR: f32 = 9.0;
/// Between a row's edge and what it shows.
const INSET: f32 = 8.0;
/// The column of the check marks and the colour dots, and the space after it.
const MARK: f32 = 16.0;
const MARK_GAP: f32 = 8.0;
/// Between the label and the shortcut or the arrow.
const TAIL_GAP: f32 = 24.0;
const ARROW: f32 = 16.0;
const MIN_WIDTH: f32 = 176.0;
const MAX_WIDTH: f32 = 360.0;
/// A submenu covers this much of its parent's panel.
const OVERLAP: f32 = 4.0;
/// The band at the top or the bottom of a list too long for the screen.
const SCROLL_HINT: f32 = 16.0;
/// The pointer moves this far before a button release selects an item.
const ARM_SLOP: i32 = 4;
/// The pointer rests this long on an item before its submenu opens.
const SUBMENU_DELAY_MS: u32 = 180;

fn px(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}

/// Where each row of a level is, in the panel's own pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub width: i32,
    /// The height of all the rows and the padding. The panel is shorter
    /// when the screen has less room: then the rows scroll.
    pub height: i32,
    /// One rectangle for each item, separators too.
    pub rows: Vec<Rect>,
    /// The width of the column before the labels (0 without marks).
    pub lead: i32,
    /// The width of the column after the labels (0 when nothing is there).
    pub tail: i32,
}

/// Lays out `items`. `widths` gives, for each item, the width of its label
/// and of its shortcut in pixels.
pub fn layout(items: &[Item], widths: &[(f32, f32)], scale: f32) -> Layout {
    let pad = px(PAD, scale);
    let inset = px(INSET, scale);
    let marks = items.iter().any(|i| i.checked.is_some() || i.swatch.is_some());
    let lead = if marks { px(MARK + MARK_GAP, scale) } else { 0 };
    let width_of = |pick: fn(&(f32, f32)) -> f32| widths.iter().map(pick).fold(0.0f32, f32::max).ceil() as i32;
    let (label, shortcut) = (width_of(|w| w.0), width_of(|w| w.1));
    // A row of a colour list shows its check mark at the right end.
    let arrow = items.iter().any(|i| i.submenu.is_some() || (i.swatch.is_some() && i.checked == Some(true)));
    let tail = shortcut.max(if arrow { px(ARROW, scale) } else { 0 });
    let gap = if tail > 0 { px(TAIL_GAP, scale) } else { 0 };
    let content = inset + lead + label + gap + tail + inset;
    let width = (content + 2 * pad).clamp(px(MIN_WIDTH, scale), px(MAX_WIDTH, scale));
    let mut y = pad;
    let rows = items
        .iter()
        .map(|item| {
            let height = px(if item.separator { SEPARATOR } else { ROW }, scale);
            let row = Rect::new(pad, y, width - pad, y + height);
            y += height;
            row
        })
        .collect();
    Layout { width, height: y + pad, rows, lead, tail }
}

impl Layout {
    /// The item at (x, y) of a panel `view` pixels high, scrolled by
    /// `scroll`. Only an item that can be selected.
    pub fn hit(&self, items: &[Item], scroll: i32, view: i32, x: i32, y: i32) -> Option<usize> {
        if y < 0 || y >= view {
            return None;
        }
        let at = self.rows.iter().position(|r| r.contains(x, y + scroll))?;
        items.get(at).filter(|i| i.selectable()).map(|_| at)
    }

    /// How far the rows can scroll in a panel `view` pixels high.
    pub fn max_scroll(&self, view: i32) -> i32 {
        (self.height - view).max(0)
    }

    /// The scroll that shows row `index` whole, nearest to `scroll`.
    pub fn scroll_to(&self, index: usize, scroll: i32, view: i32, scale: f32) -> i32 {
        let Some(row) = self.rows.get(index) else { return scroll };
        // The hint bands cover the first and the last rows in view.
        let band = px(SCROLL_HINT, scale);
        let top = row.top - band;
        let bottom = row.bottom + band - view;
        scroll.min(top).max(bottom).clamp(0, self.max_scroll(view))
    }
}

// ---------- placement ----------

/// Which side of the menu is at the anchor's x.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Which side of the menu is at the anchor's y.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Bottom,
}

/// Where a menu opens, in screen pixels: a point and the sides of the menu
/// that touch it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub h: HAlign,
    pub v: VAlign,
}

impl Anchor {
    /// The menu's top-left corner at the point: a right click.
    pub fn point(x: i32, y: i32) -> Self {
        Self { x, y, h: HAlign::Left, v: VAlign::Top }
    }

    pub fn aligned(x: i32, y: i32, h: HAlign, v: VAlign) -> Self {
        Self { x, y, h, v }
    }
}

/// The top-left corner of a panel of `size` for `anchor`, inside `work`.
/// A panel that does not fit on its side of the anchor goes to the other
/// side, as a Windows menu does.
pub fn place(anchor: Anchor, size: (i32, i32), work: Rect) -> (i32, i32) {
    let (w, h) = size;
    let mut x = match anchor.h {
        HAlign::Left => anchor.x,
        HAlign::Center => anchor.x - w / 2,
        HAlign::Right => anchor.x - w,
    };
    if anchor.h == HAlign::Left && x + w > work.right {
        x = anchor.x - w;
    } else if anchor.h == HAlign::Right && x < work.left {
        x = anchor.x;
    }
    let mut y = match anchor.v {
        VAlign::Top => anchor.y,
        VAlign::Bottom => anchor.y - h,
    };
    if anchor.v == VAlign::Top && y + h > work.bottom {
        y = anchor.y - h;
    } else if anchor.v == VAlign::Bottom && y < work.top {
        y = anchor.y;
    }
    (x.min(work.right - w).max(work.left), y.min(work.bottom - h).max(work.top))
}

/// The top-left corner of a submenu of `size`: beside `parent` (the
/// parent's panel on the screen), its first row level with `row` (the row
/// that opens it). It goes to the left of the parent when the right side
/// has no room.
pub fn place_sub(row: Rect, parent: Rect, size: (i32, i32), work: Rect, overlap: i32, pad: i32) -> (i32, i32) {
    let (w, h) = size;
    let right = parent.right - overlap;
    let left = parent.left + overlap - w;
    let x = if right + w <= work.right || left < work.left { right } else { left };
    let y = row.top - pad;
    (x.min(work.right - w).max(work.left), y.min(work.bottom - h).max(work.top))
}

// ---------- keyboard navigation ----------

/// The item after (or before) `from` that can be selected. It goes around
/// the ends. With no `from`: the first (or the last).
pub fn step(items: &[Item], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = items.len();
    if count == 0 {
        return None;
    }
    let start = match (from, forward) {
        (Some(i), true) => i + 1,
        (Some(i), false) => i + count - 1,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    let at = |n: usize| if forward { (start + n) % count } else { (start + count - n) % count };
    (0..count).map(at).find(|i| items[*i].selectable())
}

/// The first (or the last) item that can be selected.
pub fn edge(items: &[Item], first: bool) -> Option<usize> {
    step(items, None, first)
}

/// The next item after `from` whose label starts with `letter`.
pub fn jump(items: &[Item], from: Option<usize>, letter: char) -> Option<usize> {
    let count = items.len();
    let wanted: Vec<char> = letter.to_lowercase().collect();
    let starts = |item: &Item| {
        item.selectable() && item.label.chars().next().is_some_and(|c| c.to_lowercase().eq(wanted.iter().copied()))
    };
    let start = from.map_or(0, |i| i + 1);
    (0..count).map(|n| (start + n) % count).find(|i| starts(&items[*i]))
}

/// What a key does to an open menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Home,
    End,
    Left,
    Right,
    Enter,
    Escape,
    Letter(char),
}

/// What the menu does for a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// The highlight goes to this item of the deepest level.
    Highlight(usize),
    /// The submenu of this item opens, with its first item highlighted.
    OpenSubmenu(usize),
    /// The deepest submenu closes.
    CloseSubmenu,
    /// The menu ends with this item.
    Choose(usize),
    /// The menu ends with no item.
    Cancel,
    Nothing,
}

/// What `key` does in the deepest open level: `items` with `hot`
/// highlighted, `depth` levels open (1 is the root alone).
pub fn key_act(items: &[Item], hot: Option<usize>, depth: usize, key: Key) -> Act {
    let highlight = |to: Option<usize>| to.map_or(Act::Nothing, Act::Highlight);
    let open = |i: usize| match items.get(i) {
        Some(item) if item.selectable() && item.submenu.is_some() => Act::OpenSubmenu(i),
        _ => Act::Nothing,
    };
    match key {
        Key::Down => highlight(step(items, hot, true)),
        Key::Up => highlight(step(items, hot, false)),
        Key::Home => highlight(edge(items, true)),
        Key::End => highlight(edge(items, false)),
        Key::Letter(letter) => highlight(jump(items, hot, letter)),
        Key::Right => hot.map_or(Act::Nothing, open),
        Key::Left if depth > 1 => Act::CloseSubmenu,
        Key::Left => Act::Nothing,
        Key::Escape if depth > 1 => Act::CloseSubmenu,
        Key::Escape => Act::Cancel,
        Key::Enter => match hot.and_then(|i| items.get(i).map(|item| (i, item))) {
            Some((i, item)) if item.selectable() && item.submenu.is_some() => Act::OpenSubmenu(i),
            Some((i, item)) if item.selectable() => Act::Choose(i),
            _ => Act::Nothing,
        },
    }
}

// ---------- the frame: edge, ring and shadow ----------

/// The panel in the window's pixels: a rectangle with one corner radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

impl Shape {
    /// Signed distance from (x, y) to the edge: negative inside.
    pub fn distance(&self, x: f32, y: f32) -> f32 {
        let (hw, hh) = (self.width / 2.0, self.height / 2.0);
        let r = self.radius.min(hw).min(hh).max(0.0);
        let qx = (x - self.left - hw).abs() - (hw - r);
        let qy = (y - self.top - hh).abs() - (hh - r);
        (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
    }

    fn shifted(&self, dy: f32, grow: f32) -> Self {
        Self {
            left: self.left - grow,
            top: self.top + dy - grow,
            width: (self.width + 2.0 * grow).max(0.0),
            height: (self.height + 2.0 * grow).max(0.0),
            radius: (self.radius + grow).max(0.0),
        }
    }
}

/// One layer of the shadow, as a CSS box-shadow, in px before the scale.
struct Drop {
    y: f32,
    blur: f32,
    spread: f32,
    alpha: f32,
}

/// `shadow-pop` (what floats over the page): a contact shadow under the
/// edge and a long soft drop.
const SHADOW: [Drop; 2] = [
    Drop { y: 1.0, blur: 3.0, spread: 0.0, alpha: 0.06 },
    Drop { y: 12.0, blur: 32.0, spread: -8.0, alpha: lift::POP_DROP },
];

/// How far past its own edge a layer reaches, in blurs: at 2.5 sigmas the
/// tail is under a 255th, so the cut there leaves no line.
const SHADOW_TAIL: f32 = 1.25;

/// Room around the panel for its shadow, in device pixels: at each side,
/// above and below. The shadow falls down, so it needs less room above.
pub fn margins(scale: f32) -> (i32, i32, i32) {
    let (mut side, mut top, mut bottom) = (0.0f32, 0.0f32, 0.0f32);
    for drop in &SHADOW {
        let reach = (drop.blur * SHADOW_TAIL + drop.spread) * scale;
        side = side.max(reach);
        top = top.max(reach - drop.y * scale);
        bottom = bottom.max(reach + drop.y * scale);
    }
    // The ring is one pixel outside the edge.
    let room = |v: f32| v.max(0.0).ceil() as i32 + 2;
    (room(side), room(top), room(bottom))
}

/// Half of erfc(t), from a table: the shadow asks for it twice for each
/// pixel, and `exp` was most of the frame's cost. The table is made for
/// one frame and is on the stack.
struct Tail([f32; Tail::STEPS + 1]);

impl Tail {
    const STEPS: usize = 256;
    const SPAN: f32 = 3.0;

    fn new() -> Self {
        let mut table = [0.0; Self::STEPS + 1];
        for (i, slot) in table.iter_mut().enumerate() {
            let t = (i as f32 / Self::STEPS as f32 * 2.0 - 1.0) * Self::SPAN;
            *slot = 0.5 * gfx::erfc(t);
        }
        Self(table)
    }

    fn at(&self, t: f32) -> f32 {
        let at = (t / Self::SPAN + 1.0) * 0.5 * Self::STEPS as f32;
        if at <= 0.0 {
            return 1.0;
        }
        if at >= Self::STEPS as f32 {
            return 0.0;
        }
        let (low, part) = (at as usize, at.fract());
        self.0[low] + (self.0[low + 1] - self.0[low]) * part
    }
}

/// The colours of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub fill: Rgb,
    pub ring: (Rgb, f32),
    /// How much of the shadow's opacity shows (1 on a light ground).
    pub shadow: f32,
}

impl Look {
    pub fn of(palette: &Palette) -> Self {
        // A black ring and a black shadow do not show on a dark page: the
        // ring turns light and the shadow is stronger, as for the field.
        let (ring, shadow) = if palette.dark {
            ((Rgb(255, 255, 255), lift::POP_RING * 1.5), 1.6)
        } else {
            ((Rgb(0, 0, 0), lift::POP_RING), 1.0)
        };
        Self { fill: palette.field, ring, shadow }
    }
}

/// The frame at one pixel centre, premultiplied BGRA: the edge of the
/// panel, the ring one pixel outside it and the shadow. `None` more than
/// one pixel inside the panel, where the rows are.
fn frame_pixel(shape: Shape, look: Look, scale: f32) -> impl Fn(f32, f32) -> Option<u32> {
    let layers = SHADOW.map(|drop| {
        let k = 1.0 / (drop.blur * scale / 2.0 * std::f32::consts::SQRT_2);
        (shape.shifted(drop.y * scale, drop.spread * scale), k, (drop.alpha * look.shadow).min(1.0))
    });
    let pack = |a: f32, r: f32, g: f32, b: f32| -> u32 {
        // A cast rounds down: half a step is added first.
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        c(a) << 24 | c(r) << 16 | c(g) << 8 | c(b)
    };
    let f = |v: u8| v as f32 / 255.0;
    let (fr, fg, fb) = (f(look.fill.0), f(look.fill.1), f(look.fill.2));
    let (rr, rg, rb) = (f(look.ring.0.0), f(look.ring.0.1), f(look.ring.0.2));
    let tail = Tail::new();
    move |x, y| {
        let d = shape.distance(x, y);
        if d < -1.0 {
            return None;
        }
        let cover = (0.5 - d).clamp(0.0, 1.0);
        let ring = ((1.5 - d).clamp(0.0, 1.0) - cover) * look.ring.1;
        let clear: f32 = layers.iter().map(|(s, k, alpha)| 1.0 - alpha * tail.at(s.distance(x, y) * k)).product();
        // The shadow, the ring over it, then the panel's ground over both.
        let mut a = 1.0 - clear;
        a = ring + a * (1.0 - ring);
        let (mut r, mut g, mut b) = (rr * ring, rg * ring, rb * ring);
        a = cover + a * (1.0 - cover);
        r = fr * cover + r * (1.0 - cover);
        g = fg * cover + g * (1.0 - cover);
        b = fb * cover + b * (1.0 - cover);
        Some(pack(a, r, g, b))
    }
}

/// Draws the frame into `pixels` (`width` in a row) around the rows that
/// are there already. The panel has the same room at its left and at its
/// right. Returns
/// the pixels it set inside the panel's bounds (index and value): a later
/// paint of the rows puts them back.
///
/// Away from the corners, each column of a row and each row of a column is
/// the same, and the left half mirrors the right half. Only one corner
/// block and two lines are calculated: the cost does not grow with the
/// size of the menu.
pub fn frame_pixels(pixels: &mut [u32], width: usize, shape: Shape, look: Look, scale: f32) -> Vec<(u32, u32)> {
    let mut edge = Vec::new();
    if width == 0 {
        return edge;
    }
    let height = pixels.len() / width;
    let pixel = frame_pixel(shape, look, scale);
    let reach = SHADOW.iter().map(|d| (d.y.abs() + d.spread.abs()) * scale).fold(0.0f32, f32::max);
    let pad = (shape.radius + reach + 2.0).ceil() as usize;
    let (left, top) = (shape.left.max(0.0) as usize, shape.top.max(0.0) as usize);
    let (right, bottom) = ((left + shape.width as usize).min(width), (top + shape.height as usize).min(height));
    // Columns x0..x1 and rows y0..y1 are away from every corner.
    let (x0, x1) = ((left + pad).min(width / 2), width - (left + pad).min(width / 2));
    let (y0, y1) = (top + pad, bottom.saturating_sub(pad));
    let mut put = |pixels: &mut [u32], x: usize, y: usize, value: u32| {
        pixels[y * width + x] = value;
        if x >= left && x < right && y >= top && y < bottom {
            edge.push(((y * width + x) as u32, value));
        }
    };
    let line = |y: usize| -> Vec<Option<u32>> { (0..x0).map(|x| pixel(x as f32 + 0.5, y as f32 + 0.5)).collect() };
    let middle_line = if y1 > y0 { line(y0) } else { Vec::new() };
    for y in 0..height {
        let middle = y >= y0 && y < y1;
        let side = if middle { None } else { Some(line(y)) };
        let side = side.as_ref().unwrap_or(&middle_line);
        for (x, value) in side.iter().enumerate() {
            if let Some(value) = *value {
                put(pixels, x, y, value);
                put(pixels, width - 1 - x, y, value);
            }
        }
        if !middle
            && x1 > x0
            && let Some(value) = pixel(x0 as f32 + 0.5, y as f32 + 0.5)
        {
            for x in x0..x1 {
                put(pixels, x, y, value);
            }
        }
    }
    edge
}

// ---------- the windows ----------

const WM_MENU_DISMISS: u32 = WM_APP + 70;
const WM_MENU_KEY: u32 = WM_APP + 71;
const WM_MENU_WHEEL: u32 = WM_APP + 72;
const TIMER_SUBMENU: usize = 1;
/// `OBJID_CLIENT`, `CHILDID_SELF` and the events a screen reader listens to.
const OBJID_CLIENT: i32 = -4;
const CHILDID_SELF: i32 = 0;
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MENUPOPUPSTART: u32 = 0x0006;
const EVENT_SYSTEM_MENUPOPUPEND: u32 = 0x0007;
const EVENT_OBJECT_FOCUS: u32 = 0x8005;
const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;
const WINEVENT_OUTOFCONTEXT: u32 = 0;

/// A 32-bit bitmap in a memory DC: what a level's window shows.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    len: usize,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, bitmap.into());
            LIVE.with(|l| l.set((l.get().0, l.get().1 + 1)));
            Some(Self { dc, bitmap, old, bits: bits as *mut u32, len: (width * height) as usize })
        }
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
}

impl std::ops::Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
        LIVE.with(|l| l.set((l.get().0, l.get().1 - 1)));
    }
}

/// One open level: its window and what it shows.
struct Level {
    hwnd: HWND,
    menu: Menu,
    layout: Layout,
    /// The bitmap, until the level starts to fade out.
    canvas: Option<Canvas>,
    /// The frame's pixels inside the panel's bounds (refer to `frame_pixels`).
    edge: Vec<(u32, u32)>,
    /// The window on the screen, and the panel in the window's pixels.
    origin: POINT,
    size: SIZE,
    panel: Rect,
    scroll: i32,
    hot: Option<usize>,
    /// The item whose submenu is open.
    child: Option<usize>,
}

impl Level {
    fn view(&self) -> i32 {
        self.panel.height()
    }

    fn panel_on_screen(&self) -> Rect {
        let (x, y) = (self.origin.x + self.panel.left, self.origin.y + self.panel.top);
        Rect::new(x, y, x + self.panel.width(), y + self.panel.height())
    }

    /// Row `index` in the window's pixels.
    fn row(&self, index: usize) -> Rect {
        let r = self.layout.rows[index];
        let (dx, dy) = (self.panel.left, self.panel.top - self.scroll);
        Rect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy)
    }

    fn row_on_screen(&self, index: usize) -> Rect {
        let r = self.row(index);
        Rect::new(r.left + self.origin.x, r.top + self.origin.y, r.right + self.origin.x, r.bottom + self.origin.y)
    }
}

impl std::ops::Drop for Level {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        LIVE.with(|l| l.set((l.get().0 - 1, l.get().1)));
    }
}

/// Where a new level goes.
enum Place {
    Root(Anchor),
    /// Beside item `1` of level `0`.
    Sub(usize, usize),
}

/// An open menu, from `track` until the person selects an item or leaves.
struct Session {
    owner: HWND,
    palette: Palette,
    scale: f32,
    topmost: bool,
    /// A screen reader runs: the menu tells it what opens and what has
    /// the highlight. Each event takes about a millisecond when a program
    /// listens, so no event goes out without a reader.
    reader: bool,
    levels: Vec<Level>,
    surface: Surface,
    /// `Some` ends the loop: the id selected, or 0.
    result: Option<usize>,
    /// Where the pointer was at the last move, and when the menu opened.
    pointer: POINT,
    opened_at: POINT,
    /// A button release selects: the pointer moved, or a button went down
    /// in the menu. The release of the click that opened the menu does not.
    armed: bool,
    /// The level and the item that the submenu timer waits for.
    pending: Option<(usize, usize)>,
}

/// The levels of a menu that ended, while they fade out.
struct Closing {
    levels: Vec<Level>,
    level: Tween<f32>,
}

/// What the script control reads about the menus.
#[derive(Clone, Copy, Default)]
struct Stats {
    opens: u64,
    /// From the call of `track` to the first frame on the screen.
    first_frame_us: u64,
    worst_first_frame_us: u64,
    /// From the click (refer to `clicked`) to the first frame.
    click_to_frame_us: u64,
    worst_click_to_frame_us: u64,
}

thread_local! {
    static PALETTE: Cell<Option<Palette>> = const { Cell::new(None) };
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static CLOSING: RefCell<Option<Closing>> = const { RefCell::new(None) };
    /// The panels on the screen, for the hooks and the hit test: they
    /// cannot borrow the session, which a handler can hold at that time.
    static PANELS: RefCell<Vec<(isize, RECT)>> = const { RefCell::new(Vec::new()) };
    /// The root level's window and the owner's root window, while a menu
    /// is open.
    static ROOT: Cell<(isize, isize)> = const { Cell::new((0, 0)) };
    /// Windows and bitmaps alive.
    static LIVE: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    static STATS: Cell<Stats> = const { Cell::new(Stats {
        opens: 0,
        first_frame_us: 0,
        worst_first_frame_us: 0,
        click_to_frame_us: 0,
        worst_click_to_frame_us: 0,
    }) };
    static CLICK_US: Cell<Option<u64>> = const { Cell::new(None) };
    /// The hooks that are set now: mouse, keyboard, window in front.
    static HOOKS: Cell<(bool, bool, bool)> = const { Cell::new((false, false, false)) };
}

/// The colours the menus use: the app gives them when the theme changes.
pub fn set_palette(palette: &Palette) {
    PALETTE.with(|p| p.set(Some(*palette)));
}

/// The click that opens a menu arrived now: the time to the first frame is
/// measured from here. A stamp that no menu used stays, so the first of
/// two calls for one click counts.
pub fn clicked() {
    let now = crate::diag::now_us();
    CLICK_US.with(|c| {
        if c.get().is_none_or(|at| now.saturating_sub(at) > 2_000_000) {
            c.set(Some(now));
        }
    });
}

fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| s.try_borrow_mut().ok().and_then(|mut guard| guard.as_mut().map(f)))
}

fn work_area(x: i32, y: i32) -> Rect {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            return Rect::new(r.left, r.top, r.right, r.bottom);
        }
    }
    Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
}

/// The scale of the monitor at (x, y): a menu can open on a monitor that
/// is not the window's.
fn scale_at(x: i32, y: i32, owner: HWND) -> f32 {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() && dpi_x > 0 {
            return design::scale_for_dpi(dpi_x);
        }
    }
    design::window_scale(owner)
}

fn root_of(hwnd: HWND) -> isize {
    unsafe { GetAncestor(hwnd, GA_ROOTOWNER).0 as isize }
}

/// Whether the owner's window has the keyboard: the menu takes the keys
/// only then.
fn owner_in_front() -> bool {
    let (_, owner) = ROOT.with(Cell::get);
    owner != 0 && root_of(unsafe { GetForegroundWindow() }) == owner
}

fn register_class() {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(menu_proc),
            hInstance: instance.into(),
            lpszClassName: w!("NeaMenu"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
    }
}

fn label_font(scale: f32) -> Font {
    Font::new(size::TEXT * scale, size::REGULAR, Align::Leading)
}

fn note_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Leading)
}

fn shortcut_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Trailing)
}

fn text_widths(menu: &Menu, scale: f32) -> Vec<(f32, f32)> {
    menu.items
        .iter()
        .map(|item| {
            let font = if item.note { note_font(scale) } else { label_font(scale) };
            let label = if item.separator { 0.0 } else { gfx::measure(&item.label, font) };
            let shortcut =
                if item.shortcut.is_empty() { 0.0 } else { gfx::measure(&item.shortcut, shortcut_font(scale)) };
            (label, shortcut)
        })
        .collect()
}

impl Session {
    /// Makes the window of a new level, draws it and shows it.
    fn open(&mut self, menu: Menu, place_at: Place) -> bool {
        let started = Instant::now();
        let scale = self.scale;
        let layout = layout(&menu.items, &text_widths(&menu, scale), scale);
        let measured = started.elapsed();
        let (near_x, near_y) = match &place_at {
            Place::Root(anchor) => (anchor.x, anchor.y),
            Place::Sub(level, item) => {
                let row = self.levels[*level].row_on_screen(*item);
                (row.right, row.top)
            }
        };
        let work = work_area(near_x, near_y);
        let view = layout.height.min(work.height().max(px(ROW, scale)));
        let size = (layout.width, view);
        let (x, y) = match place_at {
            Place::Root(anchor) => place(anchor, size, work),
            Place::Sub(level, item) => {
                let parent = &self.levels[level];
                place_sub(
                    parent.row_on_screen(item),
                    parent.panel_on_screen(),
                    size,
                    work,
                    px(OVERLAP, scale),
                    px(PAD, scale),
                )
            }
        };
        let (side, above, below) = margins(scale);
        let origin = POINT { x: x - side, y: y - above };
        let window = SIZE { cx: size.0 + 2 * side, cy: size.1 + above + below };
        register_class();
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if self.topmost {
            ex |= WS_EX_TOPMOST;
        }
        let hwnd = unsafe {
            let Ok(instance) = GetModuleHandleW(None) else { return false };
            CreateWindowExW(
                ex,
                w!("NeaMenu"),
                None,
                WS_POPUP,
                origin.x,
                origin.y,
                window.cx,
                window.cy,
                Some(self.owner),
                None,
                Some(instance.into()),
                None,
            )
        };
        let Ok(hwnd) = hwnd else { return false };
        let made = started.elapsed();
        LIVE.with(|l| l.set((l.get().0 + 1, l.get().1)));
        let mut level = Level {
            hwnd,
            menu,
            layout,
            canvas: None,
            edge: Vec::new(),
            origin,
            size: window,
            panel: Rect::new(side, above, side + size.0, above + size.1),
            scroll: 0,
            hot: None,
            child: None,
        };
        let Some(canvas) = Canvas::new(window.cx, window.cy) else { return false };
        level.canvas = Some(canvas);
        let ready = started.elapsed();
        draw(&mut self.surface, &mut level, &self.palette, scale, None);
        let drawn = started.elapsed();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let shown = started.elapsed();
        self.tell(EVENT_SYSTEM_MENUPOPUPSTART, hwnd);
        crate::diag::trace(|| {
            let us = |d: std::time::Duration| d.as_micros();
            format!(
                "menu: level open in {} us (text {}, window {}, bitmap {}, draw {}, show {})",
                us(shown),
                us(measured),
                us(made - measured),
                us(ready - made),
                us(drawn - ready),
                us(shown - drawn),
            )
        });
        self.levels.push(level);
        self.sync_panels();
        true
    }

    /// Sends an accessibility event for a level, when a screen reader runs.
    fn tell(&self, event: u32, hwnd: HWND) {
        if self.reader {
            unsafe { NotifyWinEvent(event, hwnd, OBJID_CLIENT, CHILDID_SELF) };
        }
    }

    fn sync_panels(&self) {
        PANELS.with(|p| {
            *p.borrow_mut() = self.levels.iter().map(|l| (l.hwnd.0 as isize, l.panel_on_screen().win())).collect();
        });
    }

    fn level_of(&self, hwnd: HWND) -> Option<usize> {
        self.levels.iter().position(|l| l.hwnd == hwnd)
    }

    /// Closes every level after `keep`.
    fn close_after(&mut self, keep: usize) {
        if self.levels.len() <= keep + 1 {
            return;
        }
        for level in &self.levels[keep + 1..] {
            self.tell(EVENT_SYSTEM_MENUPOPUPEND, level.hwnd);
        }
        self.levels.truncate(keep + 1);
        self.levels[keep].child = None;
        self.sync_panels();
    }

    /// Moves the highlight of `level` and draws the rows that changed.
    fn set_hot(&mut self, level: usize, hot: Option<usize>) {
        let scale = self.scale;
        let Some(l) = self.levels.get_mut(level) else { return };
        if l.hot == hot {
            return;
        }
        let before = l.hot;
        l.hot = hot;
        let scroll = hot.map_or(l.scroll, |i| l.layout.scroll_to(i, l.scroll, l.view(), scale));
        if scroll != l.scroll {
            l.scroll = scroll;
            draw(&mut self.surface, l, &self.palette, scale, None);
        } else {
            let rows = [before, hot].into_iter().flatten().map(|i| l.row(i));
            let dirty = rows.reduce(|a, b| {
                Rect::new(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))
            });
            draw(&mut self.surface, l, &self.palette, scale, dirty);
        }
        if self.reader
            && let Some(item) = hot.and_then(|i| l.menu.items.get(i))
        {
            announce(l.hwnd, item);
        }
    }

    /// Opens the submenu of `item` in `level`, when it has one.
    fn open_submenu(&mut self, level: usize, item: usize, highlight_first: bool) {
        self.close_after(level);
        let Some(menu) = self.levels.get(level).and_then(|l| l.menu.items.get(item)).and_then(|i| i.submenu.clone())
        else {
            return;
        };
        if menu.items.is_empty() {
            return;
        }
        self.set_hot(level, Some(item));
        if self.open(menu, Place::Sub(level, item)) {
            self.levels[level].child = Some(item);
            if highlight_first {
                let deepest = self.levels.len() - 1;
                let first = edge(&self.levels[deepest].menu.items, true);
                self.set_hot(deepest, first);
            }
        }
    }

    fn wait_for(&mut self, pending: Option<(usize, usize)>) {
        if self.pending == pending {
            return;
        }
        self.pending = pending;
        let Some(root) = self.levels.first().map(|l| l.hwnd) else { return };
        unsafe {
            match pending {
                Some(_) => {
                    SetTimer(Some(root), TIMER_SUBMENU, SUBMENU_DELAY_MS, None);
                }
                None => {
                    let _ = KillTimer(Some(root), TIMER_SUBMENU);
                }
            }
        }
    }

    /// The pointer rested on an item: its submenu opens, and a submenu of
    /// a different item closes.
    fn rested(&mut self) {
        let Some((level, item)) = self.pending else { return };
        self.wait_for(None);
        if self.levels.get(level).is_none_or(|l| l.hot != Some(item) || l.child == Some(item)) {
            return;
        }
        self.close_after(level);
        self.open_submenu(level, item, false);
    }

    /// The pointer at (x, y) of the window of `level`.
    fn pointer_at(&mut self, level: usize, x: i32, y: i32) {
        let Some(l) = self.levels.get(level) else { return };
        let screen = POINT { x: l.origin.x + x, y: l.origin.y + y };
        // A window that shows under a pointer at rest gets a move too.
        if screen == self.pointer {
            return;
        }
        self.pointer = screen;
        if (screen.x - self.opened_at.x).abs() >= ARM_SLOP || (screen.y - self.opened_at.y).abs() >= ARM_SLOP {
            self.armed = true;
        }
        let hit = l.layout.hit(&l.menu.items, l.scroll, l.view(), x - l.panel.left, y - l.panel.top);
        let child = l.child;
        // The levels above keep the item that leads here.
        for above in 0..level {
            let lead = self.levels[above].child;
            self.set_hot(above, lead);
        }
        match hit {
            Some(item) => {
                self.set_hot(level, Some(item));
                self.wait_for((child != Some(item)).then_some((level, item)));
            }
            // Between two items (a separator, the padding): nothing changes.
            None => self.wait_for(None),
        }
    }

    /// A button went up at (x, y) of the window of `level`.
    fn released(&mut self, level: usize, x: i32, y: i32) {
        if !self.armed {
            return;
        }
        let Some(l) = self.levels.get(level) else { return };
        let Some(item) = l.layout.hit(&l.menu.items, l.scroll, l.view(), x - l.panel.left, y - l.panel.top) else {
            return;
        };
        let chosen = &l.menu.items[item];
        if chosen.submenu.is_some() {
            if l.child != Some(item) {
                self.wait_for(None);
                self.open_submenu(level, item, false);
            }
        } else {
            self.result = Some(chosen.id);
        }
    }

    fn key(&mut self, key: Key) {
        let Some(deepest) = self.levels.len().checked_sub(1) else { return };
        let level = &self.levels[deepest];
        let act = key_act(&level.menu.items, level.hot, deepest + 1, key);
        self.wait_for(None);
        match act {
            Act::Highlight(item) => self.set_hot(deepest, Some(item)),
            Act::OpenSubmenu(item) => self.open_submenu(deepest, item, true),
            Act::CloseSubmenu => self.close_after(deepest - 1),
            Act::Choose(item) => self.result = Some(self.levels[deepest].menu.items[item].id),
            Act::Cancel => self.result = Some(0),
            Act::Nothing => {}
        }
    }

    /// The wheel over the window of `level`: a list too long for the
    /// screen scrolls.
    fn wheel(&mut self, level: usize, delta: i32) {
        let scale = self.scale;
        let Some(l) = self.levels.get_mut(level) else { return };
        let step = px(ROW, scale) * 3 * delta / 120;
        let scroll = (l.scroll - step).clamp(0, l.layout.max_scroll(l.view()));
        if scroll != l.scroll {
            l.scroll = scroll;
            draw(&mut self.surface, l, &self.palette, scale, None);
        }
    }

    fn state(&self) -> Value {
        let rect = |r: Rect| json!([r.left, r.top, r.right, r.bottom]);
        let levels: Vec<Value> = self
            .levels
            .iter()
            .map(|l| {
                let items: Vec<Value> = l
                    .menu
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        json!({
                            "id": item.id,
                            "label": item.label,
                            "shortcut": item.shortcut,
                            "separator": item.separator,
                            "enabled": item.enabled,
                            "note": item.note,
                            "checked": item.checked,
                            "swatch": item.swatch.map(|c| [c.0, c.1, c.2]),
                            "submenu": item.submenu.is_some(),
                            "rect": rect(l.row_on_screen(i)),
                        })
                    })
                    .collect();
                json!({
                    "hwnd": l.hwnd.0 as isize,
                    "window": [l.origin.x, l.origin.y, l.size.cx, l.size.cy],
                    "panel": rect(l.panel_on_screen()),
                    "hot": l.hot,
                    "child": l.child,
                    "scroll": l.scroll,
                    "items": items,
                })
            })
            .collect();
        json!(levels)
    }
}

/// Whether a screen reader runs (Narrator, NVDA and JAWS set this flag).
fn screen_reader_runs() -> bool {
    let mut on = windows::core::BOOL(0);
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETSCREENREADER,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    read.is_ok() && on.as_bool()
}

/// Tells a screen reader which item has the highlight. The window's text
/// is the item's name: Windows gives a window with no accessibility code
/// of its own a standard object whose name is that text.
fn announce(hwnd: HWND, item: &Item) {
    let mut name = item.label.clone();
    match item.checked {
        Some(true) => name.push_str(", checked"),
        Some(false) => name.push_str(", not checked"),
        None => {}
    }
    if item.submenu.is_some() {
        name.push_str(", submenu");
    }
    if !item.shortcut.is_empty() {
        name.push_str(", ");
        name.push_str(&item.shortcut);
    }
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(name));
        NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, hwnd, OBJID_CLIENT, CHILDID_SELF);
        NotifyWinEvent(EVENT_OBJECT_FOCUS, hwnd, OBJID_CLIENT, CHILDID_SELF);
    }
}

// ---------- drawing ----------

fn rect_f(r: Rect) -> RectF {
    RectF::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32)
}

/// Draws `level` into its bitmap and puts the bitmap on the screen. With
/// `dirty` (the window's pixels) only those rows are drawn again.
fn draw(surface: &mut Surface, level: &mut Level, palette: &Palette, scale: f32, dirty: Option<Rect>) {
    let started = Instant::now();
    let panel = level.panel;
    let bound = match dirty {
        Some(d) => Rect::new(
            d.left.max(panel.left),
            d.top.max(panel.top),
            d.right.min(panel.right),
            d.bottom.min(panel.bottom),
        ),
        None => panel,
    };
    let Some(dc) = level.canvas.as_ref().map(|c| c.dc) else { return };
    if bound.right > bound.left && bound.bottom > bound.top {
        let (l, pal) = (&*level, palette);
        surface.paint(dc, bound.win(), |g| {
            g.fill(rect_f(bound), pal.field);
            for (i, item) in l.menu.items.iter().enumerate() {
                let row = rect_f(l.row(i));
                if g.visible(row) {
                    paint_row(g, item, row, &l.layout, l.hot == Some(i), pal, scale);
                }
            }
            paint_scroll_hints(g, l, pal, scale);
        });
    }
    let painted = started.elapsed();
    let Some(canvas) = level.canvas.as_mut() else { return };
    let pixels = canvas.pixels_mut();
    if dirty.is_none() {
        let shape = Shape {
            left: panel.left as f32,
            top: panel.top as f32,
            width: panel.width() as f32,
            height: panel.height() as f32,
            radius: size::RADIUS_PANEL * scale,
        };
        level.edge = frame_pixels(pixels, level.size.cx as usize, shape, Look::of(palette), scale);
    } else {
        for (index, value) in &level.edge {
            pixels[*index as usize] = *value;
        }
    }
    let framed = started.elapsed();
    let blend = blend(255);
    let zero = POINT::default();
    unsafe {
        let _ = UpdateLayeredWindow(
            level.hwnd,
            None,
            Some(&level.origin),
            Some(&level.size),
            Some(dc),
            Some(&zero),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
    anim::mark("menu");
    let took = started.elapsed();
    anim::report_frame(took);
    crate::diag::trace(|| {
        let us = |d: std::time::Duration| d.as_micros();
        let (rows, frame, put) = (us(painted), us(framed - painted), us(took - framed));
        format!("menu: drawn in {} us (rows {rows}, frame {frame}, to the screen {put})", us(took))
    });
}

fn blend(alpha: u8) -> BLENDFUNCTION {
    BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    }
}

fn paint_row(g: &Painter, item: &Item, row: RectF, layout: &Layout, hot: bool, pal: &Palette, scale: f32) {
    if item.separator {
        // The line goes from one edge of the panel to the other.
        let pad = PAD * scale;
        let thick = scale.round().max(1.0);
        let y = ((row.top + row.bottom - thick) / 2.0).round();
        g.fill(RectF::new(row.left - pad, y, row.right + pad, y + thick), pal.line);
        return;
    }
    if hot && item.selectable() {
        g.round(row, size::RADIUS * scale, pal.selection);
    }
    let inset = INSET * scale;
    let ink = if item.note {
        pal.ink_muted
    } else if item.enabled {
        pal.ink
    } else {
        pal.ink_asleep
    };
    let quiet = if item.enabled && !item.note { pal.ink_muted } else { pal.ink_asleep };
    let mark = MARK * scale;
    let middle = (row.top + row.bottom) / 2.0;
    let check = |left: f32| {
        let at = |x: f32, y: f32| (left + x * scale, middle + (y - 8.0) * scale);
        g.polyline(&[at(3.5, 8.5), at(6.5, 11.5), at(12.5, 4.5)], 1.5 * scale, ink);
    };
    let tail_left = row.right - inset - layout.tail as f32;
    if let Some(color) = item.swatch {
        let dot = 5.0 * scale;
        let cx = row.left + inset + mark / 2.0;
        g.round(RectF::new(cx - dot, middle - dot, cx + dot, middle + dot), dot, color);
        if item.checked == Some(true) {
            check(row.right - inset - mark);
        }
    } else if item.checked == Some(true) {
        check(row.left + inset);
    }
    if item.submenu.is_some() {
        let left = row.right - inset - mark;
        let at = |x: f32, y: f32| (left + x * scale, middle + (y - 8.0) * scale);
        g.polyline(&[at(6.5, 4.5), at(10.0, 8.0), at(6.5, 11.5)], 1.5 * scale, quiet);
    } else if !item.shortcut.is_empty() {
        g.text(
            &item.shortcut,
            RectF::new(tail_left, row.top, row.right - inset, row.bottom),
            shortcut_font(scale),
            quiet,
        );
    }
    let left = row.left + inset + layout.lead as f32;
    let right = if layout.tail > 0 { tail_left - TAIL_GAP * scale / 2.0 } else { row.right - inset };
    let font = if item.note { note_font(scale) } else { label_font(scale) };
    g.text(&item.label, RectF::new(left, row.top, right, row.bottom), font, ink);
}

/// A band with an arrow at the top and at the bottom of a list that
/// scrolls, where rows are out of view.
fn paint_scroll_hints(g: &Painter, level: &Level, pal: &Palette, scale: f32) {
    let max = level.layout.max_scroll(level.view());
    if max == 0 {
        return;
    }
    let panel = rect_f(level.panel);
    let band = SCROLL_HINT * scale;
    let cx = (panel.left + panel.right) / 2.0;
    let arrow = |cy: f32, up: bool| {
        let (dx, dy) = (4.0 * scale, if up { -2.0 * scale } else { 2.0 * scale });
        g.polyline(&[(cx - dx, cy - dy), (cx, cy + dy), (cx + dx, cy - dy)], 1.5 * scale, pal.ink_muted);
    };
    if level.scroll > 0 {
        g.fill(RectF::new(panel.left, panel.top, panel.right, panel.top + band), pal.field);
        arrow(panel.top + band / 2.0, true);
    }
    if level.scroll < max {
        g.fill(RectF::new(panel.left, panel.bottom - band, panel.right, panel.bottom), pal.field);
        arrow(panel.bottom - band / 2.0, false);
    }
}

// ---------- the loop ----------

/// Shows `menu` at `at` (screen pixels) and waits for the person: returns
/// the id of the item selected, or 0. The thread dispatches every message
/// in the meantime. `owner` is the window the menu belongs to.
pub fn track(owner: HWND, menu: Menu, at: Anchor) -> usize {
    if menu.items.is_empty() || SESSION.with(|s| s.try_borrow().map_or(true, |s| s.is_some())) {
        return 0;
    }
    let started = crate::diag::now_us();
    // A menu that still fades out goes at once: one set of windows at most.
    CLOSING.with(|c| c.borrow_mut().take());
    let palette = PALETTE.with(Cell::get).unwrap_or_else(|| design::palette(crate::settings::Theme::System));
    let owner_root = root_of(owner);
    let in_front = root_of(unsafe { GetForegroundWindow() }) == owner_root;
    let mut pointer = POINT::default();
    unsafe {
        // The owner can hold the mouse (a press that opened the menu).
        let _ = ReleaseCapture();
        let _ = GetCursorPos(&mut pointer);
    }
    let mut session = Session {
        owner,
        palette,
        scale: scale_at(at.x, at.y, owner),
        // A test run behind the person's own windows stays behind them.
        topmost: in_front,
        reader: screen_reader_runs(),
        levels: Vec::new(),
        surface: Surface::default(),
        result: None,
        pointer,
        opened_at: pointer,
        armed: false,
        pending: None,
    };
    if !session.open(menu, Place::Root(at)) {
        return 0;
    }
    let shown = crate::diag::now_us();
    let click = CLICK_US.with(Cell::take).filter(|at| shown.saturating_sub(*at) < 2_000_000);
    STATS.with(|s| {
        let mut stats = s.get();
        stats.opens += 1;
        stats.first_frame_us = shown.saturating_sub(started);
        stats.worst_first_frame_us = stats.worst_first_frame_us.max(stats.first_frame_us);
        if let Some(click) = click {
            stats.click_to_frame_us = shown.saturating_sub(click);
            stats.worst_click_to_frame_us = stats.worst_click_to_frame_us.max(stats.click_to_frame_us);
        }
        s.set(stats);
    });
    crate::diag::trace(|| format!("menu: first frame {} us after the call", shown.saturating_sub(started)));
    let root = session.levels[0].hwnd;
    ROOT.with(|r| r.set((root.0 as isize, owner_root)));
    SESSION.with(|s| *s.borrow_mut() = Some(session));
    let hooks = Hooks::install();
    // From here the time is the person's, who reads the menu.
    let modal = crate::diag::modal();
    run_loop(owner);
    drop(modal);
    drop(hooks);
    ROOT.with(|r| r.set((0, 0)));
    PANELS.with(|p| p.borrow_mut().clear());
    let Some(mut session) = SESSION.with(|s| s.borrow_mut().take()) else { return 0 };
    unsafe {
        let _ = KillTimer(Some(root), TIMER_SUBMENU);
    }
    for level in &session.levels {
        session.tell(EVENT_SYSTEM_MENUPOPUPEND, level.hwnd);
    }
    let result = session.result.unwrap_or(0);
    fade_out(std::mem::take(&mut session.levels));
    result
}

fn finished() -> bool {
    SESSION.with(|s| s.try_borrow().is_ok_and(|s| s.as_ref().is_none_or(|s| s.result.is_some())))
}

fn dismiss() {
    with_session(|s| s.result = s.result.or(Some(0)));
}

fn run_loop(owner: HWND) {
    let mut msg = MSG::default();
    while !finished() {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            // The quit is for the main loop: it gets it again.
            if got.0 == 0 {
                unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            }
            dismiss();
            break;
        }
        // The owner is asked to close: the menu goes first, and the app is
        // then free to answer.
        if msg.message == WM_CLOSE && msg.hwnd == owner {
            dismiss();
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !unsafe { IsWindow(Some(owner)) }.as_bool() || !unsafe { IsWindowVisible(owner) }.as_bool() {
            dismiss();
        }
    }
}

/// Lets the levels go: with a fade, or at once when Windows has the
/// animations off.
fn fade_out(mut levels: Vec<Level>) {
    if levels.is_empty() || anim::reduced() {
        return;
    }
    for level in &mut levels {
        // Windows has its own copy of the pixels: the bitmap goes now.
        level.canvas = None;
        level.edge = Vec::new();
        unsafe {
            let ex = GetWindowLongW(level.hwnd, GWL_EXSTYLE);
            SetWindowLongW(level.hwnd, GWL_EXSTYLE, ex | WS_EX_TRANSPARENT.0 as i32);
        }
    }
    let level = Tween::new(1.0, 0.0, Instant::now(), specs::MENU_OUT);
    CLOSING.with(|c| *c.borrow_mut() = Some(Closing { levels, level }));
    anim::drive(fade_step);
}

/// One frame of the fade: only the opacity of the windows changes.
fn fade_step() -> bool {
    let now = Instant::now();
    let moving = CLOSING.with(|c| {
        let c = c.borrow();
        let Some(closing) = c.as_ref() else { return false };
        let alpha = (closing.level.value(now).clamp(0.0, 1.0) * 255.0).round() as u8;
        let blend = blend(alpha);
        for level in &closing.levels {
            unsafe {
                let _ =
                    UpdateLayeredWindow(level.hwnd, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA);
            }
        }
        anim::mark("menu");
        closing.level.moving(now)
    });
    if !moving {
        CLOSING.with(|c| c.borrow_mut().take());
    }
    moving
}

// ---------- the hooks ----------

/// The hooks that are set while a menu is open: the mouse (a click outside
/// the menu, the wheel), the keyboard (the menu takes the keys and the page
/// keeps the focus) and the change of the window in front.
struct Hooks {
    mouse: Option<HHOOK>,
    keyboard: Option<HHOOK>,
    foreground: HWINEVENTHOOK,
}

impl Hooks {
    fn install() -> Self {
        let hooks = unsafe {
            let instance = GetModuleHandleW(None).ok().map(Into::into);
            Self {
                mouse: SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), instance, 0).ok(),
                keyboard: SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), instance, 0).ok(),
                foreground: SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(foreground_hook),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                ),
            }
        };
        HOOKS.with(|h| h.set((hooks.mouse.is_some(), hooks.keyboard.is_some(), !hooks.foreground.is_invalid())));
        hooks
    }
}

impl std::ops::Drop for Hooks {
    fn drop(&mut self) {
        unsafe {
            for hook in [self.mouse.take(), self.keyboard.take()].into_iter().flatten() {
                let _ = UnhookWindowsHookEx(hook);
            }
            if !self.foreground.is_invalid() {
                let _ = UnhookWinEvent(self.foreground);
            }
        }
        HOOKS.with(|h| h.set((false, false, false)));
    }
}

fn post_to_root(msg: u32, wparam: usize, lparam: isize) {
    let (root, _) = ROOT.with(Cell::get);
    if root != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(root as *mut _)), msg, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

/// The level whose panel has the screen point, if one has.
fn panel_at(point: POINT) -> Option<isize> {
    PANELS.with(|p| {
        let panels = p.try_borrow().ok()?;
        let inside = |r: &RECT| point.x >= r.left && point.x < r.right && point.y >= r.top && point.y < r.bottom;
        // The deepest level is the last, and it is on top.
        panels.iter().rev().find(|(_, r)| inside(r)).map(|(hwnd, _)| *hwnd)
    })
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        match wparam.0 as u32 {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN if panel_at(info.pt).is_none() => {
                // In a test run the person works in other windows: only a
                // click on the owner's window closes the menu then.
                let (_, owner) = ROOT.with(Cell::get);
                let on_owner = || root_of(unsafe { WindowFromPoint(info.pt) }) == owner;
                if !crate::bench::from_env() || on_owner() {
                    post_to_root(WM_MENU_DISMISS, 0, 0);
                }
            }
            WM_MOUSEWHEEL => {
                if let Some(hwnd) = panel_at(info.pt) {
                    let delta = (info.mouseData >> 16) as i16 as isize;
                    post_to_root(WM_MENU_WHEEL, hwnd as usize, delta);
                    return LRESULT(1);
                }
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The key of a virtual key code, when the menu uses it.
fn key_of(vk: u32) -> Option<Key> {
    let is = |key: VIRTUAL_KEY| vk == key.0 as u32;
    Some(if is(VK_UP) {
        Key::Up
    } else if is(VK_DOWN) {
        Key::Down
    } else if is(VK_HOME) {
        Key::Home
    } else if is(VK_END) {
        Key::End
    } else if is(VK_LEFT) {
        Key::Left
    } else if is(VK_RIGHT) {
        Key::Right
    } else if is(VK_RETURN) || is(VK_SPACE) {
        Key::Enter
    } else if is(VK_ESCAPE) {
        Key::Escape
    } else {
        let unit = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) } & 0xFFFF;
        Key::Letter(char::from_u32(unit).filter(|c| c.is_alphanumeric())?)
    })
}

/// What the keyboard hook does with a key that goes down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    /// The menu gets the key, and nothing else does.
    Menu,
    /// The menu closes, and the key goes where it goes without a menu.
    Leave,
    /// The key goes on, and the menu stays.
    Pass,
}

/// Where a key that goes down goes while a menu is open. `chord`: Ctrl or
/// Alt is down with it.
pub fn key_route(vk: u32, chord: bool) -> KeyRoute {
    match vk {
        // Shift, Ctrl and the lock keys alone do nothing to a menu.
        0x10 | 0x11 | 0x14 | 0x90 | 0x91 | 0xA0..=0xA3 => KeyRoute::Pass,
        // Alt, the Windows keys and F10 start something else.
        0x12 | 0x5B | 0x5C | 0x79 | 0xA4 | 0xA5 => KeyRoute::Leave,
        // Print Screen, Sleep and the media keys are not for the window.
        0x2C | 0x5F | 0xA6..=0xB7 => KeyRoute::Pass,
        // A shortcut of the app or of the system: the menu goes, and the
        // shortcut runs.
        _ if chord => KeyRoute::Leave,
        // Every other key is the menu's, as with a Windows menu: the page
        // under it gets none.
        _ => KeyRoute::Menu,
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    if code == HC_ACTION as i32 && down && lparam.0 != 0 && owner_in_front() {
        let vk = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) }.vkCode;
        let held = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        match key_route(vk, held(VK_CONTROL) || held(VK_MENU)) {
            KeyRoute::Menu => {
                post_to_root(WM_MENU_KEY, vk as usize, 0);
                return LRESULT(1);
            }
            KeyRoute::Leave => post_to_root(WM_MENU_DISMISS, 0, 0),
            KeyRoute::Pass => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn foreground_hook(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
    let (_, owner) = ROOT.with(Cell::get);
    // A different program came in front. In a test run that is the person
    // at work: the menu stays for the script.
    if owner != 0 && root_of(hwnd) != owner && !crate::bench::from_env() {
        post_to_root(WM_MENU_DISMISS, 0, 0);
    }
}

fn x_y(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // What is on the screen came from `UpdateLayeredWindow`. A paint
        // only comes from `PrintWindow` (the capture tools): it gets the
        // bitmap of the level, premultiplied alpha included.
        WM_PAINT | WM_PRINT | WM_PRINTCLIENT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = if msg == WM_PAINT { unsafe { BeginPaint(hwnd, &mut ps) } } else { HDC(wparam.0 as _) };
            with_session(|s| {
                let level = s.levels.iter().find(|l| l.hwnd == hwnd)?;
                let canvas = level.canvas.as_ref()?;
                unsafe {
                    let _ = BitBlt(hdc, 0, 0, level.size.cx, level.size.cy, Some(canvas.dc), 0, 0, SRCCOPY);
                }
                Some(())
            });
            if msg == WM_PAINT {
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            // The shadow around the panel is not the menu.
            let (x, y) = x_y(lparam);
            let on_panel = panel_at(POINT { x, y }) == Some(hwnd.0 as isize);
            LRESULT(if on_panel { HTCLIENT as isize } else { HTTRANSPARENT as isize })
        }
        WM_MOUSEMOVE => {
            let (x, y) = x_y(lparam);
            with_session(|s| s.level_of(hwnd).map(|level| s.pointer_at(level, x, y)));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK => {
            with_session(|s| s.armed = true);
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let (x, y) = x_y(lparam);
            with_session(|s| s.level_of(hwnd).map(|level| s.released(level, x, y)));
            LRESULT(0)
        }
        WM_MENU_KEY => {
            if let Some(key) = key_of(wparam.0 as u32) {
                with_session(|s| s.key(key));
            }
            LRESULT(0)
        }
        WM_MENU_WHEEL => {
            let over = HWND(wparam.0 as *mut _);
            with_session(|s| s.level_of(over).map(|level| s.wheel(level, lparam.0 as i32)));
            LRESULT(0)
        }
        WM_MENU_DISMISS => {
            dismiss();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_SUBMENU => {
            with_session(Session::rested);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ---------- the script control ----------

/// What a script asks of the menus (refer to `bench::protocol::Action::Menu`).
pub struct Script<'a> {
    pub op: &'a str,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub left: bool,
    pub key: Option<&'a str>,
    pub level: Option<usize>,
    pub item: Option<usize>,
}

fn script_key(name: &str) -> Option<Key> {
    Some(match name.to_ascii_lowercase().as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(letter), None) => Key::Letter(letter),
                _ => return None,
            }
        }
    })
}

/// The `menus` key of the script control's state.
pub fn state() -> Value {
    let stats = STATS.with(Cell::get);
    let (windows, bitmaps) = LIVE.with(Cell::get);
    let levels = SESSION.with(|s| s.try_borrow().ok().and_then(|s| s.as_ref().map(Session::state)));
    json!({
        "open": levels.is_some(),
        "levels": levels.unwrap_or_else(|| json!([])),
        "fading": CLOSING.with(|c| c.try_borrow().is_ok_and(|c| c.is_some())),
        "hooks": HOOKS.with(Cell::get),
        "windows": windows,
        "bitmaps": bitmaps,
        "opens": stats.opens,
        "first_frame_us": stats.first_frame_us,
        "worst_first_frame_us": stats.worst_first_frame_us,
        "click_to_frame_us": stats.click_to_frame_us,
        "worst_click_to_frame_us": stats.worst_click_to_frame_us,
    })
}

/// Runs one request of the script control. It needs nothing of the app, so
/// it also runs while a menu is open and the app waits in `track`.
pub fn script(window: HWND, request: &Script) -> Result<Value, String> {
    let open = SESSION.with(|s| s.try_borrow().is_ok_and(|s| s.is_some()));
    let need_open = || if open { Ok(()) } else { Err("no menu is open".to_string()) };
    match request.op {
        "state" => {}
        // A click at (x, y) of the window, as the mouse sends it: the right
        // button, or the left one with `left`.
        "open" => {
            if open {
                return Err("a menu is open".to_string());
            }
            let (Some(x), Some(y)) = (request.x, request.y) else { return Err("open needs x and y".to_string()) };
            let at = LPARAM(((y as u16 as isize) << 16) | (x as u16 as isize));
            clicked();
            unsafe {
                if request.left {
                    let _ = PostMessageW(Some(window), WM_LBUTTONDOWN, WPARAM(1), at);
                    let _ = PostMessageW(Some(window), WM_LBUTTONUP, WPARAM(0), at);
                } else {
                    let _ = PostMessageW(Some(window), WM_RBUTTONUP, WPARAM(0), at);
                }
            }
            return Ok(json!({ "posted": true }));
        }
        "key" => {
            need_open()?;
            let key = request.key.and_then(script_key).ok_or("key needs a key name or one letter")?;
            with_session(|s| s.key(key));
        }
        // The pointer over an item, or a click on it.
        "hover" | "click" => {
            need_open()?;
            let click = request.op == "click";
            let found = with_session(|s| {
                let level = request.level.unwrap_or(s.levels.len().saturating_sub(1));
                let l = s.levels.get(level)?;
                let item = request.item.filter(|i| *i < l.menu.items.len())?;
                let row = l.row(item);
                let (x, y) = ((row.left + row.right) / 2, (row.top + row.bottom) / 2);
                s.pointer_at(level, x, y);
                if click {
                    s.armed = true;
                    s.released(level, x, y);
                }
                Some(())
            });
            if found.flatten().is_none() {
                return Err("no such level or item".to_string());
            }
        }
        "close" => dismiss(),
        other => return Err(format!("unknown menu request: {other}")),
    }
    Ok(state())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/menu.rs"]
#[rustfmt::skip]
mod tests;
