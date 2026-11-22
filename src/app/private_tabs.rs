//! Private tabs: InPrivate views on a profile of their own, created
//! from the one shared environment. Nothing about them reaches the history,
//! the session, the reopen list or the suggestions; their cookies and cache
//! live in memory and go away with the last private tab.

use std::{cell::Cell, rc::Rc};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::Foundation::HWND,
    core::{HSTRING, Interface, Result},
};

use super::App;
use crate::{
    session::SavedTab,
    tabs::TabList,
    web::{self, WebTab},
};

/// Kept apart from the default profile, so an InPrivate session never shares
/// the off-the-record state of anything else the engine might create.
// The profile folder is in the user data: keep its name.
const PROFILE_NAME: &str = "Privado";

/// Same as `web::create_controller`, with the controller options that make
/// the view InPrivate.
pub(super) fn create_controller(env: &ICoreWebView2Environment, parent: HWND, id: u64) -> Result<()> {
    let env10 = env.cast::<ICoreWebView2Environment10>()?;
    let handler = web::controller_handler(id);
    unsafe {
        let options = env10.CreateCoreWebView2ControllerOptions()?;
        options.SetProfileName(&HSTRING::from(PROFILE_NAME))?;
        options.SetIsInPrivateModeEnabled(true)?;
        env10.CreateCoreWebView2ControllerWithOptions(parent, &options, &handler)
    }
}

/// Closes the last private view after wiping its profile. The engine drops
/// an InPrivate profile with its last view, but that teardown is
/// asynchronous: a private tab opened right after could still find the old
/// cookies.
fn close_last(mut web: WebTab) {
    web.hide();
    let profile = unsafe {
        web.webview.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile2>())
    };
    let Ok(profile) = profile else {
        web.close();
        return;
    };
    let slot = Rc::new(Cell::new(Some(web)));
    let pending = slot.clone();
    let handler = ClearBrowsingDataCompletedHandler::create(Box::new(move |_| {
        if let Some(web) = pending.take() {
            web.close();
        }
        Ok(())
    }));
    if unsafe { profile.ClearBrowsingDataAll(&handler) }.is_err()
        && let Some(web) = slot.take()
    {
        web.close();
    }
}

/// Whether session.json keeps this tab. The split pairs are saved by
/// position among these same tabs, so both go through this one rule.
pub(super) fn kept_in_session<W>(t: &crate::tabs::Tab<W>) -> bool {
    !t.private && !t.test && !t.url.is_empty()
}

/// The ids of the tabs session.json keeps, in strip order.
pub(super) fn kept_ids<W>(list: &TabList<W>) -> Vec<u64> {
    list.tabs.iter().filter(|t| kept_in_session(t)).map(|t| t.id).collect()
}

/// The tabs a session keeps, and the position of the active one among them.
/// When the active tab is not kept (private, or still empty), the next kept
/// tab takes its place. Each tab's group is a position in `saved_groups`.
pub(super) fn saved_tabs<W>(list: &TabList<W>) -> (Vec<SavedTab>, usize) {
    let kept = kept_in_session::<W>;
    let groups = list.saved_group_ids(kept);
    let tabs: Vec<SavedTab> = list
        .tabs
        .iter()
        .filter(|t| kept(t))
        .map(|t| SavedTab {
            url: t.url.clone(),
            title: t.title.clone(),
            pinned: t.pinned,
            group: t.group.and_then(|g| groups.iter().position(|s| *s == g)),
        })
        .collect();
    let before = list.active_index().map_or(0, |i| list.tabs[..i].iter().filter(|t| kept(t)).count());
    let active = before.min(tabs.len().saturating_sub(1));
    (tabs, active)
}

/// The groups a session keeps: those with a kept tab. A group of private
/// tabs only is not written, and neither are its private tabs.
pub(super) fn saved_groups<W>(list: &TabList<W>) -> Vec<crate::tab_groups::SavedGroup> {
    list.saved_groups(kept_in_session::<W>)
}

/// The window title for the active tab (`label`, and whether it is private).
pub(super) fn window_title(active: Option<(&str, bool)>) -> String {
    match active {
        Some((label, true)) => format!("{label} — {} (private tab)", crate::storage::DISPLAY_NAME),
        Some((label, false)) => format!("{label} — {}", crate::storage::DISPLAY_NAME),
        None => crate::storage::DISPLAY_NAME.to_string(),
    }
}

impl App {
    pub(super) fn open_private_tab(&mut self, url: &str, at: Option<usize>) {
        let id = self.tabs.insert_private(url, at);
        self.tabs.select(id);
        self.relayout();
        self.focus_page();
    }

    /// Closes the view of a private tab already taken out of the list; the
    /// last one takes the profile's data with it.
    pub(super) fn close_private_view(&self, web: WebTab) {
        if self.tabs.has_private() { web.close() } else { close_last(web) }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/private_tabs.rs"]
#[rustfmt::skip]
mod tests;
