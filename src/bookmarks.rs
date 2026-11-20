//! Bookmarks (bookmarks.json): a flat list, newest first. Nothing keeps them
//! in memory: the file is read when a panel or the field needs it and the
//! copy is dropped when it closes.

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

    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.url != url);
        self.items.len() != before
    }

    /// The panel's list: every bookmark whose title or address has all the
    /// words typed, newest first.
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

