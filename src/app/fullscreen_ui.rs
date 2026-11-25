//! Full screen: the window covers its screen, with no frame and no strip,
//! and the page fills it. F11 turns it on and off. A page that asks for it
//! (the HTML5 Fullscreen API, a video's full screen button) turns it on
//! too, and it ends when that page leaves its full screen element, when
//! another tab comes in front, or when its tab closes.
//!
//! Before, the engine only told the page that it was in full screen: the
//! video filled the page's own rectangle, under the strip.

use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2;
use windows::Win32::{
    Foundation::RECT,
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongW, GetWindowPlacement, HWND_TOP, IsIconic, SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongW,
        SetWindowPlacement, SetWindowPos, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED, WS_OVERLAPPEDWINDOW,
    },
};

use super::App;
use crate::{
    design::dwm,
    tabbar::{Layout, Rect},
    tabs::Live,
};

/// Makes the page leave its full screen element, when it has one.
const EXIT_PAGE_FULL_SCREEN: &str = "document.fullscreenElement && document.exitFullscreen()";

#[derive(Default)]
pub(super) struct FullscreenUi {
    /// The window covers its screen: its style and place from before.
    saved: Option<(i32, WINDOWPLACEMENT)>,
    /// The tab whose page asked for full screen. None: F11 asked for it.
    by_page: Option<u64>,
}

impl FullscreenUi {
    pub(super) fn on(&self) -> bool {
        self.saved.is_some()
    }

    /// The page of a tab fills the window alone, also in split view.
    pub(super) fn by_page(&self) -> Option<u64> {
        self.by_page.filter(|_| self.on())
    }

    /// The window's place before full screen, for the session.
    pub(super) fn normal_placement(&self) -> Option<WINDOWPLACEMENT> {
        self.saved.map(|(_, placement)| placement)
    }
}

/// The layout in full screen: the page has all of the client area, and the
/// strip, its buttons and the window buttons have no area.
pub(super) fn fullscreen_layout(width: i32, height: i32, vertical: bool) -> Layout {
    Layout { content: Rect::new(0, 0, width.max(0), height.max(0)), vertical, ..Default::default() }
}

/// What a change of a page's full screen state does: turn the window's
/// full screen on, turn it off, or nothing.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PageChange {
    Enter,
    Leave,
    Nothing,
}

/// `wants`: the page of `tab` has a full screen element now. `on_screen`:
/// that tab is shown. `by_page`: the tab that asked for the full screen in
/// force, if a page asked (`on` without it: F11).
pub(super) fn page_change(tab: u64, wants: bool, on_screen: bool, on: bool, by_page: Option<u64>) -> PageChange {
    match (wants, on) {
        (true, false) if on_screen => PageChange::Enter,
        (false, true) if by_page == Some(tab) => PageChange::Leave,
        _ => PageChange::Nothing,
    }
}

/// How the window comes back from full screen: as it was before, but
/// still minimized when it is minimized now.
pub(super) fn placement_show(before: u32, minimized: bool) -> u32 {
    if minimized { SW_SHOWMINNOACTIVE.0 as u32 } else { before }
}

fn exit_page_full_screen(webview: &ICoreWebView2) {
    crate::inject::execute_without_gesture(webview, EXIT_PAGE_FULL_SCREEN);
}

impl App {
    /// F11.
    pub(super) fn toggle_fullscreen(&mut self) {
        if self.fullscreen.on() {
            self.leave_fullscreen();
        } else {
            self.enter_fullscreen(None);
        }
    }

    /// The page of `id` entered or left its full screen element.
    pub(super) fn page_fullscreen(&mut self, id: u64) {
        let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live) else { return };
        let mut wants = windows::core::BOOL(0);
        unsafe {
            let _ = web.webview.ContainsFullScreenElement(&mut wants);
        }
        let on_screen = self.visible_tabs().contains(&id);
        match page_change(id, wants.as_bool(), on_screen, self.fullscreen.on(), self.fullscreen.by_page) {
            PageChange::Enter => {
                if self.tabs.active != Some(id) {
                    self.tabs.select(id);
                }
                self.enter_fullscreen(Some(id));
            }
            PageChange::Leave => self.leave_fullscreen(),
            PageChange::Nothing => {}
        }
    }

    /// The tab that asked for full screen leaves the screen (another tab
    /// comes in front, or it closes): the full screen ends with it.
    pub(super) fn fullscreen_tab_leaves(&mut self, id: u64) {
        if self.fullscreen.by_page() == Some(id) {
            self.leave_fullscreen();
        }
    }

    fn enter_fullscreen(&mut self, by_page: Option<u64>) {
        // A minimized window has no screen to cover: it stays as it is. A
        // locked one shows no page to give the screen to.
        if self.fullscreen.on() || self.locked() || unsafe { IsIconic(self.hwnd).as_bool() } {
            return;
        }
        self.close_omnibar();
        self.hide_card();
        self.hide_tip();
        if by_page.is_some() {
            self.hide_space_name();
        }
        let hwnd = self.hwnd;
        unsafe {
            let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let mut monitor = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetWindowPlacement(hwnd, &mut placement).is_err()
                || !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor).as_bool()
            {
                return;
            }
            let style = GetWindowLongW(hwnd, GWL_STYLE);
            self.fullscreen.saved = Some((style, placement));
            self.fullscreen.by_page = by_page;
            SetWindowLongW(hwnd, GWL_STYLE, style & !(WS_OVERLAPPEDWINDOW.0 as i32));
            dwm::set(hwnd, dwm::CORNERS, dwm::DO_NOT_ROUND);
            let RECT { left, top, right, bottom } = monitor.rcMonitor;
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                left,
                top,
                right - left,
                bottom - top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
            );
        }
        self.relayout();
        self.focus_page();
    }

    pub(super) fn leave_fullscreen(&mut self) {
        let Some((style, mut placement)) = self.fullscreen.saved.take() else { return };
        let minimized = unsafe { IsIconic(self.hwnd).as_bool() };
        // A page can leave its full screen while the window is minimized
        // (its video ended): the window stays minimized.
        if minimized && placement.showCmd == SW_SHOWMAXIMIZED.0 as u32 {
            placement.flags |= WPF_RESTORETOMAXIMIZED;
        }
        placement.showCmd = placement_show(placement.showCmd, minimized);
        if let Some(id) = self.fullscreen.by_page.take()
            && let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live)
        {
            exit_page_full_screen(&web.webview);
        }
        let hwnd = self.hwnd;
        unsafe {
            SetWindowLongW(hwnd, GWL_STYLE, style);
            dwm::set(hwnd, dwm::CORNERS, dwm::ROUND);
            let _ = SetWindowPlacement(hwnd, &placement);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
            );
        }
        // Minimized, the layout stays as it is (refer to `memory_ui`).
        if !minimized {
            self.relayout();
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/fullscreen_ui.rs"]
#[rustfmt::skip]
mod tests;
