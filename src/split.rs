//! Split view, without the engine: which tabs form a pair, where each page
//! of a pair goes in the content area, how the strip keeps partners side by
//! side and how pairs are written to session.json. Pure, so all of it is
//! tested without a window.

use serde::{Deserialize, Serialize};

use crate::tabbar::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// The left page's share of the width a new pair starts with.
pub const DEFAULT_RATIO: f32 = 0.5;

/// Two tabs shown side by side. `ratio` is the left page's share of the
/// width (the divider excluded).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    pub left: u64,
    pub right: u64,
    pub ratio: f32,
}

impl Pair {
    pub fn contains(&self, id: u64) -> bool {
        self.left == id || self.right == id
    }

    pub fn partner(&self, id: u64) -> Option<u64> {
        match id {
            _ if id == self.left => Some(self.right),
            _ if id == self.right => Some(self.left),
            _ => None,
        }
    }

    pub fn side_of(&self, id: u64) -> Option<Side> {
        match id {
            _ if id == self.left => Some(Side::Left),
            _ if id == self.right => Some(Side::Right),
            _ => None,
        }
    }

    pub fn at(&self, side: Side) -> u64 {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// A ratio read from a file or a drag, kept where both pages stay visible.
fn sane_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() { ratio.clamp(0.05, 0.95) } else { DEFAULT_RATIO }
}

/// Every pair; a tab belongs to one pair at most.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Splits {
    pairs: Vec<Pair>,
}

impl Splits {
    pub fn pairs(&self) -> &[Pair] {
        &self.pairs
    }

    pub fn pair_of(&self, id: u64) -> Option<&Pair> {
        self.pairs.iter().find(|p| p.contains(id))
    }

    pub fn partner(&self, id: u64) -> Option<u64> {
        self.pair_of(id).and_then(|p| p.partner(id))
    }

    /// Pairs `placed` (on `side`) with `anchor` (on the other side). Whatever
    /// either was paired with becomes a single tab again; the anchor keeps the
    /// width it had, so replacing one page doesn't move the divider.
    pub fn join(&mut self, placed: u64, side: Side, anchor: u64) -> bool {
        if placed == anchor {
            return false;
        }
        let ratio = match self.pair_of(anchor) {
            Some(old) if old.side_of(anchor) == Some(side) => 1.0 - old.ratio,
            Some(old) => old.ratio,
            None => DEFAULT_RATIO,
        };
        self.pairs.retain(|p| !p.contains(placed) && !p.contains(anchor));
        let (left, right) = match side {
            Side::Left => (placed, anchor),
            Side::Right => (anchor, placed),
        };
        self.pairs.push(Pair { left, right, ratio });
        true
    }

    /// Undoes the pair of `id`; returns the tab it was paired with.
    pub fn split_off(&mut self, id: u64) -> Option<u64> {
        let index = self.pairs.iter().position(|p| p.contains(id))?;
        self.pairs.remove(index).partner(id)
    }

    pub fn swap_sides(&mut self, id: u64) {
        if let Some(pair) = self.pairs.iter_mut().find(|p| p.contains(id)) {
            std::mem::swap(&mut pair.left, &mut pair.right);
            pair.ratio = 1.0 - pair.ratio;
        }
    }

    pub fn set_ratio(&mut self, id: u64, ratio: f32) {
        if let Some(pair) = self.pairs.iter_mut().find(|p| p.contains(id)) {
            pair.ratio = sane_ratio(ratio);
        }
    }

    /// Drops the pairs that lost a tab.
    pub fn retain(&mut self, exists: impl Fn(u64) -> bool) {
        self.pairs.retain(|p| exists(p.left) && exists(p.right));
    }

    /// The pairs as session.json keeps them: positions in `saved`, the ids
    /// of the tabs written, in order. A pair with an unsaved tab is left out.
    pub fn to_saved(&self, saved: &[u64]) -> Vec<SavedPair> {
        let index = |id: u64| saved.iter().position(|s| *s == id);
        self.pairs
            .iter()
            .filter_map(|p| Some(SavedPair { left: index(p.left)?, right: index(p.right)?, ratio: p.ratio }))
            .collect()
    }

