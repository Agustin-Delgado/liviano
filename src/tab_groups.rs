//! Tab groups, without the engine: a named, coloured run of tabs in the
//! strip that can be folded away to its label. The rules live here, on
//! `TabList`, so they are tested without a window:
//!
//! - the tabs of a group are always next to each other (`normalize_groups`
//!   restores it after anything that moves tabs);
//! - pinned tabs never belong to one;
//! - a group without tabs is gone;
//! - the live tab is never hidden: folding the group that holds it moves
//!   the selection out, and selecting a tab of a folded group unfolds it.
//!
//! Not to be confused with split view's pairs, which the strip also draws
//! on one ground: those live in `split`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::tabs::{Tab, TabList};

/// The closed palette a group's colour comes from (celagem's tones; the
/// values for each theme are in `design::group_tone`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GroupColor {
    Grey,
    Blue,
    Red,
    Yellow,
    Green,
    Pink,
    Purple,
    Cyan,
    Orange,
}

impl GroupColor {
    /// In the order a new group takes them and the menu lists them.
    pub const ALL: [GroupColor; 9] = [
        GroupColor::Blue,
        GroupColor::Red,
        GroupColor::Yellow,
        GroupColor::Green,
        GroupColor::Pink,
        GroupColor::Purple,
        GroupColor::Cyan,
        GroupColor::Orange,
        GroupColor::Grey,
    ];

    /// Stable name for session.json.
    pub fn name(self) -> &'static str {
        match self {
            GroupColor::Grey => "grey",
            GroupColor::Blue => "blue",
            GroupColor::Red => "red",
            GroupColor::Yellow => "yellow",
            GroupColor::Green => "green",
            GroupColor::Pink => "pink",
            GroupColor::Purple => "purple",
            GroupColor::Cyan => "cyan",
            GroupColor::Orange => "orange",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// What the menu calls it.
    pub fn label(self) -> &'static str {
        match self {
            GroupColor::Grey => "Gris",
            GroupColor::Blue => "Azul",
            GroupColor::Red => "Rojo",
            GroupColor::Yellow => "Amarillo",
            GroupColor::Green => "Verde",
            GroupColor::Pink => "Rosa",
            GroupColor::Purple => "Violeta",
            GroupColor::Cyan => "Cian",
            GroupColor::Orange => "Naranja",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// From the same counter as the tabs' ids: an id names one tab or one
    /// group, never both (the strip's motion keeps both in one map).
    pub id: u64,
    /// Optional: an unnamed group shows only its colour.
    pub name: String,
    pub color: GroupColor,
    /// Folded to its label: its tabs are not drawn.
    pub collapsed: bool,
}

impl Group {
    /// What a menu or the field calls it.
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() {
            format!("Grupo sin nombre ({})", self.color.label().to_lowercase())
        } else {
            self.name.clone()
        }
    }
}

/// One group in session.json; its tabs point at it by position
/// (`SavedTab::group`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedGroup {
    pub name: String,
    /// A `GroupColor` name. Kept as text so a colour this version doesn't
    /// know reads as the first one instead of breaking the session.
    pub color: String,
    pub collapsed: bool,
}

/// Where a tab dragged along the strip lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// In the place of this tab (by index), taking its group: dragged over
    /// a tab of a group it joins it, over a loose tab it is loose.
    Tab(usize),
    /// Right before this group's label, outside it.
    BeforeGroup(u64),
    /// After every tab, outside any group.
    End,
}

impl<W> TabList<W> {
    pub fn group(&self, id: u64) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn group_mut(&mut self, id: u64) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    pub fn group_of(&self, tab: u64) -> Option<u64> {
        self.get(tab).and_then(|t| t.group)
    }

    /// The group's tabs, in strip order.
    pub fn members(&self, group: u64) -> Vec<u64> {
        self.tabs.iter().filter(|t| t.group == Some(group)).map(|t| t.id).collect()
    }

    /// The index range its tabs take (they are contiguous).
    fn span(&self, group: u64) -> Option<(usize, usize)> {
        let first = self.tabs.iter().position(|t| t.group == Some(group))?;
        let last = self.tabs.iter().rposition(|t| t.group == Some(group))?;
        Some((first, last))
    }

    /// Folded away: the tab is in a collapsed group.
    pub fn is_hidden(&self, tab: u64) -> bool {
        self.group_of(tab).and_then(|g| self.group(g)).is_some_and(|g| g.collapsed)
    }

    pub(crate) fn tab_hidden(&self, tab: &Tab<W>) -> bool {
        tab.group.and_then(|g| self.group(g)).is_some_and(|g| g.collapsed)
    }

