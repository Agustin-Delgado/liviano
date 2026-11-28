//! Web notifications, the window's part (refer to `notify_watch` and
//! `notifications`): the question a site asks before it may notify, the
//! decision kept per site, the toasts and what a click on one does.
//!
//! A site asks once. The answer goes to settings.json (Settings > Privacy
//! can forget them all) and to the engine's profile, so that the page's
//! `Notification.permission` says it on the next visit. A private tab's
//! answer lasts while Nea runs. A site that may notify never sleeps: a
//! sleeping page sends nothing.

use std::collections::{HashMap, VecDeque};

use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2Notification};
use windows::{
    UI::Notifications::ToastNotification,
    Win32::{
        Foundation::POINT,
        Graphics::Gdi::ClientToScreen,
        UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow},
    },
};

use super::{App, Live, with_app};
use crate::{
    notifications::{self, Outcome, Toast, Toasts},
    notify_watch::{self, Answer, Message, Received, Report},
    vault_bar::{self, Choice, VaultBar},
};

/// The buttons of the question (DIPs): three short words.
const WIDTHS: [f32; 3] = [92.0, 92.0, 92.0];

#[derive(Default)]
pub(super) struct Notify {
    asking: Option<Ask>,
    bar: Option<VaultBar>,
    /// Questions behind the one on screen, in order.
    waiting: VecDeque<Ask>,
    /// The answers of private tabs, for this run only.
    private: HashMap<String, bool>,
    shown: HashMap<u64, Shown>,
    next: u64,
    /// `None` until the first toast; then what Windows gave.
    toasts: Option<Option<Toasts>>,
    /// The profile's own decisions were read into the settings.
    imported: bool,
}

struct Ask {
    tab: u64,
    origin: String,
    private: bool,
    request: u64,
}

struct Shown {
    tab: u64,
    tag: String,
    notification: ICoreWebView2Notification,
    toast: ToastNotification,
}

fn on_choice(choice: Choice) {
    with_app(|app| app.notify_choice(choice));
}

/// What the decision for a site is, from what was stored.
pub fn decided(decision: Option<bool>) -> Option<Answer> {
    decision.map(|allowed| if allowed { Answer::Allow } else { Answer::Block })
}

impl App {
    pub(super) fn on_notify(&mut self, tab: u64, message: Message) {
        match message {
            Message::Asked(uri, request) => self.notify_asked(tab, &uri, request),
            Message::Received(received) => self.notify_received(tab, received),
            Message::Closed(key) => {
                if let Some(shown) = self.notify.shown.remove(&key) {
                    if let Some(Some(toasts)) = &self.notify.toasts {
                        toasts.hide(&shown.toast);
                    }
                    notify_watch::report(&shown.notification, Report::Closed);
                }
            }
        }
    }

    /// The stored decision for a site, of a private tab or not.
    fn notify_decision(&self, origin: &str, private: bool) -> Option<bool> {
        if private {
            self.notify.private.get(origin).copied()
        } else {
            self.settings.notifications.get(origin).copied()
        }
    }

    /// The page of this tab may notify: it must not sleep.
    pub(super) fn notifies(&self, url: &str, private: bool) -> bool {
        !private
            && notifications::origin_of(url)
                .is_some_and(|origin| self.settings.notifications.get(&origin) == Some(&true))
    }

    fn notify_asked(&mut self, tab: u64, uri: &str, request: u64) {
        let private = self.tabs.get(tab).is_some_and(|t| t.private);
        let Some(origin) = notifications::origin_of(uri) else {
            // Not a secure page: Chrome does not ask either.
            notify_watch::answer(request, Answer::Block, false);
            return;
        };
        if let Some(answer) = decided(self.notify_decision(&origin, private)) {
            notify_watch::answer(request, answer, !private);
            return;
        }
        let ask = Ask { tab, origin, private, request };
        if self.notify.asking.is_some() {
            self.notify.waiting.push_back(ask);
        } else {
            self.notify_ask(ask);
        }
    }

