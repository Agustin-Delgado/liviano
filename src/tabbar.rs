//! The tab strip: where each tab sits (pure geometry, tested) and how it is
//! drawn with GDI into one off-screen bitmap per paint.

use windows::Win32::{
    Foundation::RECT,
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS,
        DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, Ellipse, FillRect,
        GetStockObject, HDC, HFONT, NULL_PEN, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    },
};

use crate::design::{Palette, Rgb};

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
    fn inset(&self, dx: i32, dy: i32) -> Self {
        Self::new(self.left + dx, self.top + dy, self.right - dx, self.bottom - dy)
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
    m.px(32) + m.px(6) + shown_marks(pinned, width / 4, m) as i32 * (m.px(28) + m.px(2))
}

/// Pinned extensions that fit in `room` pixels.
fn shown_marks(pinned: usize, room: i32, m: &Metrics) -> usize {
    pinned.min((room / (m.px(28) + m.px(2))).max(0) as usize)
}

/// Horizontal: the button at the right end, pinned ones to its left.
/// Vertical: the button at the bottom left, pinned ones to its right.
fn place_extensions(layout: &mut Layout, pinned: usize, m: &Metrics) {
    let strip = layout.strip;
    let (button_w, mark_w, gap) = (m.px(32), m.px(28), m.px(2));
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
        let (top, bottom) = (m.px(5), strip.bottom - m.px(4));
        let right = strip.right - m.px(6);
        layout.extensions = Rect::new(right - button_w, top, right, bottom);
        let mut x = layout.extensions.left - gap;
        for _ in 0..shown_marks(pinned, strip.width() / 4, m) {
            layout.extension_marks.insert(0, Rect::new(x - mark_w, top, x, bottom));
            x -= mark_w + gap;
        }
    }
}

