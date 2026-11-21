//! The tab model, independent of the engine: `W` is whatever holds a live web
//! view, so the ordering and selection rules are tested without one.

use std::time::Instant;

#[derive(Debug)]
pub enum Live<W> {
    /// No web view: only metadata. Costs almost nothing.
    Asleep,
    /// A web view is being created.
    Creating,
    Ready(W),
}

#[derive(Debug)]
pub struct Tab<W> {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub pinned: bool,
    /// An InPrivate view: never saved, never reopened, never pinned.
    pub private: bool,
    pub loading: bool,
    pub live: Live<W>,
    pub last_active: Instant,
    /// Opened by a script (RF-23): never recorded in history, session or
    /// the reopen list.
    pub test: bool,
    /// The tab group it belongs to (`tab_groups`).
    pub group: Option<u64>,
}

impl<W> Tab<W> {
    pub fn label(&self) -> &str {
        if self.title.trim().is_empty() {
            let host = crate::omnibox::display_host(&self.url);
            if host.is_empty() { "Nueva pestaña" } else { host }
        } else {
            &self.title
        }
    }

    pub fn initial(&self) -> char {
        crate::omnibox::display_host(&self.url)
            .chars()
            .next()
            .or_else(|| self.title.chars().next())
            .unwrap_or('·')
            .to_ascii_uppercase()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Closed {
    pub url: String,
    pub title: String,
    pub index: usize,
}

pub struct TabList<W> {
    pub tabs: Vec<Tab<W>>,
    pub active: Option<u64>,
    /// Tab groups, in no particular order (the strip order is the tabs').
    pub groups: Vec<crate::tab_groups::Group>,
    next_id: u64,
    closed: Vec<Closed>,
}

impl<W> Default for TabList<W> {
    fn default() -> Self {
        Self { tabs: Vec::new(), active: None, groups: Vec::new(), next_id: 1, closed: Vec::new() }
    }
}

impl<W> TabList<W> {
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn get(&self, id: u64) -> Option<&Tab<W>> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Tab<W>> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active_tab(&self) -> Option<&Tab<W>> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.index_of(id))
    }

