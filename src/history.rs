//! Local history and the autocomplete built on it. A flat vector scanned on
//! each keystroke: tens of thousands of entries take well under a millisecond
//! and cost no extra index in memory.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::omnibox::strip_for_match;

const MAX_ENTRIES: usize = 20_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    pub visits: u32,
    /// Unix seconds.
    pub last: u64,
}

#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
    by_url: HashMap<String, usize>,
    pub dirty: bool,
}

impl History {
    pub fn from_entries(mut entries: Vec<Entry>) -> Self {
        entries.sort_by_key(|e| std::cmp::Reverse(e.last));
        entries.truncate(MAX_ENTRIES);
        let by_url = entries.iter().enumerate().map(|(i, e)| (e.url.clone(), i)).collect();
        Self { entries, by_url, dirty: false }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn record_visit(&mut self, url: &str, title: &str, now: u64) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return;
        }
        if let Some(&i) = self.by_url.get(url) {
            let entry = &mut self.entries[i];
            entry.visits += 1;
            entry.last = now;
            if !title.is_empty() {
                entry.title = title.to_string();
            }
        } else {
            if self.entries.len() >= MAX_ENTRIES {
                self.evict_oldest();
            }
            self.by_url.insert(url.to_string(), self.entries.len());
            self.entries.push(Entry { url: url.to_string(), title: title.to_string(), visits: 1, last: now });
        }
        self.dirty = true;
    }

    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(&i) = self.by_url.get(url)
            && self.entries[i].title != title
            && !title.is_empty()
        {
            self.entries[i].title = title.to_string();
            self.dirty = true;
        }
    }

    /// Forgets one address. True when it was there.
    pub fn remove(&mut self, url: &str) -> bool {
        let Some(index) = self.by_url.remove(url) else { return false };
        self.entries.swap_remove(index);
        if let Some(moved) = self.entries.get(index) {
            self.by_url.insert(moved.url.clone(), index);
        }
        self.dirty = true;
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.by_url.clear();
        self.dirty = true;
    }

    /// The panel's list: entries with every word typed in the title or the
    /// address, most recent first.
    pub fn recent(&self, query: &str, limit: usize) -> Vec<&Entry> {
        let query = crate::panels::Query::new(query);
        let mut found: Vec<&Entry> = self.entries.iter().filter(|e| query.matches(&[&e.title, &e.url])).collect();
        found.sort_by_key(|e| std::cmp::Reverse(e.last));
        found.truncate(limit);
        found
    }

    fn evict_oldest(&mut self) {
        if let Some((oldest, _)) = self.entries.iter().enumerate().min_by_key(|(_, e)| e.last) {
            let removed = self.entries.swap_remove(oldest);
            self.by_url.remove(&removed.url);
            if let Some(moved) = self.entries.get(oldest) {
                self.by_url.insert(moved.url.clone(), oldest);
            }
        }
    }

    /// Best matches for the typed text: prefix of the address first, then
    /// words of the title or address, weighted by visits and recency.
    pub fn suggest(&self, query: &str, limit: usize, now: u64) -> Vec<&Entry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        // Every keystroke scans the whole history: no lowercase copies of
        // twenty thousand addresses and titles each time.
        let mut scored: Vec<(f64, &Entry)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let address = strip_for_match(&e.url);
                let base = if starts_with_folded(address, &query) {
                    3.0
                } else if contains_folded(address, &query) {
                    1.5
                } else if contains_folded(&e.title, &query) {
                    1.0
                } else {
                    return None;
                };
                let age_days = now.saturating_sub(e.last) as f64 / 86_400.0;
                let frequency = (e.visits as f64).ln_1p();
                let short = 1.0 / (1.0 + address.len() as f64 / 40.0);
                Some((base + frequency + short - age_days.min(90.0) / 30.0, e))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }

    /// Text to complete inline after what was typed: the rest of the best
    /// host (or address) that starts with it. "git" -> "hub.com".
    pub fn inline_completion(&self, typed: &str) -> Option<String> {
        let typed_lower = typed.to_lowercase();
        if typed_lower.is_empty() || typed_lower.contains(char::is_whitespace) {
            return None;
        }
        let mut best: Option<(&str, u32)> = None;
        for entry in &self.entries {
            let address = strip_for_match(&entry.url);
            if !starts_with_folded(address, &typed_lower) {
                continue;
            }
            // Complete up to the end of the host when the typed text is still
            // inside it; otherwise up to the whole address.
            let host_end = address.find('/').unwrap_or(address.len());
            let candidate = if typed.len() < host_end { &address[..host_end] } else { address };
            let candidate = candidate.trim_end_matches('/');
            if candidate.len() <= typed.len() {
                continue;
            }
            if best.is_none_or(|(_, visits)| entry.visits > visits) {
                best = Some((candidate, entry.visits));
            }
        }
        best.map(|(candidate, _)| candidate[typed.len()..].to_string())
    }
}

/// `text` starts with `lower` (already lowercase) ignoring case. ASCII is
/// compared in place; anything else falls back to a lowercase copy.
fn starts_with_folded(text: &str, lower: &str) -> bool {
    if lower.is_ascii() {
        let (text, lower) = (text.as_bytes(), lower.as_bytes());
        return text.len() >= lower.len() && text[..lower.len()].eq_ignore_ascii_case(lower);
    }
    text.to_lowercase().starts_with(lower)
}

/// `text` contains `lower` (already lowercase) ignoring case, as above.
fn contains_folded(text: &str, lower: &str) -> bool {
    if lower.is_empty() {
        return true;
    }
    if lower.is_ascii() {
        let (text, lower) = (text.as_bytes(), lower.as_bytes());
        return text.windows(lower.len()).any(|w| w.eq_ignore_ascii_case(lower));
    }
    text.to_lowercase().contains(lower)
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/history.rs"]
#[rustfmt::skip]
mod tests;