    fn notify_ask(&mut self, ask: Ask) {
        let question = format!("{} wants to show notifications.", notifications::site_label(&ask.origin));
        self.notify.bar = None;
        self.notify.bar = VaultBar::create(
            self.hwnd,
            &self.palette,
            self.font,
            self.scale,
            [&question, "Allow", "Block", "Not now"],
            WIDTHS,
            on_choice,
        );
        self.notify.asking = Some(ask);
        self.place_notify_bar();
    }

    fn notify_choice(&mut self, choice: Choice) {
        let Some(ask) = self.notify.asking.take() else { return };
        self.notify.bar = None;
        let answer = match choice {
            Choice::Save => Answer::Allow,
            Choice::Never => Answer::Block,
            Choice::NotNow => Answer::Later,
        };
        if answer != Answer::Later {
            let allowed = answer == Answer::Allow;
            if ask.private {
                self.notify.private.insert(ask.origin.clone(), allowed);
            } else {
                self.settings.notifications.insert(ask.origin.clone(), allowed);
                self.save_settings();
                crate::settings_window::sync(&self.settings);
            }
        }
        notify_watch::answer(ask.request, answer, !ask.private);
        // The same site asked from other tabs: one answer for all of them.
        let (same, rest): (VecDeque<Ask>, VecDeque<Ask>) = std::mem::take(&mut self.notify.waiting)
            .into_iter()
            .partition(|a| a.origin == ask.origin && a.private == ask.private);
        for other in same {
            notify_watch::answer(other.request, answer, !other.private);
        }
        self.notify.waiting = rest;
        self.notify_next();
    }

    /// The next question that still has a tab to ask from.
    fn notify_next(&mut self) {
        while let Some(ask) = self.notify.waiting.pop_front() {
            if self.tabs.get(ask.tab).is_none() {
                notify_watch::answer(ask.request, Answer::Later, false);
                continue;
            }
            if let Some(answer) = decided(self.notify_decision(&ask.origin, ask.private)) {
                notify_watch::answer(ask.request, answer, !ask.private);
                continue;
            }
            self.notify_ask(ask);
            return;
        }
    }

    /// A tab closed: its question goes with it.
    pub(super) fn notify_tab_closed(&mut self, tab: u64) {
        if self.notify.asking.as_ref().is_some_and(|a| a.tab == tab) {
            self.notify_choice(Choice::NotNow);
        }
        let (gone, rest): (VecDeque<Ask>, VecDeque<Ask>) =
            std::mem::take(&mut self.notify.waiting).into_iter().partition(|a| a.tab == tab);
        for ask in gone {
            notify_watch::answer(ask.request, Answer::Later, false);
        }
        self.notify.waiting = rest;
    }