    /// Reads them back against the restored tabs' ids. Entries that point
    /// nowhere, pair a tab with itself or reuse a tab are skipped: a hand-edited
    /// or stale file never breaks the session.
    pub fn from_saved(saved: &[SavedPair], restored: &[u64]) -> Self {
        let mut splits = Splits::default();
        for entry in saved {
            let (Some(&left), Some(&right)) = (restored.get(entry.left), restored.get(entry.right)) else { continue };
            if left == right || splits.pair_of(left).is_some() || splits.pair_of(right).is_some() {
                continue;
            }
            splits.pairs.push(Pair { left, right, ratio: sane_ratio(entry.ratio) });
        }
        splits
    }
}

/// One pair in session.json: positions in its `tabs` list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPair {
    pub left: usize,
    pub right: usize,
    pub ratio: f32,
}

/// The strip order with every pair together, left then right, both with
/// the same pinned state. A pair goes where its `anchor` is (the tab being
/// dragged, the page that stays) or else where its first tab is, and takes
/// that tab's pinned state; pinned tabs still come first.
pub fn arrange(order: &[(u64, bool)], pairs: &[Pair], anchor: Option<u64>) -> Vec<(u64, bool)> {
    let pinned_of = |id: u64| order.iter().find(|(t, _)| *t == id).map(|(_, p)| *p);
    let mut out = Vec::with_capacity(order.len());
    for &(id, pinned) in order {
        let Some(pair) = pairs.iter().find(|p| p.contains(id)) else {
            out.push((id, pinned));
            continue;
        };
        let (Some(_), Some(_)) = (pinned_of(pair.left), pinned_of(pair.right)) else {
            // A pair with a tab not in the strip is not drawn as one.
            out.push((id, pinned));
            continue;
        };
        let lead = match anchor {
            Some(a) if pair.contains(a) => a,
            _ => {
                let first = order.iter().find(|(t, _)| pair.contains(*t)).map(|(t, _)| *t);
                first.unwrap_or(id)
            }
        };
        if id != lead {
            continue;
        }
        out.push((pair.left, pinned));
        out.push((pair.right, pinned));
    }
    out
}

/// Where the two pages and the divider go inside `content`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panes {
    pub left: Rect,
    pub divider: Rect,
    pub right: Rect,
}

impl Panes {
    pub fn of(&self, side: Side) -> Rect {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

/// The narrowest a page may get, lowered when the window can't give both
/// pages that much.
fn min_width(available: i32, min_side: i32) -> i32 {
    min_side.min(available / 2).max(0)
}

pub fn panes(content: Rect, ratio: f32, divider: i32, min_side: i32) -> Panes {
    let available = (content.width() - divider).max(0);
    let min = min_width(available, min_side);
    let left_width = ((available as f32 * sane_ratio(ratio)).round() as i32).clamp(min, available - min);
    let split = content.left + left_width;
    Panes {
        left: Rect::new(content.left, content.top, split, content.bottom),
        divider: Rect::new(split, content.top, split + divider, content.bottom),
        right: Rect::new(split + divider, content.top, content.right, content.bottom),
    }
}

/// The ratio that puts the divider's middle under `x`, within the minimum widths.
pub fn ratio_at(content: Rect, x: i32, divider: i32, min_side: i32) -> f32 {
    let available = (content.width() - divider).max(0);
    if available == 0 {
        return DEFAULT_RATIO;
    }
    let min = min_width(available, min_side);
    let left_width = (x - content.left - divider / 2).clamp(min, available - min);
    left_width as f32 / available as f32
}

/// The half of the content area a dragged tab dropped at `x` lands on.
pub fn side_at(content: Rect, x: i32) -> Side {
    if x < content.left + content.width() / 2 { Side::Left } else { Side::Right }
}

/// That half, as the drop hint shows it.
pub fn half(content: Rect, side: Side) -> Rect {
    let middle = content.left + content.width() / 2;
    match side {
        Side::Left => Rect::new(content.left, content.top, middle, content.bottom),
        Side::Right => Rect::new(middle, content.top, content.right, content.bottom),
    }
}