    /// A fresh id, for a tab or a group: they share the counter.
    pub(crate) fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn pinned_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.pinned).count()
    }

    /// Inserts an asleep tab. `at` is clamped so unpinned tabs never sit
    /// among the pinned ones. Placed inside a group, or right after the
    /// live tab when that one is in a group (opened from it), it joins it.
    pub fn insert(&mut self, url: &str, title: &str, pinned: bool, at: Option<usize>) -> u64 {
        let id = self.take_id();
        let pinned_count = self.pinned_count();
        let index = match (pinned, at) {
            (true, Some(i)) => i.min(pinned_count),
            (true, None) => pinned_count,
            (false, Some(i)) => i.clamp(pinned_count, self.tabs.len()),
            (false, None) => self.tabs.len(),
        };
        self.tabs.insert(
            index,
            Tab {
                id,
                url: url.to_string(),
                title: title.to_string(),
                pinned,
                private: false,
                loading: false,
                live: Live::Asleep,
                last_active: Instant::now(),
                test: false,
                group: None,
            },
        );
        if !pinned && at.is_some() {
            let group_at = |i: usize| self.tabs.get(i).and_then(|t| t.group);
            let before = index.checked_sub(1);
            let left = before.and_then(group_at);
            let from_live = before.and_then(|i| self.tabs.get(i)).is_some_and(|t| Some(t.id) == self.active);
            if left.is_some() && (left == group_at(index + 1) || from_live) {
                self.tabs[index].group = left;
            }
        }
        id
    }

    /// Inserts a private tab, placed like any unpinned one.
    pub fn insert_private(&mut self, url: &str, at: Option<usize>) -> u64 {
        let id = self.insert(url, "", false, at);
        if let Some(tab) = self.get_mut(id) {
            tab.private = true;
        }
        id
    }

    pub fn has_private(&self) -> bool {
        self.tabs.iter().any(|t| t.private)
    }

    /// Index right after the active tab, where a tab opened from it goes.
    pub fn after_active(&self) -> Option<usize> {
        self.active_index().map(|i| i + 1)
    }

    /// Makes `id` the live tab; a tab of a folded group unfolds it.
    pub fn select(&mut self, id: u64) -> bool {
        let Some(tab) = self.get(id) else { return false };
        if let Some(group) = tab.group {
            if let Some(g) = self.groups.iter_mut().find(|g| g.id == group) {
                g.collapsed = false;
            }
        }
        if let Some(previous) = self.active.and_then(|a| self.get_mut(a)) {
            previous.last_active = Instant::now();
        }
        self.active = Some(id);
        if let Some(tab) = self.get_mut(id) {
            tab.last_active = Instant::now();
        }
        true
    }

    /// Removes the tab and returns it; if it was active, the neighbour to the
    /// right (or left) becomes active.
    pub fn close(&mut self, id: u64) -> Option<Tab<W>> {
        let index = self.index_of(id)?;
        let tab = self.tabs.remove(index);
        // A private or test tab leaves no trace, not even in Ctrl+Shift+T.
        if !tab.url.is_empty() && !tab.private && !tab.test {
            self.closed.push(Closed { url: tab.url.clone(), title: tab.title.clone(), index });
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        if self.active == Some(id) {
            // The neighbour on show: a folded group's tabs are passed over.
            let at = index.min(self.tabs.len());
            let shown = |t: &&Tab<W>| !self.tab_hidden(t);
            let next = self.tabs[at..]
                .iter()
                .find(shown)
                .or_else(|| self.tabs[..at].iter().rev().find(shown))
                .or_else(|| self.tabs.get(index).or_else(|| self.tabs.last()));
            self.active = next.map(|t| t.id);
            if let Some(next) = self.active {
                self.select(next);
            }
        }
        if tab.group.is_some() {
            self.normalize_groups();
        }
        Some(tab)
    }

    /// Makes a tab a test tab (see `Tab::test`).
    pub fn mark_test(&mut self, id: u64) {
        if let Some(tab) = self.get_mut(id) {
            tab.test = true;
        }
    }

    /// Whether a visit or title in this tab may go into history.
    pub fn records(&self, id: u64) -> bool {
        self.get(id).is_some_and(|t| !t.test && !t.private)
    }

    pub fn pop_closed(&mut self) -> Option<Closed> {
        self.closed.pop()
    }

    /// The tab `offset` positions away from the active one, wrapping
    /// around; the tabs of a folded group don't count.
    pub fn neighbour(&self, offset: isize) -> Option<u64> {
        let shown: Vec<u64> =
            self.tabs.iter().filter(|t| !self.tab_hidden(t) || Some(t.id) == self.active).map(|t| t.id).collect();
        let current = shown.iter().position(|id| Some(*id) == self.active)? as isize;
        let index = (current + offset).rem_euclid(shown.len() as isize);
        shown.get(index as usize).copied()
    }

    /// Ctrl+1..8 go to that position; Ctrl+9 to the last tab.
    pub fn by_number(&self, n: u8) -> Option<u64> {
        if n == 9 {
            return self.tabs.last().map(|t| t.id);
        }
        self.tabs.get(n as usize - 1).map(|t| t.id)
    }

    /// Pinning moves the tab to the end of the pinned group; unpinning to the
    /// start of the rest.
    pub fn toggle_pin(&mut self, id: u64) {
        let Some(index) = self.index_of(id) else { return };
        // Pinned tabs are the ones kept for good; a private one is not.
        if self.tabs[index].private {
            return;
        }
        let mut tab = self.tabs.remove(index);
        tab.pinned = !tab.pinned;
        // A pinned tab belongs to no group.
        let grouped = tab.group.take().is_some();
        let target = self.pinned_count();
        self.tabs.insert(target, tab);
        if grouped {
            self.normalize_groups();
        }
    }

    pub fn move_to(&mut self, id: u64, to: usize) {
        let Some(from) = self.index_of(id) else { return };
        let pinned = self.tabs[from].pinned;
        let tab = self.tabs.remove(from);
        let pinned_count = self.pinned_count();
        let to = if pinned { to.min(pinned_count) } else { to.clamp(pinned_count, self.tabs.len()) };
        self.tabs.insert(to, tab);
    }
}

