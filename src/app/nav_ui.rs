//! Back, forward and reload beside the window's buttons: whether each can
//! act (the live tab's `CanGoBack`/`CanGoForward`, kept current by the
//! engine's `HistoryChanged`), the click that runs it on release, and the
//! tab's history in a menu on a right click or a press held half a second,
//! as Safari and Chrome offer it.

use serde_json::Value;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::InvalidateRect,
    UI::{
        Input::KeyboardAndMouse::{ReleaseCapture, SetCapture},
        WindowsAndMessaging::*,
    },
};

use super::{App, wide, with_app};
use crate::{
    shortcuts::Command,
    tabbar::{Hit, HoverKey, NavButton, NavState, Rect},
    tabs::Live,
};

const TIMER_HOLD: usize = 0x4E41;
/// A press held this long on back or forward opens the history.
const HOLD_MS: u32 = 450;
/// Entries listed in the menu, nearest first.
const MENU_ENTRIES: usize = 15;
/// Menu ids: the nth entry listed.
const MENU_ENTRY: usize = 1;
/// Entry titles are cut to this many characters.
const TITLE_CHARS: usize = 60;

#[derive(Default)]
pub(super) struct NavUi {
    pub(super) state: NavState,
    /// Back or forward held down, waiting for release (a click) or the hold.
    press: Option<NavButton>,
    /// The history menu asked for: tab and whether it lists back.
    asked: Option<(u64, bool)>,
}

/// What a history menu lists, nearest first: (entry id, label). `json` is
/// DevTools' answer to `Page.getNavigationHistory`.
pub(super) fn history_entries(json: &str, back: bool) -> Vec<(i64, String)> {
    let Ok(value) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(entries) = value["entries"].as_array() else { return Vec::new() };
    let Some(current) = value["currentIndex"].as_u64().map(|i| i as usize) else { return Vec::new() };
    let label = |e: &Value| {
        let title = e["title"].as_str().unwrap_or("").trim();
        let text = if title.is_empty() { e["url"].as_str().unwrap_or("") } else { title };
        let mut cut: String = text.chars().take(TITLE_CHARS).collect();
        if text.chars().count() > TITLE_CHARS {
            cut.push('…');
        }
        // A lone '&' would underline the next letter in a Windows menu.
        cut.replace('&', "&&")
    };
    let picked: Vec<&Value> = if back {
        entries.iter().take(current.min(entries.len())).rev().collect()
    } else {
        entries.iter().skip(current.saturating_add(1)).collect()
    };
    picked.into_iter().take(MENU_ENTRIES).filter_map(|e| e["id"].as_i64().map(|id| (id, label(e)))).collect()
}

unsafe extern "system" fn on_hold(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| app.nav_held());
}

impl App {
    /// Back, forward and reload as the live tab allows them; repaints them
    /// when that changed.
    pub(super) fn refresh_nav(&mut self) {
        let state = match self.tabs.active_tab().map(|t| &t.live) {
            Some(Live::Ready(web)) => {
                let (back, forward) = web.can_go();
                NavState { back, forward, reload: true }
            }
            Some(_) => NavState { back: false, forward: false, reload: true },
            None => NavState::default(),
        };
        if state != self.nav.state {
            self.nav.state = state;
            self.invalidate_nav();
        }
    }

    fn invalidate_nav(&self) {
        for r in self.layout.nav.iter().filter(|r| r.width() > 0) {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&r.win()), false);
            }
        }
    }

    pub(super) fn nav_rect(&self, button: NavButton) -> Rect {
        self.layout.nav[NavButton::ALL.iter().position(|b| *b == button).unwrap_or(0)]
    }

    /// A press on one of them: it acts on release, so back and forward can
    /// be held for their history instead.
    pub(super) fn nav_mouse_down(&mut self, button: NavButton) {
        self.close_omnibar();
        if !self.nav.state.enabled(button) {
            return;
        }
        self.motion.press(HoverKey::Nav(button));
        self.nav.press = Some(button);
        unsafe {
            SetCapture(self.hwnd);
            if button != NavButton::Reload {
                SetTimer(Some(self.hwnd), TIMER_HOLD, HOLD_MS, Some(on_hold));
            }
        }
        self.invalidate_nav();
    }

    /// The press ends; true when it was one of ours.
    pub(super) fn nav_mouse_up(&mut self, x: i32, y: i32) -> bool {
        let Some(button) = self.nav.press.take() else { return false };
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOLD);
            let _ = ReleaseCapture();
        }
        if self.layout.hit(x, y) == Some(Hit::Nav(button)) {
            self.run(match button {
                NavButton::Back => Command::Back,
                NavButton::Forward => Command::Forward,
                NavButton::Reload => Command::Reload,
            });
        }
        true
    }

    pub(super) fn nav_capture_lost(&mut self) {
        if self.nav.press.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_HOLD);
            }
        }
    }

    /// Held long enough: the history instead of the click.
    fn nav_held(&mut self) {
        let Some(button) = self.nav.press.take() else { return };
        unsafe {
            let _ = ReleaseCapture();
        }
        self.ask_history(button);
    }

    /// Right click (or the hold) on back or forward: the tab's history.
    pub(super) fn ask_history(&mut self, button: NavButton) {
        let back = match button {
            NavButton::Back => true,
            NavButton::Forward => false,
            NavButton::Reload => return,
        };
        if !self.nav.state.enabled(button) {
            return;
        }
        self.hide_tip();
        let Some(tab) = self.tabs.active_tab() else { return };
        if let Live::Ready(web) = &tab.live {
            self.nav.asked = Some((tab.id, back));
            web.request_history(tab.id, back);
        }
    }

    /// The history arrived: the menu under the button, and the entry chosen.
    pub(super) fn on_nav_history(&mut self, tab: u64, back: bool, json: &str) {
        if self.nav.asked.take() != Some((tab, back)) || self.tabs.active != Some(tab) {
            return;
        }
        let entries = history_entries(json, back);
        if entries.is_empty() {
            return;
        }
        let anchor = self.screen_rect(self.nav_rect(if back { NavButton::Back } else { NavButton::Forward }));
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (i, (_, label)) in entries.iter().enumerate() {
                let _ = AppendMenuW(menu, MF_STRING, MENU_ENTRY + i, &wide(label));
            }
            // Its time is the person reading the menu.
            self.save_before_modal();
            let modal = crate::diag::modal();
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
                anchor.left,
                anchor.bottom,
                None,
                self.hwnd,
                None,
            )
            .0 as usize;
            drop(modal);
            let _ = DestroyMenu(menu);
            chosen
        };
        let Some((entry, _)) = chosen.checked_sub(MENU_ENTRY).and_then(|i| entries.get(i)) else { return };
        if let Some(Live::Ready(web)) = self.tabs.get(tab).map(|t| &t.live) {
            web.go_to_history_entry(*entry);
        }
    }

    /// The mouse's back and forward buttons over the strip (over the page
    /// the engine answers them itself).
    pub(super) fn x_button(&mut self, which: u16) {
        match which {
            1 => self.run(Command::Back),
            2 => self.run(Command::Forward),
            _ => {}
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/nav_ui.rs"]
#[rustfmt::skip]
mod tests;
