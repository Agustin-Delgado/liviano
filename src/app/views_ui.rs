//! Which views are on screen: the active tab's (and its partner's, when
//! split) are shown, the others hidden.
//!
//! Each show or hide is a call to the engine's process. It is a few
//! milliseconds when the engine is idle, and hundreds when it is busy (many
//! views made or closed). So a tab switch tells the engine only what
//! changed: the views that are already hidden get no call, and with 200
//! tabs a switch is still one show and one hide. The hides wait for the next
//! frame, one in each tick: the page that comes on screen goes first.

use windows::Win32::{
    Foundation::{HWND, RECT},
    UI::WindowsAndMessaging::{KillTimer, SetTimer, USER_TIMER_MINIMUM},
};

use super::{App, with_app};
use crate::tabs::Live;

const TIMER_HIDE: usize = 0x4C;

/// What a tab's view is, as far as showing it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    /// No view: the tab sleeps.
    Asleep,
    /// The engine makes its view.
    Creating,
    /// A live view, and whether it is on screen now.
    Ready { shown: bool },
}

/// One call to make.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Change {
    Show(u64, RECT),
    Hide(u64),
    Wake(u64),
}

/// The calls that put `wanted` on screen (each tab with its place) and take
/// the other views off. A view already hidden gets no call. Linear in the
/// number of tabs.
pub(super) fn plan(tabs: &[(u64, View)], wanted: &[(u64, RECT)]) -> Vec<Change> {
    let mut shows = Vec::new();
    let mut hides = Vec::new();
    for &(id, view) in tabs {
        let place = wanted.iter().find(|(shown, _)| *shown == id).map(|(_, r)| *r);
        match (view, place) {
            (View::Ready { .. }, Some(r)) => shows.push(Change::Show(id, r)),
            (View::Ready { shown: true }, None) => hides.push(Change::Hide(id)),
            (View::Asleep, Some(_)) => shows.push(Change::Wake(id)),
            _ => {}
        }
    }
    // What comes on screen first.
    shows.extend(hides);
    shows
}

unsafe extern "system" fn on_hide_timer(_: HWND, _: u32, _: usize, _: u32) {
    let _doing = crate::diag::doing(&"timer:hide", 0);
    with_app(|app| app.hide_next());
}

#[derive(Default)]
pub(super) struct ViewsUi {
    /// Views to take off screen, one in each tick.
    hides: Vec<u64>,
}

impl App {
    /// Shows the active tab's view (and its partner's, when split), creating
    /// them if asleep, and hides the rest.
    pub(super) fn show_active(&mut self) {
        // A minimized window hides its pages (refer to `memory_ui`).
        let wanted = if self.memory.pages_hidden() { Vec::new() } else { self.shown_panes() };
        let views: Vec<(u64, View)> = self.tabs.tabs.iter().map(|t| (t.id, view_of(&t.live))).collect();
        let mut wake = Vec::new();
        for change in plan(&views, &wanted) {
            match change {
                Change::Show(id, bounds) => {
                    self.views.hides.retain(|h| *h != id);
                    if let Some(Live::Ready(web)) = self.tabs.get_mut(id).map(|t| &mut t.live) {
                        let _doing = crate::diag::doing(&"view:show", id);
                        web.show(bounds);
                    }
                }
                Change::Hide(id) => {
                    if !self.views.hides.contains(&id) {
                        self.views.hides.push(id);
                    }
                }
                Change::Wake(id) => wake.push(id),
            }
        }
        if !self.views.hides.is_empty() {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_HIDE, USER_TIMER_MINIMUM, Some(on_hide_timer));
            }
        }
        for id in wake {
            self.wake(id);
        }
        self.keyboard_off_hidden_views();
        self.update_title();
        self.autofill_follow();
        self.place_vault_bar();
        self.refresh_nav();
    }

    /// Takes the next view off screen, unless it came back on screen.
    fn hide_next(&mut self) {
        if !self.views.hides.is_empty() {
            let id = self.views.hides.remove(0);
            let wanted = !self.memory.pages_hidden() && self.visible_tabs().contains(&id);
            if !wanted && let Some(Live::Ready(web)) = self.tabs.get_mut(id).map(|t| &mut t.live) {
                let _doing = crate::diag::doing(&"view:hide", id);
                web.hide();
                self.keyboard_off_hidden_views();
            }
        }
        if self.views.hides.is_empty() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            }
        }
    }
}

fn view_of(live: &Live<crate::web::WebTab>) -> View {
    match live {
        Live::Asleep => View::Asleep,
        Live::Creating => View::Creating,
        Live::Ready(web) => View::Ready { shown: web.is_shown() },
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/views_ui.rs"]
#[rustfmt::skip]
mod tests;
