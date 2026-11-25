//! Which views are on screen: the active tab's (and its partner's, when
//! split) are shown, the others hidden.
//!
//! Each show or hide is a call to the engine's process. It is a few
//! milliseconds when the engine is idle, and hundreds when it is busy (many
//! views made or closed). So a tab switch tells the engine only what
//! changed: the views that are already hidden get no call, and with 200
//! tabs a switch is still one show and one hide. The page that comes on
//! screen goes first. The hide is not put off to a later tick: it then fell
//! inside the slide of the strip and stopped it for 50-100 ms.

use windows::Win32::Foundation::RECT;

use super::App;
use crate::tabs::Live;

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

impl App {
    /// Shows the active tab's view (and its partner's, when split), creating
    /// them if asleep, and hides the rest.
    pub(super) fn show_active(&mut self) {
        // A minimized window hides its pages (refer to `memory_ui`), and so
        // does a locked one (`lock_ui`): the pages keep running.
        let wanted = if self.memory.pages_hidden() || self.locked() { Vec::new() } else { self.shown_panes() };
        // Every space: the page of the space just left is still on screen.
        let views: Vec<(u64, View)> = self.tabs.all().map(|t| (t.id, view_of(&t.live))).collect();
        let mut wake = Vec::new();
        for change in plan(&views, &wanted) {
            match change {
                Change::Show(id, bounds) => {
                    if let Some(Live::Ready(web)) = self.tabs.get_mut(id).map(|t| &mut t.live) {
                        let _doing = crate::diag::doing(&"view:show", id);
                        web.show(bounds);
                    }
                }
                Change::Hide(id) => {
                    if let Some(Live::Ready(web)) = self.tabs.get_mut(id).map(|t| &mut t.live) {
                        let _doing = crate::diag::doing(&"view:hide", id);
                        web.hide();
                    }
                }
                Change::Wake(id) => wake.push(id),
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
