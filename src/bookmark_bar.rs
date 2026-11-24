//! The bookmarks bar: the bookmarks as chips in a slim band under a top
//! strip (Safari's favorites bar), and as square tiles at the top of the
//! column (Arc's favorites). Pure geometry (tested), then the drawing.
//! The app keeps the list in memory (`app::bookmarks_ui`), so a paint
//! reads no file and the layout looks only at what fits.

use crate::{
    design::{Palette, size},
    frame,
    gfx::{Align, Color, Font, Painter, RectF},
    tabbar::{self, Hit, HoverKey, Layout, Motion, Rect},
};

/// The band under a top strip.
pub const BAR_H: i32 = 28;
/// A chip in the band: shorter than the band, so it has air above and under.
const CHIP_H: i32 = 24;
/// Air left of the icon and right of the title.
const CHIP_PAD: i32 = 8;
/// Between the icon and the title.
const ICON_GAP: i32 = 6;
/// A long title is cut short with an ellipsis.
const CHIP_MAX_W: i32 = 160;
const GAP: i32 = 2;
/// The "»" button at the end of the band.
const MORE_W: i32 = 28;
/// A tile in the column: the smallest width, and its height.
const TILE_MIN_W: i32 = 40;
const TILE_H: i32 = 36;
/// More rows push the tabs too far down: the rest go behind the "…" tile.
const MAX_ROWS: i32 = 3;
/// The column's pad, and the air between the tiles and the tabs (as under
/// the pinned tabs).
const COLUMN_PAD: i32 = 8;
const TILE_AIR: i32 = 6;
/// Chips measured at most: past this many, none fits on a screen anyway.
pub const MEASURED: usize = 64;

/// One bookmark as the bar shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    /// The title, or the site when the title is empty.
    pub label: String,
    /// The site's initial, for a site without an icon.
    pub initial: char,
}

impl Entry {
    pub fn new(url: &str, title: &str) -> Self {
        let host = crate::omnibox::display_host(url);
        let label = if !title.trim().is_empty() {
            title.trim().to_string()
        } else if !host.is_empty() {
            host.to_string()
        } else {
            url.to_string()
        };
        let initial = host.chars().next().or_else(|| label.chars().next()).unwrap_or('·').to_ascii_uppercase();
        Self { url: url.to_string(), title: title.to_string(), label, initial }
    }

    /// The tooltip: the title, then the site.
    pub fn tip(&self) -> String {
        let host = crate::omnibox::display_host(&self.url);
        if host.is_empty() || host == self.label { self.label.clone() } else { format!("{} — {host}", self.label) }
    }
}

/// Where the bar and each shown bookmark are. Empty when the bar does not show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarLayout {
    /// The band (top strip) or the block of tiles (column).
    pub area: Rect,
    /// Tiles in the column, chips under a top strip.
    pub tiles: bool,
    /// `items[i]` is the place of bookmark `i`; the rest are behind `more`.
    pub items: Vec<Rect>,
    /// The overflow button, when not all bookmarks fit.
    pub more: Option<Rect>,
}

impl BarLayout {
    pub fn shown(&self) -> bool {
        self.area.height() > 0
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.area.contains(x, y) {
            return None;
        }
        if let Some(i) = self.items.iter().position(|r| r.contains(x, y)) {
            return Some(Hit::Bookmark(i));
        }
        self.more.filter(|r| r.contains(x, y)).map(|_| Hit::BookmarksMore)
    }

    /// The place a dragged bookmark takes when the pointer is at (x, y):
    /// the bookmark there. A chip answers along the whole band height, and
    /// past the last chip the drag goes to the end.
    pub fn drop_index(&self, x: i32, y: i32) -> Option<usize> {
        let last = self.items.len().checked_sub(1)?;
        if self.tiles {
            return self.items.iter().position(|r| r.contains(x, y));
        }
        if y < self.area.top || y >= self.area.bottom {
            return None;
        }
        Some(self.items.iter().position(|r| x < r.right).unwrap_or(last))
    }
}

struct M(f32);

impl M {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.0).round() as i32
    }
}

/// A chip's width for a title `text_w` pixels wide in `chip_font`.
pub fn chip_width(text_w: f32, scale: f32) -> i32 {
    let m = M(scale);
    let text = text_w.max(0.0).ceil() as i32;
    (2 * m.px(CHIP_PAD) + m.px(size::MARK as i32) + m.px(ICON_GAP) + text).min(m.px(CHIP_MAX_W))
}

/// The chips' title: the strip's small step, regular, as a tab title.
pub fn chip_font(scale: f32) -> Font {
    Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Leading)
}