    /// The first colour no group wears, or the next in turn when all are taken.
    pub fn next_color(&self) -> GroupColor {
        GroupColor::ALL
            .into_iter()
            .find(|c| !self.groups.iter().any(|g| g.color == *c))
            .unwrap_or(GroupColor::ALL[self.groups.len() % GroupColor::ALL.len()])
    }

    /// A new group with `tab` in it, where the tab is (a tab in another
    /// group leaves it, to the group's right). Pinned tabs can't be grouped.
    pub fn create_group(&mut self, tab: u64) -> Option<u64> {
        if self.get(tab)?.pinned {
            return None;
        }
        if self.group_of(tab).is_some() {
            self.remove_from_group(tab);
        }
        let id = self.take_id();
        let color = self.next_color();
        self.groups.push(Group { id, name: String::new(), color, collapsed: false });
        if let Some(t) = self.get_mut(tab) {
            t.group = Some(id);
        }
        Some(id)
    }

    /// Moves `tab` to the end of `group` and into it.
    pub fn add_to_group(&mut self, tab: u64, group: u64) -> bool {
        if self.group(group).is_none() || self.get(tab).is_none_or(|t| t.pinned) || self.group_of(tab) == Some(group) {
            return false;
        }
        let Some(from) = self.index_of(tab) else { return false };
        let mut moved = self.tabs.remove(from);
        moved.group = Some(group);
        let (_, last) = self.span(group).unwrap_or((self.tabs.len(), self.tabs.len()));
        self.tabs.insert((last + 1).min(self.tabs.len()), moved);
        self.normalize_groups();
        true
    }

    /// Takes `tab` out of its group, to the group's right.
    pub fn remove_from_group(&mut self, tab: u64) {
        let Some(group) = self.group_of(tab) else { return };
        let Some(from) = self.index_of(tab) else { return };
        let mut moved = self.tabs.remove(from);
        moved.group = None;
        let at = self.span(group).map_or(from, |(_, last)| last + 1);
        self.tabs.insert(at.min(self.tabs.len()), moved);
        self.normalize_groups();
    }

    /// The group goes; its tabs stay where they are.
    pub fn ungroup(&mut self, group: u64) {
        for tab in self.tabs.iter_mut().filter(|t| t.group == Some(group)) {
            tab.group = None;
        }
        self.groups.retain(|g| g.id != group);
    }

    pub fn rename_group(&mut self, group: u64, name: &str) {
        if let Some(g) = self.group_mut(group) {
            g.name = name.trim().to_string();
        }
    }

    pub fn set_group_color(&mut self, group: u64, color: GroupColor) {
        if let Some(g) = self.group_mut(group) {
            g.color = color;
        }
    }

    /// Folds or unfolds the group. Folding the group that holds the live
    /// tab hands the selection to the nearest tab still on show (right
    /// first, as Chrome does), or to none when nothing else shows. Returns
    /// whether anything changed.
    pub fn set_collapsed(&mut self, group: u64, collapsed: bool) -> bool {
        if self.group(group).is_none_or(|g| g.collapsed == collapsed) {
            return false;
        }
        if let Some(g) = self.group_mut(group) {
            g.collapsed = collapsed;
        }
        if collapsed && self.active.is_some_and(|a| self.group_of(a) == Some(group)) {
            let index = self.active_index().unwrap_or(0);
            let shown = |t: &&Tab<W>| !self.tab_hidden(t);
            let next = self.tabs[index..].iter().find(shown).or_else(|| self.tabs[..index].iter().rev().find(shown));
            match next.map(|t| t.id) {
                Some(id) => {
                    self.select(id);
                }
                None => self.active = None,
            }
        }
        true
    }

