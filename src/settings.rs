//! User settings (settings.json). Every field has a default, so a missing or
//! partial file is fine and new fields never break an old file.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Where the single field floats over the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldPosition {
    /// Under the top edge of the page: where it always was.
    #[default]
    Top,
    /// In the middle of the page, as a command palette.
    Middle,
    /// On the bottom edge; its list opens upwards.
    Bottom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Where the field opens (Settings > Field).
    pub field_position: FieldPosition,
    /// `{q}` is replaced by the encoded query.
    pub search_url: String,
    /// Seconds a background tab stays live before it is suspended.
    pub suspend_after_secs: u64,
    /// Minutes a background tab stays out of sight before its view is
    /// destroyed. 0: never. A suspended tab uses almost no memory of the
    /// computer and comes back as it was, so the default is 0: a tab is
    /// discarded only when the system is low on memory.
    pub discard_after_mins: u64,
    /// Views out of sight that stay live at most (the most recently seen);
    /// the others are discarded at once. 0: no limit. Views out of sight
    /// for `memory::LONG_OUT_OF_SIGHT` keep only half of it.
    pub live_tabs_limit: usize,
    /// System memory load (percent) from which background tabs are suspended
    /// at once and only a few stay live (`memory::Pressure::Low`).
    pub memory_pressure_percent: u32,
    /// Hosts that never sleep nor get discarded (they cost more memory).
    pub keep_awake: BTreeSet<String>,
    /// Hosts where ad and tracker blocking is off (it is on everywhere else).
    pub shield_disabled: BTreeSet<String>,
    /// Sites (same key as `shield_disabled`) that may open windows on their
    /// own; everywhere else a window nobody clicked for is blocked.
    pub popups_allowed: BTreeSet<String>,
    /// Engine tracking prevention (strict) on the main profile, and cookie
    /// banners hidden with the cookie list. Off turns both off.
    pub block_trackers: bool,
    /// Command name -> "Ctrl+Shift+X" (comma separated for several).
    pub shortcuts: HashMap<String, String>,
    /// Two pages side by side (Ctrl+Alt+N, or a tab dropped on the page). On
    /// by default: it costs nothing until a pair exists.
    pub split_view: bool,
    /// A picture of the page on a tab's hover card (the card shows anyway).
    pub hover_preview: bool,
    /// The bookmarks bar: chips under a top strip, tiles at the top of the
    /// column. It shows only when there is a bookmark.
    pub show_bookmarks_bar: bool,
    /// Look for a new version once a day.
    pub check_updates: bool,
    /// Manifest address; empty means the one built in (`updater`).
    pub update_url: String,
    /// The local pipe scripts drive the browser through (tests only).
    pub bench_enabled: bool,
    /// Offer to save a password after a login (filling is always on request).
    pub passwords_offer: bool,
    /// Origins where saving is never offered ("Never for this site").
    pub passwords_never: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            field_position: FieldPosition::Top,
            search_url: "https://duckduckgo.com/?q={q}".to_string(),
            suspend_after_secs: 60,
            discard_after_mins: 0,
            live_tabs_limit: 0,
            memory_pressure_percent: 85,
            keep_awake: BTreeSet::new(),
            shield_disabled: BTreeSet::new(),
            popups_allowed: BTreeSet::new(),
            block_trackers: true,
            shortcuts: HashMap::new(),
            split_view: true,
            hover_preview: true,
            show_bookmarks_bar: true,
            check_updates: true,
            update_url: String::new(),
            bench_enabled: false,
            passwords_offer: true,
            passwords_never: BTreeSet::new(),
        }
    }
}

/// What the Settings window hands back, on top of the settings in force. The
/// window edits a copy taken when it opened and has no control for the
/// per-site lists the app changes from a tab (shield exceptions, "never for
/// this site") nor for the update address: those stay as the app has them.
pub fn from_window(current: &Settings, edited: Settings) -> Settings {
    Settings {
        shield_disabled: current.shield_disabled.clone(),
        popups_allowed: current.popups_allowed.clone(),
        passwords_never: current.passwords_never.clone(),
        update_url: current.update_url.clone(),
        ..edited
    }
}

/// A search address the field can use: http(s) with a `{q}` for the query.
pub fn valid_search_url(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && text.contains("{q}")
        && !text.contains(char::is_whitespace)
}

/// A whole number within `range`, as typed in a settings field.
pub fn parse_number(text: &str, range: std::ops::RangeInclusive<u64>) -> Option<u64> {
    text.trim().parse::<u64>().ok().filter(|n| range.contains(n))
}

/// The host a "keep awake" entry is stored as, the way tabs are compared
/// against it: no scheme, no path, no "www.". "https://www.YouTube.com/x" -> "youtube.com".
pub fn normalize_host(text: &str) -> Option<String> {
    let text = text.trim().to_lowercase();
    let host = crate::omnibox::display_host(&text);
    (!host.is_empty() && !host.contains(char::is_whitespace) && host.contains(|c: char| c.is_ascii_alphanumeric()))
        .then(|| host.to_string())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/settings.rs"]
#[rustfmt::skip]
mod tests;