    /// Keeps the question at the page's top right corner, under the
    /// password offer if one stands, shown only over its tab.
    pub(super) fn place_notify_bar(&self) {
        let (Some(ask), Some(bar)) = (&self.notify.asking, &self.notify.bar) else { return };
        if self.tabs.active != Some(ask.tab) || self.locked() {
            bar.hide();
            return;
        }
        let content = self.layout.content;
        let margin = (8.0 * self.scale).round() as i32;
        let below =
            if self.password_bar_shown() { (vault_bar::HEIGHT * self.scale).round() as i32 + margin } else { 0 };
        let mut corner = POINT { x: content.right - margin, y: content.top + margin + below };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut corner);
        }
        bar.place(corner.x, corner.y);
    }

    pub(super) fn restyle_notify(&mut self) {
        let (palette, font, scale) = (self.palette, self.font, self.scale);
        if let Some(bar) = &mut self.notify.bar {
            bar.restyle(&palette, font, scale);
        }
        self.place_notify_bar();
    }

    fn notify_received(&mut self, tab: u64, received: Received) {
        let page = self.tabs.get(tab).map(|t| t.url.clone()).unwrap_or_default();
        let Some(origin) = notifications::origin_of(&received.origin).or_else(|| notifications::origin_of(&page))
        else {
            notify_watch::report(&received.notification, Report::Closed);
            return;
        };
        if self.notify.toasts.is_none() {
            self.notify.toasts = Some(Toasts::new(notifications::AUMID));
        }
        let Some(Some(toasts)) = &self.notify.toasts else {
            notify_watch::report(&received.notification, Report::Closed);
            return;
        };
        self.notify.next += 1;
        let key = self.notify.next;
        let tag = if received.tag.is_empty() { key.to_string() } else { received.tag.clone() };
        let tag = notifications::toast_tag(&origin, &tag);
        let group = notifications::toast_tag(&origin, "");
        let toast = Toast {
            title: &received.title,
            body: &received.body,
            site: notifications::site_label(&origin),
            silent: received.silent,
            tag: &tag,
            group: &group,
            quiet: false,
        };
        match toasts.show(&toast, key) {
            Ok(shown) => {
                // A notification with the tag of an earlier one replaced it.
                let replaced: Vec<u64> =
                    self.notify.shown.iter().filter(|(_, s)| s.tag == tag).map(|(k, _)| *k).collect();
                for old in replaced {
                    if let Some(old) = self.notify.shown.remove(&old) {
                        notify_watch::report(&old.notification, Report::Closed);
                    }
                }
                notify_watch::report(&received.notification, Report::Shown);
                notify_watch::watch_close(&received.notification, tab, key);
                self.notify.shown.insert(key, Shown { tab, tag, notification: received.notification, toast: shown });
            }
            Err(error) => {
                crate::diag::trace(|| format!("toast failed: {error}"));
                notify_watch::report(&received.notification, Report::Closed);
            }
        }
    }

    /// What the toasts said: a click brings Nea forward on the tab that
    /// sent it, in its space.
    pub(super) fn toast_outcomes(&mut self) {
        for outcome in notifications::take_outcomes() {
            match outcome {
                Outcome::Clicked(key) => {
                    let Some(shown) = self.notify.shown.remove(&key) else { continue };
                    notify_watch::report(&shown.notification, Report::Clicked);
                    notify_watch::report(&shown.notification, Report::Closed);
                    unsafe {
                        if IsIconic(self.hwnd).as_bool() {
                            let _ = ShowWindow(self.hwnd, SW_RESTORE);
                        }
                        let _ = SetForegroundWindow(self.hwnd);
                    }
                    if self.tabs.get(shown.tab).is_some() {
                        self.select_anywhere(shown.tab);
                    }
                }
                Outcome::Closed(key) => {
                    if let Some(shown) = self.notify.shown.remove(&key) {
                        notify_watch::report(&shown.notification, Report::Closed);
                    }
                }
            }
        }
    }

    /// The first view of the main profile: decisions the engine kept on
    /// its own (before Nea asked) come into the settings once.
    pub(super) fn notify_import(&mut self, webview: &ICoreWebView2) {
        if self.notify.imported {
            return;
        }
        self.notify.imported = true;
        notify_watch::read_profile(webview, |found| {
            with_app(|app| app.notify_imported(found));
        });
    }

    fn notify_imported(&mut self, found: Vec<(String, bool)>) {
        let mut changed = false;
        for (origin, allowed) in found {
            if let Some(origin) = notifications::origin_of(&origin)
                && !self.settings.notifications.contains_key(&origin)
            {
                self.settings.notifications.insert(origin, allowed);
                changed = true;
            }
        }
        if changed {
            self.save_settings();
            crate::settings_window::sync(&self.settings);
        }
    }

    /// Settings forgot these sites: the profile forgets them too, so they
    /// ask again.
    pub(super) fn notify_forget(&self, origins: &[String]) {
        let view = self.tabs.all().find_map(|t| match &t.live {
            Live::Ready(web) if !t.private => Some(web.webview.clone()),
            _ => None,
        });
        if let Some(view) = view {
            for origin in origins {
                notify_watch::forget_in_profile(&view, origin);
            }
        }
    }

    /// At exit: a toast left in the notification center would lead nowhere.
    pub(super) fn notify_shutdown(&mut self) {
        if let Some(Some(toasts)) = &self.notify.toasts {
            toasts.clear();
        }
        for (_, shown) in self.notify.shown.drain() {
            notify_watch::report(&shown.notification, Report::Closed);
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/notify_ui.rs"]
#[rustfmt::skip]
mod tests;
