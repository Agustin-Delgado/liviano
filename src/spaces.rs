//! Spaces, without the engine: named sets of tabs ("Work", "Personal")
//! that the window goes between with one key. The rules live here, on
//! `TabList`, so they are tested without a window:
//!
//! - only the active space's tabs are in `TabList::tabs`; the other
//!   spaces' tabs wait in `TabList::parked`, each space's in its own order.
//!   Thus the strip, the drops, Ctrl+1…8 and the groups work on one set of
//!   tabs, as before;
//! - a switch only moves tabs between the two lists: it makes and closes no
//!   view. The app then shows one page and hides one (`views_ui`);
//! - there is always at least one space, and deleting one never loses a
//!   tab: its tabs go to the space before it;
//! - a group's tabs and a split pair's two tabs are always in one space.
//!
//! "Session" means the restore file (`session`): these are spaces.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::{
    session::Session,
    tab_groups::GroupColor,
    tabbar::Rect,
    tabs::{Tab, TabList},
};

/// The id of the space a new window starts with. Tab and group ids start
/// at 1 (`TabList::take_id`), so it names no tab.
pub const FIRST_SPACE: u64 = 0;
/// What the first space is called until it is renamed.
pub const FIRST_NAME: &str = "Personal";
/// Ctrl+Shift+1…9 reach each of them, and they fit in the switcher.
pub const MAX_SPACES: usize = 9;

#[derive(Clone, Debug, PartialEq)]
pub struct Space {
    pub id: u64,
    pub name: String,
    pub color: GroupColor,
    /// The tab that was live when the space was left: it is live again
    /// when the space comes back.
    pub last_active: Option<u64>,
}

impl Space {
    pub fn first() -> Self {
        Space { id: FIRST_SPACE, name: FIRST_NAME.to_string(), color: GroupColor::ALL[0], last_active: None }
    }
}

/// One space in session.json; its tabs point at it by position
/// (`SavedTab::space`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSpace {
    pub name: String,
    /// A `GroupColor` name, kept as text as for the groups.
    pub color: String,
    /// The position in `Session::tabs` of the tab that was live in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<usize>,
}

impl<W> TabList<W> {
    pub fn space_by_id(&self, id: u64) -> Option<&Space> {
        self.spaces.iter().find(|s| s.id == id)
    }

    fn space_mut(&mut self, id: u64) -> Option<&mut Space> {
        self.spaces.iter_mut().find(|s| s.id == id)
    }

    /// The space whose tabs are on the strip.
    pub fn active_space(&self) -> &Space {
        self.space_by_id(self.space).unwrap_or(&self.spaces[0])
    }

    pub fn space_index(&self, id: u64) -> Option<usize> {
        self.spaces.iter().position(|s| s.id == id)
    }

    pub fn space_of(&self, tab: u64) -> Option<u64> {
        self.get(tab).map(|t| t.space)
    }

    /// The ids of a space's tabs, in its strip order.
    pub fn space_tabs(&self, space: u64) -> Vec<u64> {
        let tabs = if space == self.space { &self.tabs } else { &self.parked };
        tabs.iter().filter(|t| t.space == space).map(|t| t.id).collect()
    }

    /// Every tab, space by space in switcher order, each space's in strip
    /// order: the order session.json writes them in.
    pub fn in_space_order(&self) -> Vec<&Tab<W>> {
        let mut out = Vec::with_capacity(self.count());
        for space in &self.spaces {
            if space.id == self.space {
                out.extend(self.tabs.iter());
            } else {
                out.extend(self.parked.iter().filter(|t| t.space == space.id));
            }
        }
        out
    }

    /// "Space 2", "Space 3"…: the first number no space is called by.
    pub fn next_space_name(&self) -> String {
        (self.spaces.len() + 1..)
            .map(|n| format!("Space {n}"))
            .find(|name| !self.spaces.iter().any(|s| &s.name == name))
            .unwrap_or_default()
    }