/// The chips from `left` to `right`, `count` bookmarks of which the first
/// `widths.len()` are measured. All of them when they fit; otherwise as
/// many as fit before the "»" button, which takes the right end.
pub fn lay_chips(
    widths: &[i32],
    count: usize,
    left: i32,
    top: i32,
    right: i32,
    scale: f32,
) -> (Vec<Rect>, Option<Rect>) {
    let m = M(scale);
    let (gap, h) = (m.px(GAP), m.px(CHIP_H));
    let all = widths.len() >= count && widths.iter().map(|w| w + gap).sum::<i32>() - gap <= right - left;
    let end = if all { right } else { right - m.px(MORE_W) - gap };
    let mut rects = Vec::new();
    let mut x = left;
    for &w in widths.iter().take(count) {
        if x + w > end {
            break;
        }
        rects.push(Rect::new(x, top, x + w, top + h));
        x += w + gap;
    }
    let more = (rects.len() < count).then(|| Rect::new(right - m.px(MORE_W), top, right, top + h));
    (rects, more)
}

/// The tiles of `count` bookmarks in a grid from `left` to `right`, from
/// `top` down: as many to a row as fit, stretched to fill the row, at most
/// `MAX_ROWS` rows; the last cell is the "…" tile when not all fit. Also
/// returns the bottom of the grid.
pub fn lay_tiles(count: usize, left: i32, top: i32, right: i32, scale: f32) -> (Vec<Rect>, Option<Rect>, i32) {
    let m = M(scale);
    let (gap, h) = (m.px(GAP), m.px(TILE_H));
    let width = (right - left).max(1);
    let per_row = ((width + gap) / (m.px(TILE_MIN_W) + gap)).max(1) as usize;
    let capacity = per_row * MAX_ROWS as usize;
    let shown = if count > capacity { capacity - 1 } else { count };
    let cells = shown + usize::from(shown < count);
    // The row's width without its gaps, shared out so the grid ends on
    // `right` exactly.
    let room = width - (per_row as i32 - 1) * gap;
    let cell = |i: usize| {
        let (row, col) = ((i / per_row) as i32, (i % per_row) as i32);
        let n = per_row as i32;
        let x0 = left + col * gap + room * col / n;
        let x1 = left + col * gap + room * (col + 1) / n;
        let y0 = top + row * (h + gap);
        Rect::new(x0, y0, x1, y0 + h)
    };
    let rects: Vec<Rect> = (0..shown).map(cell).collect();
    let more = (shown < count).then(|| cell(shown));
    let rows = cells.div_ceil(per_row) as i32;
    let bottom = if rows == 0 { top } else { top + rows * (h + gap) - gap };
    (rects, more, bottom)
}

/// Puts the bar into the strip's layout: a band under a top strip (the
/// page moves down by its height), tiles under the caption row of the
/// column (the tabs move down). `widths`: the measured chips (`chip_width`),
/// only used on a top strip. Nothing changes for no bookmarks, or with no
/// strip (tabs hidden, full screen).
pub fn place(layout: &mut Layout, count: usize, widths: &[i32], scale: f32) {
    unplace(layout);
    if count == 0 || layout.strip.width() == 0 {
        return;
    }
    let m = M(scale);
    if layout.vertical {
        let top = layout.frame.caption.bottom;
        let pad = m.px(COLUMN_PAD);
        let (items, more, bottom) = lay_tiles(count, pad, top, layout.strip.right - pad, scale);
        let dy = bottom - top + m.px(TILE_AIR);
        shift_column(layout, dy);
        layout.bookmarks =
            BarLayout { area: Rect::new(0, top, layout.strip.right, top + dy), tiles: true, items, more };
    } else {
        let top = layout.strip.bottom;
        let h = m.px(BAR_H);
        let edge = m.px(frame::LEAD);
        let chip_top = top + (h - m.px(CHIP_H)) / 2;
        let (items, more) = lay_chips(widths, count, edge, chip_top, layout.strip.right - edge, scale);
        layout.bookmarks =
            BarLayout { area: Rect::new(0, top, layout.strip.right, top + h), tiles: false, items, more };
        // The band belongs to the strip: its line moves under the band, and
        // a hover in the band repaints with the strip.
        layout.strip.bottom += h;
        layout.content.top = (layout.content.top + h).min(layout.content.bottom);
    }
}

/// Takes out a bar placed before, so a second `place` does not stack.
fn unplace(layout: &mut Layout) {
    let bar = std::mem::take(&mut layout.bookmarks);
    let h = bar.area.height();
    if h == 0 {
        return;
    }
    if bar.tiles {
        shift_column(layout, -h);
    } else {
        layout.strip.bottom -= h;
        layout.content.top = layout.strip.bottom.min(layout.content.bottom);
    }
}

