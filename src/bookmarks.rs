//! Bookmarks (bookmarks.json): a flat list in the order of the bookmarks bar.
//! A new bookmark goes first; a drag on the bar moves it. The panel and the
//! field read the file when they need it and drop the copy when they close.
//! Only the bar keeps a small copy in memory (`bookmark_bar`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{panels, storage};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    /// Unix seconds.
    pub added: u64,
}

#[derive(Debug, Default, PartialEq)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
}

fn path() -> PathBuf {
    storage::data_dir().join("bookmarks.json")
}

impl Bookmarks {
    pub fn load() -> Self {
        Self { items: storage::read_json(&path()) }
    }

    pub fn save(&self) -> std::io::Result<()> {
        storage::write_json(&path(), &self.items)
    }

    /// Marks the page, or unmarks it if it already was. True when it ends marked.
    pub fn toggle(&mut self, url: &str, title: &str, now: u64) -> bool {
        if self.remove(url) {
            return false;
        }
        self.items.insert(0, Bookmark { url: url.to_string(), title: title.to_string(), added: now });
        true
    }

    /// Gives the bookmark of `url` a new title. True when it exists.
    pub fn rename(&mut self, url: &str, title: &str) -> bool {
        match self.items.iter_mut().find(|b| b.url == url) {
            Some(b) => {
                b.title = title.to_string();
                true
            }
            None => false,
        }
    }

    /// Puts the bookmarks in the order of the addresses in `order` (the
    /// bar's order after a drag). One not in `order` goes to the end, in the
    /// order it had. True when the order changed.
    pub fn arrange(&mut self, order: &[String]) -> bool {
        let before: Vec<String> = self.items.iter().map(|b| b.url.clone()).collect();
        self.items.sort_by_key(|b| order.iter().position(|u| *u == b.url).unwrap_or(usize::MAX));
        self.items.iter().map(|b| &b.url).ne(before.iter())
    }

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.url != url);
        self.items.len() != before
    }

    /// The panel's list: every bookmark whose title or address has all the
    /// words typed, in the order of the bar.
    pub fn search(&self, query: &str) -> Vec<&Bookmark> {
        self.items.iter().filter(|b| panels::matches(query, &[&b.title, &b.url])).collect()
    }

    /// For the single field: bookmarks whose address starts with what was
    /// typed come first, then any other match.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<&Bookmark> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let typed = query.trim().to_lowercase();
        let mut found = self.search(query);
        found.sort_by_key(|b| !crate::omnibox::strip_for_match(&b.url).to_lowercase().starts_with(&typed));
        found.truncate(limit);
        found
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/bookmarks.rs"]
#[rustfmt::skip]
mod tests;