    /// The first colour no space wears, or the next in turn.
    fn next_space_color(&self) -> GroupColor {
        GroupColor::ALL
            .into_iter()
            .find(|c| !self.spaces.iter().any(|s| s.color == *c))
            .unwrap_or(GroupColor::ALL[self.spaces.len() % GroupColor::ALL.len()])
    }

    /// A new, empty space at the end of the switcher. None when there are
    /// `MAX_SPACES` already.
    pub fn add_space(&mut self) -> Option<u64> {
        if self.spaces.len() >= MAX_SPACES {
            return None;
        }
        let id = self.take_id();
        let space = Space { id, name: self.next_space_name(), color: self.next_space_color(), last_active: None };
        self.spaces.push(space);
        Some(id)
    }

    /// Makes `id` the active space: the strip's tabs go to `parked`, the
    /// space's come out of it, and the tab that was live in it is live
    /// again (or its first tab on show; none in an empty space). False when
    /// nothing changed.
    pub fn switch_space(&mut self, id: u64) -> bool {
        if id == self.space || self.space_by_id(id).is_none() {
            return false;
        }
        let leaving = self.active.take();
        if let Some(tab) = leaving.and_then(|a| self.tabs.iter_mut().find(|t| t.id == a)) {
            // Its idle time (for sleep) starts now.
            tab.last_active = Instant::now();
        }
        let from = self.space;
        if let Some(space) = self.space_mut(from) {
            space.last_active = leaving;
        }
        let (incoming, rest): (Vec<Tab<W>>, Vec<Tab<W>>) =
            std::mem::take(&mut self.parked).into_iter().partition(|t| t.space == id);
        let outgoing = std::mem::replace(&mut self.tabs, incoming);
        self.parked = rest;
        self.parked.extend(outgoing);
        self.space = id;
        let remembered = self.active_space().last_active.filter(|t| self.index_of(*t).is_some());
        let next = remembered
            .or_else(|| self.tabs.iter().find(|t| !self.tab_hidden(t)).map(|t| t.id))
            .or_else(|| self.tabs.first().map(|t| t.id));
        if let Some(next) = next {
            self.select(next);
        }
        true
    }

    /// The space `offset` places away from the active one, wrapping around.
    pub fn space_neighbour(&self, offset: isize) -> Option<u64> {
        let current = self.space_index(self.space)? as isize;
        let index = (current + offset).rem_euclid(self.spaces.len() as isize);
        self.spaces.get(index as usize).map(|s| s.id).filter(|id| *id != self.space)
    }

    /// Ctrl+Shift+1…9: the space at that position.
    pub fn space_by_number(&self, n: u8) -> Option<u64> {
        self.spaces.get(usize::from(n).checked_sub(1)?).map(|s| s.id)
    }

    /// An empty name keeps the old one: a space is always called something.
    pub fn rename_space(&mut self, id: u64, name: &str) -> bool {
        let name = name.trim();
        match self.space_mut(id) {
            Some(space) if !name.is_empty() && space.name != name => {
                space.name = name.to_string();
                true
            }
            _ => false,
        }
    }

    pub fn set_space_color(&mut self, id: u64, color: GroupColor) {
        if let Some(space) = self.space_mut(id) {
            space.color = color;
        }
    }

    /// The space goes; its tabs join the space before it (after it, for the
    /// first one), at the end, their groups kept. Leaving the active space
    /// switches to that one first. Returns the space that took the tabs;
    /// None for the last space, which cannot go.
    pub fn delete_space(&mut self, id: u64) -> Option<u64> {
        let index = self.space_index(id)?;
        if self.spaces.len() < 2 {
            return None;
        }
        let heir = self.spaces[if index > 0 { index - 1 } else { 1 }].id;
        if id == self.space {
            self.switch_space(heir);
        }
        let parked = std::mem::take(&mut self.parked);
        let (mut moving, rest): (Vec<Tab<W>>, Vec<Tab<W>>) = parked.into_iter().partition(|t| t.space == id);
        self.parked = rest;
        for tab in &mut moving {
            tab.space = heir;
        }
        // The heir is the active space here (it took the switch above) or
        // waits in `parked`.
        for tab in moving {
            self.put_in(tab);
        }
        self.spaces.retain(|s| s.id != id);
        self.normalize_groups();
        Some(heir)
    }