fn layout_horizontal(tabs: &[TabInfo], width: i32, height: i32, m: &Metrics, extensions: Option<usize>) -> Layout {
    let bar = m.px(38);
    let pad = m.px(6);
    let gap = m.px(2);
    let pinned_w = m.px(34);
    let plus_w = m.px(32);
    let item_top = m.px(5);
    let item_bottom = bar - m.px(4);

    let pinned = tabs.iter().filter(|t| t.pinned).count() as i32;
    let normal = tabs.len() as i32 - pinned;
    let reserved = extensions.map_or(0, |n| extensions_width(n, width, m));
    let available = width - 2 * pad - plus_w - pinned * (pinned_w + gap) - normal * gap - reserved;
    let normal_w = if normal > 0 { (available / normal).clamp(m.px(44), m.px(220)) } else { 0 };

    let mut x = pad;
    let mut items = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let w = if tab.pinned { pinned_w } else { normal_w };
        let rect = Rect::new(x, item_top, x + w, item_bottom);
        let close = (!tab.pinned && w >= m.px(72)).then(|| {
            let size = m.px(18);
            let cy = (rect.top + rect.bottom) / 2;
            Rect::new(rect.right - m.px(6) - size, cy - size / 2, rect.right - m.px(6), cy + size / 2)
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
        let size = m.px(18);
        let cy = (rect.top + rect.bottom) / 2;
        let close = Some(Rect::new(rect.right - m.px(7) - size, cy - size / 2, rect.right - m.px(7), cy + size / 2));
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
    pub hover: Option<Hit>,
    pub palette: &'a Palette,
    pub font: HFONT,
    pub hint: Option<&'a str>,
    pub scale: f32,
    /// Split pairs (left, right), drawn on one shared background.
    pub groups: &'a [(u64, u64)],
    /// Extra flat areas over the content (the split divider and focus line).
    pub fills: &'a [(Rect, Rgb)],
    /// Initials of the pinned extensions, in `layout.extension_marks` order.
    pub extension_initials: &'a [char],
}

/// Draws the strip and, when there is no page on screen, the empty canvas
/// with its hint. Everything goes to a memory DC first: no flicker.
pub fn paint(hdc: HDC, width: i32, height: i32, p: &Paint) {
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, width.max(1), height.max(1));
        let old_bitmap = SelectObject(mem, bitmap.into());
        let old_font = SelectObject(mem, p.font.into());
        SetBkMode(mem, TRANSPARENT);

        fill(mem, Rect::new(0, 0, width, height), p.palette.chrome);
        let px = |v: i32| (v as f32 * p.scale).round() as i32;

        for &(left, right) in p.groups {
            let find = |id: u64| p.layout.items.iter().find(|i| i.id == id).map(|i| i.rect);
            let (Some(a), Some(b)) = (find(left), find(right)) else { continue };
            // Side by side in a row or a column: one shape; otherwise (a grid
            // that wrapped) each its own.
            let shapes = if a.top == b.top || a.left == b.left {
                vec![Rect::new(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))]
            } else {
                vec![a, b]
            };
            for shape in shapes {
                fill(mem, shape.inset(-px(2), -px(2)), p.palette.line);
            }
        }
        // The partner of the active tab is on screen too: its label reads as such.
        let shown_partner = p.active.and_then(|a| {
            p.groups.iter().find_map(|&(l, r)| if l == a { Some(r) } else if r == a { Some(l) } else { None })
        });

        for (item, tab) in p.layout.items.iter().zip(order_like(p.layout, p.tabs)) {
            let active = p.active == Some(item.id);
            let hovered = matches!(p.hover, Some(Hit::Tab(id) | Hit::Close(id)) if id == item.id);
            if active {
                let back = if tab.private { p.palette.private_tab } else { p.palette.tab_active };
                fill(mem, item.rect, back);
            } else if hovered {
                fill(mem, item.rect, p.palette.tab_hover);
            }
            if tab.private {
                let r = item.rect;
                fill(mem, Rect::new(r.left, r.top + px(6), r.left + px(3), r.bottom - px(6)), p.palette.private_mark);
            }
            let ink = if shown_partner == Some(item.id) {
                p.palette.ink
            } else if tab.asleep && !active {
                p.palette.ink_asleep
            } else if active {
                p.palette.ink
            } else {
                p.palette.ink_muted
            };
            SetTextColor(mem, ink.colorref());
            if item.pinned {
                let mut text: Vec<u16> = tab.initial.to_string().encode_utf16().collect();
                let mut r = item.rect.win();
                DrawTextW(mem, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            } else {
                let mut r = item.rect.inset(px(10), 0);
                if item.close.is_some() {
                    r.right -= px(20);
                }
                let mut text: Vec<u16> = tab.label.encode_utf16().collect();
                let mut wr = r.win();
                DrawTextW(
                    mem,
                    &mut text,
                    &mut wr,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
            }
            if let Some(close) = item.close.filter(|_| active || hovered) {
                if p.hover == Some(Hit::Close(item.id)) {
                    fill(mem, close, p.palette.line);
                }
                SetTextColor(mem, p.palette.ink_muted.colorref());
                let mut text: Vec<u16> = "×".encode_utf16().collect();
                let mut r = close.win();
                DrawTextW(mem, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            }
            if tab.loading {
                let r = item.rect;
                fill(mem, Rect::new(r.left + px(8), r.bottom - px(2), r.right - px(8), r.bottom), p.palette.accent);
            }
        }

        if p.layout.strip.width() > 0 {
            let plus = p.layout.new_tab;
            if p.hover == Some(Hit::NewTab) {
                fill(mem, plus, p.palette.tab_hover);
            }
            SetTextColor(mem, p.palette.ink_muted.colorref());
            let label = if p.layout.vertical { "+  Nueva pestaña" } else { "+" };
            let mut text: Vec<u16> = label.encode_utf16().collect();
            let mut r = if p.layout.vertical { plus.inset(px(10), 0).win() } else { plus.win() };
            let align = if p.layout.vertical { DT_LEFT } else { DT_CENTER };
            DrawTextW(mem, &mut text, &mut r, align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

            let s = p.layout.strip;
            let line = if p.layout.vertical {
                Rect::new(s.right - 1, 0, s.right, s.bottom)
            } else {
                Rect::new(0, s.bottom - 1, s.right, s.bottom)
            };
            fill(mem, line, p.palette.line);
        }

        for &(rect, color) in p.fills {
            fill(mem, rect, color);
        }

        if p.layout.extensions.width() > 0 {
            let button = p.layout.extensions;
            if p.hover == Some(Hit::Extensions) {
                fill(mem, button, p.palette.tab_hover);
            }
            draw_puzzle(mem, button, p.palette.ink_muted, p.scale);
            SetTextColor(mem, p.palette.ink_muted.colorref());
            for (i, (mark, initial)) in p.layout.extension_marks.iter().zip(p.extension_initials).enumerate() {
                if p.hover == Some(Hit::PinnedExtension(i)) {
                    fill(mem, *mark, p.palette.tab_hover);
                }
                let mut text: Vec<u16> = initial.to_string().encode_utf16().collect();
                let mut r = mark.win();
                DrawTextW(mem, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
            }
        }

        if let Some(hint) = p.hint {
            SetTextColor(mem, p.palette.ink_asleep.colorref());
            let mut text: Vec<u16> = hint.encode_utf16().collect();
            let mut r = p.layout.content.win();
            DrawTextW(mem, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        }

        let _ = BitBlt(hdc, 0, 0, width, height, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_font);
        SelectObject(mem, old_bitmap);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
    }
}

/// Items are laid out pinned-first in both orientations, as the tab list
/// keeps them; this pairs each item with its tab by id.
fn order_like<'a>(layout: &Layout, tabs: &'a [TabInfo<'a>]) -> Vec<&'a TabInfo<'a>> {
    layout.items.iter().filter_map(|item| tabs.iter().find(|t| t.id == item.id)).collect()
}

/// A puzzle piece, the usual mark for extensions: a square with a knob on
/// top and one on the right, about 16 px, centred in `rect`.
fn draw_puzzle(hdc: HDC, rect: Rect, color: Rgb, scale: f32) {
    let px = |v: f32| (v * scale).round() as i32;
    let (cx, cy) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    unsafe {
        let brush = CreateSolidBrush(color.colorref());
        let old_brush = SelectObject(hdc, brush.into());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        FillRect(hdc, &Rect::new(cx - px(7.0), cy - px(3.0), cx + px(4.0), cy + px(8.0)).win(), brush);
        let _ = Ellipse(hdc, cx - px(5.0), cy - px(8.0), cx + px(2.0), cy - px(1.0));
        let _ = Ellipse(hdc, cx + px(2.0), cy - px(1.0), cx + px(9.0), cy + px(6.0));
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

fn fill(hdc: HDC, rect: Rect, color: Rgb) {
    unsafe {
        let brush = CreateSolidBrush(color.colorref());
        FillRect(hdc, &rect.win(), brush);
        let _ = DeleteObject(brush.into());
    }
}