/// The tabs, the group headers and the + button of the column move down by `dy`.
fn shift_column(layout: &mut Layout, dy: i32) {
    let down = |r: &mut Rect| {
        r.top += dy;
        r.bottom += dy;
    };
    for item in &mut layout.items {
        down(&mut item.rect);
        if let Some(c) = &mut item.close {
            down(c);
        }
    }
    layout.labels.iter_mut().for_each(|l| down(&mut l.rect));
    down(&mut layout.new_tab);
}

pub struct BarPaint<'a> {
    pub layout: &'a Layout,
    pub entries: &'a [Entry],
    pub palette: &'a Palette,
    pub motion: &'a Motion,
    pub scale: f32,
    /// The bookmark being dragged, drawn as held.
    pub held: Option<usize>,
}

/// Draws the bar over what the strip drew.
pub fn paint(g: &Painter, p: &BarPaint) {
    let bar = &p.layout.bookmarks;
    if !bar.shown() || !g.visible(bar.area.into()) {
        return;
    }
    // In the column the tiles slide in with it.
    let dx = if bar.tiles { -(1.0 - p.motion.rail()) * p.layout.strip.width() as f32 } else { 0.0 };
    g.offset(dx, 0.0, |g| {
        for (i, (rect, entry)) in bar.items.iter().zip(p.entries).enumerate() {
            let rect = RectF::from(*rect);
            if g.visible(rect) {
                paint_item(g, p, rect, entry, i);
            }
        }
        if let Some(more) = bar.more {
            paint_more(g, p, more.into());
        }
    });
}

fn paint_item(g: &Painter, p: &BarPaint, rect: RectF, entry: &Entry, i: usize) {
    let pal = p.palette;
    let px = |v: f32| v * p.scale;
    let over = p.motion.hover(HoverKey::Bookmark(i)).max(if p.held == Some(i) { 1.0 } else { 0.0 });
    let pressed = p.motion.pressed(HoverKey::Bookmark(i));
    let mark = px(size::MARK);
    let (cx, cy) = rect.center();
    if p.layout.bookmarks.tiles {
        // A tile rests on the pinned tabs' soft ground.
        let ground = pal.tab_tile.mix(pal.tab_hover, over).mix(pal.pin_live, pressed);
        g.round(rect, px(size::RADIUS), ground);
        let icon = RectF::new(cx - mark / 2.0, cy - mark / 2.0, cx + mark / 2.0, cy + mark / 2.0);
        paint_icon(g, p, icon, entry);
    } else {
        // A chip is a ghost button: no ground at rest.
        if over > 0.0 || pressed > 0.0 {
            let fill = pal.tab_hover.mix(pal.pin_live, pressed);
            g.round(rect, px(size::RADIUS_CONTROL), Color::from(fill).with_alpha(over.max(pressed)));
        }
        let left = rect.left + px(CHIP_PAD as f32);
        let icon = RectF::new(left, cy - mark / 2.0, left + mark, cy + mark / 2.0);
        paint_icon(g, p, icon, entry);
        let text =
            RectF::new(icon.right + px(ICON_GAP as f32), rect.top, rect.right - px(CHIP_PAD as f32), rect.bottom);
        g.text(&entry.label, text, chip_font(p.scale), pal.ink_muted.mix(pal.ink, over));
    }
}

fn paint_icon(g: &Painter, p: &BarPaint, rect: RectF, entry: &Entry) {
    match crate::favicon::for_url(&entry.url) {
        Some(icon) => {
            let px = &icon.pixels;
            let (l, t) = (rect.left.round(), rect.top.round());
            let snapped = RectF::new(l, t, l + rect.width().round(), t + rect.height().round());
            g.image(&px.data, px.width, px.height, snapped, 1.0);
        }
        None => tabbar::draw_mark(g, rect, entry.initial, false, false, p.palette, p.scale),
    }
}

fn paint_more(g: &Painter, p: &BarPaint, rect: RectF) {
    let pal = p.palette;
    let over = p.motion.hover(HoverKey::BookmarksMore);
    let pressed = p.motion.pressed(HoverKey::BookmarksMore);
    let tiles = p.layout.bookmarks.tiles;
    if tiles {
        g.round(rect, size::RADIUS * p.scale, pal.tab_tile.mix(pal.tab_hover, over).mix(pal.pin_live, pressed));
    } else if over > 0.0 || pressed > 0.0 {
        let fill = Color::from(pal.tab_hover.mix(pal.pin_live, pressed)).with_alpha(over.max(pressed));
        g.round(rect, size::RADIUS_CONTROL * p.scale, fill);
    }
    let font = Font::new(size::TEXT * p.scale, size::MEDIUM, Align::Center);
    g.text(if tiles { "…" } else { "»" }, rect, font, pal.ink_muted.mix(pal.ink, over));
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/bookmark_bar.rs"]
#[rustfmt::skip]
mod tests;