    /// Puts a tab at the end of its space: among the pinned tabs when it
    /// is pinned.
    fn put_in(&mut self, tab: Tab<W>) {
        if tab.space != self.space {
            self.parked.push(tab);
        } else if tab.pinned {
            let at = self.pinned_count();
            self.tabs.insert(at, tab);
        } else {
            self.tabs.push(tab);
        }
    }

    /// Moves one tab to the end of another space. It leaves its group (a
    /// group stays in one space). When it was the live tab, its neighbour
    /// on show takes over. False when nothing changed.
    pub fn move_to_space(&mut self, tab: u64, space: u64) -> bool {
        if self.space_by_id(space).is_none() || self.space_of(tab).is_none_or(|s| s == space) {
            return false;
        }
        let mut moved = match self.index_of(tab) {
            Some(index) => {
                let moved = self.tabs.remove(index);
                if self.active == Some(tab) {
                    let at = index.min(self.tabs.len());
                    let shown = |t: &&Tab<W>| !self.tab_hidden(t);
                    let next = self.tabs[at..]
                        .iter()
                        .find(shown)
                        .or_else(|| self.tabs[..at].iter().rev().find(shown))
                        .map(|t| t.id);
                    self.active = None;
                    if let Some(next) = next {
                        self.select(next);
                    }
                }
                moved
            }
            None => {
                let Some(at) = self.parked.iter().position(|t| t.id == tab) else { return false };
                self.parked.remove(at)
            }
        };
        moved.space = space;
        moved.group = None;
        self.put_in(moved);
        self.normalize_groups();
        true
    }

    /// The tabs of a session, asleep, in their spaces and groups, with the
    /// saved live tab selected; also the restored ids in saved order (the
    /// split pairs point at them by position). A session written before
    /// spaces existed has none: all of it goes into the first space.
    pub fn from_session(session: &Session) -> (Self, Vec<u64>) {
        let mut list = TabList::default();
        let restored: Vec<(u64, Option<usize>)> = session
            .tabs
            .iter()
            .map(|saved| (list.insert(&saved.url, &saved.title, saved.pinned, None), saved.group))
            .collect();
        list.restore_groups(&session.groups, &restored);
        let ids: Vec<u64> = restored.iter().map(|(id, _)| *id).collect();
        let places: Vec<usize> = session.tabs.iter().map(|t| t.space).collect();
        list.restore_spaces(&session.spaces, session.active_space, &ids, &places);
        // Selected after the groups are back: a live tab in a folded group unfolds it.
        let saved_live = ids.get(session.active.min(ids.len().saturating_sub(1))).copied();
        let live = saved_live
            .filter(|id| list.index_of(*id).is_some())
            .or(list.active_space().last_active.filter(|id| list.index_of(*id).is_some()))
            .or_else(|| list.tabs.first().map(|t| t.id));
        if let Some(id) = live {
            list.select(id);
        }
        (list, ids)
    }

    /// Puts each restored tab (`ids`, with the saved space position of each
    /// in `places`) in its space. Positions that point nowhere go to the
    /// first space, and a group that a hand-edited file spread over two
    /// spaces stays in the space of its first tab.
    fn restore_spaces(&mut self, saved: &[SavedSpace], active: usize, ids: &[u64], places: &[usize]) {
        if saved.is_empty() {
            return;
        }
        self.spaces = saved
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let name = s.name.trim();
                let name = match (name.is_empty(), i) {
                    (false, _) => name.to_string(),
                    (true, 0) => FIRST_NAME.to_string(),
                    (true, n) => format!("Space {}", n + 1),
                };
                Space {
                    id: self.take_id(),
                    name,
                    color: GroupColor::from_name(&s.color).unwrap_or(GroupColor::ALL[0]),
                    last_active: s.active.and_then(|p| ids.get(p).copied()),
                }
            })
            .collect();
        self.spaces.truncate(MAX_SPACES);
        self.space = self.spaces[active.min(self.spaces.len() - 1)].id;
        let space_ids: Vec<u64> = self.spaces.iter().map(|s| s.id).collect();
        for (&id, &place) in ids.iter().zip(places) {
            let space = space_ids.get(place).copied().unwrap_or(space_ids[0]);
            if let Some(tab) = self.get_mut(id) {
                tab.space = space;
            }
        }
        let mut home: Vec<(u64, u64)> = Vec::new();
        for tab in &mut self.tabs {
            let Some(group) = tab.group else { continue };
            match home.iter().find(|(g, _)| *g == group) {
                Some((_, space)) if *space != tab.space => tab.group = None,
                Some(_) => {}
                None => home.push((group, tab.space)),
            }
        }
        let active = self.space;
        let (tabs, parked): (Vec<Tab<W>>, Vec<Tab<W>>) =
            std::mem::take(&mut self.tabs).into_iter().partition(|t| t.space == active);
        self.tabs = tabs;
        self.parked = parked;
        self.normalize_groups();
    }
}

