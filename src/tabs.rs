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
    next_id: u64,
    closed: Vec<Closed>,
}

impl<W> Default for TabList<W> {
    fn default() -> Self {
        Self { tabs: Vec::new(), active: None, next_id: 1, closed: Vec::new() }
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

    fn pinned_count(&self) -> usize {
        self.tabs.iter().filter(|t| t.pinned).count()
    }

    /// Inserts an asleep tab. `at` is clamped so unpinned tabs never sit
    /// among the pinned ones.
    pub fn insert(&mut self, url: &str, title: &str, pinned: bool, at: Option<usize>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
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
            },
        );
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

    pub fn select(&mut self, id: u64) -> bool {
        if self.get(id).is_none() {
            return false;
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
            self.active = self.tabs.get(index).or_else(|| self.tabs.last()).map(|t| t.id);
            if let Some(next) = self.active {
                self.select(next);
            }
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

    /// The tab `offset` positions away from the active one, wrapping around.
    pub fn neighbour(&self, offset: isize) -> Option<u64> {
        let len = self.tabs.len() as isize;
        let current = self.active_index()? as isize;
        let index = (current + offset).rem_euclid(len);
        self.tabs.get(index as usize).map(|t| t.id)
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
        let target = self.pinned_count();
        self.tabs.insert(target, tab);
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

