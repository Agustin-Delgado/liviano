//! The downloads list (downloads.json). The engine does the downloading; this
//! only remembers what it did. Only downloads still in progress stay in
//! memory (their progress changes every moment); the file is read when the
//! panel opens and written when a download starts, ends or is removed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{panels, storage};

const MAX_ENTRIES: usize = 300;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    InProgress,
    Completed,
    Interrupted,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Download {
    pub id: u64,
    pub url: String,
    /// Where the file is (or will be) on disk.
    pub path: String,
    pub state: State,
    pub received: u64,
    /// None while the server hasn't said.
    pub total: Option<u64>,
    /// Unix seconds.
    pub started: u64,
}

impl Download {
    /// The file name, from the path (or the address while there is none).
    pub fn name(&self) -> &str {
        let from_path = Path::new(&self.path).file_name().and_then(|n| n.to_str());
        from_path.filter(|n| !n.is_empty()).unwrap_or_else(|| {
            let without_query = self.url.split(['?', '#']).next().unwrap_or("");
            without_query.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&self.url)
        })
    }

    /// The panel's row: "report.pdf    45% · 1.2 MB of 2.7 MB".
    pub fn row(&self, file_exists: bool) -> String {
        let status = match self.state {
            State::InProgress => match self.total {
                Some(total) if total > 0 => format!(
                    "{}% · {} of {}",
                    (self.received.min(total) * 100 / total),
                    panels::format_bytes(self.received),
                    panels::format_bytes(total)
                ),
                _ => format!("Downloading · {}", panels::format_bytes(self.received)),
            },
            State::Completed if !file_exists => "File deleted or moved".to_string(),
            State::Completed => format!("Completed · {}", panels::format_bytes(self.total.unwrap_or(self.received))),
            State::Interrupted => "Interrupted".to_string(),
        };
        format!("{}    {status}", self.name())
    }
}

fn path() -> PathBuf {
    storage::data_dir().join("downloads.json")
}

pub fn load() -> Vec<Download> {
    storage::read_json(&path())
}

pub fn save(list: &[Download]) {
    let _ = storage::write_json(&path(), &list);
}

/// Puts `download` in the list (replacing the one with its id), newest first.
pub fn upsert(list: &mut Vec<Download>, download: Download) {
    match list.iter_mut().find(|d| d.id == download.id) {
        Some(existing) => *existing = download,
        None => list.insert(0, download),
    }
    list.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
    list.truncate(MAX_ENTRIES);
}

/// The file's list with the live progress of the downloads in memory on top.
pub fn merged(mut saved: Vec<Download>, active: &[Download]) -> Vec<Download> {
    for download in active {
        upsert(&mut saved, download.clone());
    }
    saved
}

pub fn search<'a>(list: &'a [Download], query: &str) -> Vec<&'a Download> {
    list.iter().filter(|d| panels::matches(query, &[d.name(), &d.url])).collect()
}

/// Reads the file, applies `change` and writes it back: one short read and
/// one atomic write per start, end or removal, nothing held in between.
pub fn update_file(change: impl FnOnce(&mut Vec<Download>)) {
    let mut list = load();
    change(&mut list);
    save(&list);
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/downloads.rs"]
#[rustfmt::skip]
mod tests;