/// What the switcher needs to know about one space.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceInfo<'a> {
    pub id: u64,
    pub name: &'a str,
    pub color: GroupColor,
    pub active: bool,
    /// The width of its name in the switcher's type, in pixels.
    pub name_w: i32,
}

/// One space in the switcher: where it is and whether it shows its name
/// (else only its colour dot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceChip {
    pub id: u64,
    pub rect: Rect,
    pub named: bool,
}

/// A chip's height: the group label's, a little under a tab's.
pub const CHIP_H: i32 = 24;
/// A chip that shows only its dot is a square.
const DOT_CHIP_W: i32 = 24;
pub const CHIP_PAD: i32 = 8;
pub const CHIP_DOT: f32 = 6.0;
pub const DOT_GAP: i32 = 6;
/// Past this, a long name is cut.
const NAME_MAX_W: i32 = 96;
const CHIP_GAP: i32 = 2;

fn px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

fn chip_w(space: &SpaceInfo, named: bool, scale: f32) -> i32 {
    if named {
        px(CHIP_PAD, scale)
            + (CHIP_DOT * scale).round() as i32
            + px(DOT_GAP, scale)
            + space.name_w.clamp(0, px(NAME_MAX_W, scale))
            + px(CHIP_PAD, scale)
    } else {
        px(DOT_CHIP_W, scale)
    }
}

/// The narrowest a switcher of `count` spaces gets: only the dots.
pub fn dots_width(count: usize, scale: f32) -> i32 {
    let n = count as i32;
    n * px(DOT_CHIP_W, scale) + (n - 1).max(0) * px(CHIP_GAP, scale)
}

/// The switcher's chips in a row from `left`, centred between `top` and
/// `bottom`. All names show when they fit in `room`; else only the active
/// space's; else only the dots (also when even those do not fit).
pub fn chip_row(spaces: &[SpaceInfo], left: i32, room: i32, top: i32, bottom: i32, scale: f32) -> Vec<SpaceChip> {
    let gap = px(CHIP_GAP, scale);
    let width = |named: &dyn Fn(&SpaceInfo) -> bool| -> i32 {
        spaces.iter().map(|s| chip_w(s, named(s), scale) + gap).sum::<i32>() - gap
    };
    let all = |_: &SpaceInfo| true;
    let active = |s: &SpaceInfo| s.active;
    let none = |_: &SpaceInfo| false;
    let named: &dyn Fn(&SpaceInfo) -> bool = if width(&all) <= room {
        &all
    } else if width(&active) <= room {
        &active
    } else {
        &none
    };
    let h = px(CHIP_H, scale);
    let chip_top = top + (bottom - top - h) / 2;
    let mut x = left;
    spaces
        .iter()
        .map(|s| {
            let w = chip_w(s, named(s), scale);
            let chip = SpaceChip { id: s.id, rect: Rect::new(x, chip_top, x + w, chip_top + h), named: named(s) };
            x += w + gap;
            chip
        })
        .collect()
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/spaces.rs"]
#[rustfmt::skip]
mod tests;
