//! What survives a restart: open tabs (metadata only), their spaces and
//! groups, split pairs and their widths, layout and window position. Restoring creates no web view except
//! the active one (and its partner, when it is split).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    /// Position in `Session::groups` of the tab's group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<usize>,
    /// Position in `Session::spaces` of the tab's space (0 for a session
    /// written before spaces existed).
    #[serde(skip_serializing_if = "is_zero")]
    pub space: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub active: usize,
    pub vertical_tabs: bool,
    pub sidebar_collapsed: bool,
    /// left, top, right, bottom in screen pixels; None the first time.
    pub window: Option<[i32; 4]>,
    pub maximized: bool,
    /// Split view pairs, by position in `tabs` (see `split`).
    pub splits: Vec<crate::split::SavedPair>,
    /// Tab groups: name, colour and whether folded; their tabs point at
    /// them by position (see `tab_groups`).
    pub groups: Vec<crate::tab_groups::SavedGroup>,
    /// Spaces: name, colour and live tab; their tabs point at them by
    /// position (see `spaces`). Empty: one space holds every tab.
    pub spaces: Vec<crate::spaces::SavedSpace>,
    /// Position in `spaces` of the space on the strip.
    pub active_space: usize,
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/session.rs"]
#[rustfmt::skip]
mod tests;
