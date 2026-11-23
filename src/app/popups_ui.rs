//! New windows a page asks for: opened as a tab next to it, or blocked
//! (`shield::popups` decides) with a note in the page saying how to allow
//! them on that site.
//!
//! The new tab is the window that the page asked for, as in other
//! browsers: the engine waits until the view of the tab is ready and then
//! gives it to the page (`web::answer_new_window`). Thus `window.open`
//! returns the window, the new page has its `window.opener`, and a sign-in
//! pop-up ("Sign in with Google") sends its result to the page with
//! `postMessage` and closes itself. Refer to docs/sign-in.md.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use super::App;
use crate::{inject, shield, shield::popups::Verdict, tabs::Live, web};

const BLOCKED_NOTE: &str = "Pop-up blocked · To allow pop-ups on this site, right-click the tab";
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
    pub(super) openings: Openings,
}

/// The tabs that pages opened as their new windows, while they last.
/// Pure bookkeeping: the engine calls are in `App`.
#[derive(Debug, Default)]
pub(super) struct Openings {
    /// A new tab -> the engine request that waits for its view.
    waiting: HashMap<u64, u64>,
    /// A tab that a page opened -> the tab of that page (until it closes),
    /// and whether it is a sign-in pop-up (a window of a given size).
    opened: HashMap<u64, (Option<u64>, bool)>,
}

impl Openings {
    /// Tab `tab` was opened for the new-window `request` of the page in
    /// tab `opener`.
    pub(super) fn wait(&mut self, tab: u64, request: u64, opener: u64, popup: bool) {
        self.waiting.insert(tab, request);
        self.opened.insert(tab, (Some(opener), popup));
    }

    /// The view of `tab` is ready: the request that it answers, if any.
    /// Only the first view answers it. A later view (after a discard or a
    /// crash) loads the address again, with no opener.
    pub(super) fn take_request(&mut self, tab: u64) -> Option<u64> {
        self.waiting.remove(&tab)
    }

    /// The page of `tab` is no longer the window that a page opened: its
    /// view loaded the address by itself, or it was discarded.
    pub(super) fn forget_page(&mut self, tab: u64) {
        self.opened.remove(&tab);
    }

    /// `tab` closed: what it waited for and what it opened is forgotten.
    /// The request that still waits, if any, has to be answered with no
    /// window.
    pub(super) fn closed(&mut self, tab: u64) -> Option<u64> {
        self.opened.remove(&tab);
        for (opener, _) in self.opened.values_mut() {
            if *opener == Some(tab) {
                *opener = None;
            }
        }
        self.waiting.remove(&tab)
    }

    /// A page may close its own tab only if a page opened it, as in
    /// other browsers. A tab that the person opened stays.
    pub(super) fn may_close(&self, tab: u64) -> bool {
        self.opened.contains_key(&tab)
    }

    pub(super) fn opener(&self, tab: u64) -> Option<u64> {
        self.opened.get(&tab).and_then(|(opener, _)| *opener)
    }

    /// The page of `tab` waits for the result of a sign-in pop-up that it
    /// opened: it stays live (not suspended, not discarded) until the
    /// pop-up closes, so that the result arrives at once.
    pub(super) fn waits_for_popup(&self, tab: u64) -> bool {
        self.opened.values().any(|(opener, popup)| *opener == Some(tab) && *popup)
    }
}

impl App {
    fn popups_allowed(&self, private: bool, site: &str) -> bool {
        if private { self.popups.private_allowed.contains(site) } else { self.settings.popups_allowed.contains(site) }
    }

    pub(super) fn new_window(&mut self, from: u64, url: String, user_initiated: bool, request: web::NewWindowRequest) {
        let Some(tab) = self.tabs.get(from) else {
            web::answer_new_window(request.id, None);
            return;
        };
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
            web::answer_new_window(request.id, None);
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
        match self.tabs.active.filter(|id| *id != from) {
            Some(id) => self.popups.openings.wait(id, request.id, from, request.popup),
            None => {
                web::answer_new_window(request.id, None);
            }
        }
    }

    /// The view of a tab that a page opened is ready: it becomes that
    /// page's new window. False when it does not (no request, or the
    /// engine refused the view): the caller loads the address.
    pub(super) fn answer_opening(
        &mut self,
        id: u64,
        view: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
    ) -> bool {
        let answered =
            self.popups.openings.take_request(id).is_some_and(|request| web::answer_new_window(request, Some(view)));
        if !answered {
            self.popups.openings.forget_page(id);
        }
        answered
    }

    /// A tab closed: a request that waited for its view ends with no window.
    pub(super) fn opening_closed(&mut self, id: u64) {
        if let Some(request) = self.popups.openings.closed(id) {
            web::answer_new_window(request, None);
        }
    }

    /// The page of a tab called `window.close()`. Only a tab that a page
    /// opened closes (a sign-in pop-up when it is done), and the page that
    /// opened it comes back to the front.
    pub(super) fn page_closed_itself(&mut self, id: u64) {
        if !self.popups.openings.may_close(id) {
            return;
        }
        if self.tabs.active == Some(id)
            && let Some(opener) = self.popups.openings.opener(id).filter(|o| self.tabs.get(*o).is_some())
        {
            self.select(opener);
        }
        self.close_tab(id);
    }

    pub(super) fn waits_for_popup(&self, tab: u64) -> bool {
        self.popups.openings.waits_for_popup(tab)
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

    /// The tab menu's "Allow pop-ups on this site". From a
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
        // An open Settings window holds a copy: its next change would undo this.
        crate::settings_window::sync(&self.settings);
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/popups_ui.rs"]
#[rustfmt::skip]
mod tests;
