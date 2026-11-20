//! New windows a page asks for: opened as a tab next to it, or blocked
//! (`shield::popups` decides) with a note in the page saying how to allow
//! them on that site.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use super::App;
use crate::{inject, shield, shield::popups::Verdict, tabs::Live};

const BLOCKED_NOTE: &str = "Ventana emergente bloqueada · Para permitirlas en este sitio: clic derecho en la pestaña";
/// A page that keeps trying gets one note, not one per attempt.
const NOTE_EVERY: Duration = Duration::from_secs(4);

#[derive(Default)]
pub(super) struct Popups {
    /// Sites allowed from a private tab: never written to settings.json.
    private_allowed: HashSet<String>,
    last_note: HashMap<u64, Instant>,
    /// Blocked since start, for the script control pipe.
    pub(super) blocked: u64,
    /// The last decisions ("Open https://…"), for the same pipe.
    pub(super) recent: std::collections::VecDeque<String>,
}

impl App {
    fn popups_allowed(&self, private: bool, site: &str) -> bool {
        if private { self.popups.private_allowed.contains(site) } else { self.settings.popups_allowed.contains(site) }
    }

    pub(super) fn new_window(&mut self, from: u64, url: String, user_initiated: bool) {
        let Some(tab) = self.tabs.get(from) else { return };
        let (opener, private) = (tab.url.clone(), tab.private);
        let allowed = self.popups_allowed(private, shield::site_key(&opener));
        let verdict = shield::popup_verdict(&opener, &url, user_initiated, allowed);
        if self.popups.recent.len() == 8 {
            self.popups.recent.pop_front();
        }
        self.popups.recent.push_back(format!("{verdict:?} user={user_initiated} {url}"));
        if verdict != Verdict::Open {
            #[cfg(debug_assertions)]
            eprintln!("popups: {verdict:?} {url} from {opener}");
            self.popups.blocked += 1;
            self.note_blocked(from);
            return;
        }
        let at = self.tabs.index_of(from).map(|i| i + 1);
        let test = !self.tabs.records(from) && !private;
        if private {
            self.open_private_tab(&url, at);
        } else {
            self.open_tab(&url, at);
            // What a test tab opens is a test tab too.
            if let (true, Some(id)) = (test, self.tabs.active) {
                self.tabs.mark_test(id);
            }
        }
    }

    fn note_blocked(&mut self, tab: u64) {
        let now = Instant::now();
        if self.popups.last_note.get(&tab).is_some_and(|at| now.duration_since(*at) < NOTE_EVERY) {
            return;
        }
        self.popups.last_note.retain(|id, _| self.tabs.get(*id).is_some());
        self.popups.last_note.insert(tab, now);
        if let Some(Live::Ready(web)) = self.tabs.get(tab).map(|t| &t.live) {
            // Without a gesture: one would let the page's next try through.
            inject::execute_without_gesture(&web.webview, &inject::toast_script(BLOCKED_NOTE));
        }
    }

    /// Whether the site of `url` may open windows on its own, as the tab
    /// menu shows it.
    pub(super) fn popups_allowed_for(&self, url: &str, private: bool) -> bool {
        self.popups_allowed(private, shield::site_key(url))
    }

    /// The tab menu's "Permitir ventanas emergentes en este sitio". From a
    /// private tab the choice lasts until the app closes.
    pub(super) fn toggle_popups(&mut self) {
        let Some(tab) = self.tabs.active_tab() else { return };
        let site = shield::site_key(&tab.url).to_string();
        if site.is_empty() {
            return;
        }
        if tab.private {
            if !self.popups.private_allowed.remove(&site) {
                self.popups.private_allowed.insert(site);
            }
            return;
        }
        if !self.settings.popups_allowed.remove(&site) {
            self.settings.popups_allowed.insert(site);
        }
        self.save_settings();
        // An open Ajustes window holds a copy: its next change would undo this.
        crate::settings_window::sync(&self.settings);
    }
}