    /// Back to the invariants: pinned tabs and tabs pointing at no group are
    /// loose, each group's tabs gather where its first tab is (the others
    /// keep their order) and groups left without tabs are dropped. Returns
    /// whether anything changed.
    pub fn normalize_groups(&mut self) -> bool {
        let mut changed = false;
        let known: Vec<u64> = self.groups.iter().map(|g| g.id).collect();
        for tab in &mut self.tabs {
            if tab.group.is_some_and(|g| tab.pinned || !known.contains(&g)) {
                tab.group = None;
                changed = true;
            }
        }
        // Each tab's place: its group's first index, then its own.
        let mut first: HashMap<u64, usize> = HashMap::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            if let Some(g) = tab.group {
                first.entry(g).or_insert(i);
            }
        }
        let key = |i: usize, tab: &Tab<W>| (tab.group.map_or(i, |g| first[&g]), i);
        let mut keys: Vec<(usize, usize)> = self.tabs.iter().enumerate().map(|(i, t)| key(i, t)).collect();
        if keys.windows(2).any(|w| w[0] > w[1]) {
            let mut indexed: Vec<((usize, usize), Tab<W>)> = keys.drain(..).zip(self.tabs.drain(..)).collect();
            indexed.sort_by_key(|(k, _)| *k);
            self.tabs = indexed.into_iter().map(|(_, t)| t).collect();
            changed = true;
        }
        let before = self.groups.len();
        let tabs = &self.tabs;
        self.groups.retain(|g| tabs.iter().any(|t| t.group == Some(g.id)));
        changed || self.groups.len() != before
    }

    /// A tab dragged along the strip dropped at `at`.
    pub fn drop_tab(&mut self, tab: u64, at: Drop) {
        let Some(from) = self.index_of(tab) else { return };
        let pinned = self.tabs[from].pinned;
        let (to, group) = match at {
            Drop::Tab(i) => (i, self.tabs.get(i).and_then(|t| t.group)),
            Drop::BeforeGroup(g) => {
                let Some((first, _)) = self.span(g) else { return };
                (if from < first { first - 1 } else { first }, None)
            }
            Drop::End => (self.tabs.len().saturating_sub(1), None),
        };
        self.move_to(tab, to);
        if !pinned {
            if let Some(t) = self.get_mut(tab) {
                t.group = group;
            }
        }
        self.normalize_groups();
    }

    /// Moves the whole group, its order kept, before or after `anchor`
    /// (when the anchor is in a group, before or after all of that group).
    pub fn move_group(&mut self, group: u64, anchor: u64, after: bool) {
        if self.group_of(anchor) == Some(group) || self.group(group).is_none() {
            return;
        }
        let block: Vec<Tab<W>> = {
            let mut taken = Vec::new();
            let mut i = 0;
            while i < self.tabs.len() {
                if self.tabs[i].group == Some(group) { taken.push(self.tabs.remove(i)) } else { i += 1 }
            }
            taken
        };
        let at = match self.group_of(anchor).and_then(|g| self.span(g)) {
            Some((first, last)) => if after { last + 1 } else { first },
            None => self.index_of(anchor).map_or(self.tabs.len(), |i| if after { i + 1 } else { i }),
        };
        // Never among the pinned tabs.
        let at = at.max(self.tabs.iter().filter(|t| t.pinned).count()).min(self.tabs.len());
        for (k, tab) in block.into_iter().enumerate() {
            self.tabs.insert(at + k, tab);
        }
        self.normalize_groups();
    }

    /// The groups session.json keeps, in strip order: those with at least
    /// one tab `kept` saves. Their positions are what `SavedTab::group` holds.
    pub fn saved_group_ids(&self, kept: impl Fn(&Tab<W>) -> bool) -> Vec<u64> {
        let mut ids = Vec::new();
        for tab in self.tabs.iter().filter(|t| kept(t)) {
            if let Some(g) = tab.group.filter(|g| !ids.contains(g)) {
                ids.push(g);
            }
        }
        ids
    }

    pub fn saved_groups(&self, kept: impl Fn(&Tab<W>) -> bool) -> Vec<SavedGroup> {
        self.saved_group_ids(kept)
            .into_iter()
            .filter_map(|id| self.group(id))
            .map(|g| SavedGroup { name: g.name.clone(), color: g.color.name().to_string(), collapsed: g.collapsed })
            .collect()
    }

    /// Restores the groups of a session: `tabs` pairs each restored tab's
    /// id with the group position it was saved with. Positions that point
    /// nowhere are ignored, and the invariants are put back afterwards (a
    /// hand-edited file may scatter a group or group a pinned tab).
    pub fn restore_groups(&mut self, saved: &[SavedGroup], tabs: &[(u64, Option<usize>)]) {
        let ids: Vec<u64> = saved
            .iter()
            .map(|s| {
                let id = self.take_id();
                let color = GroupColor::from_name(&s.color).unwrap_or(GroupColor::ALL[0]);
                self.groups.push(Group { id, name: s.name.trim().to_string(), color, collapsed: s.collapsed });
                id
            })
            .collect();
        for &(tab, index) in tabs {
            let group = index.and_then(|i| ids.get(i).copied());
            if let Some(t) = self.get_mut(tab) {
                t.group = group;
            }
        }
        self.normalize_groups();
    }
}

