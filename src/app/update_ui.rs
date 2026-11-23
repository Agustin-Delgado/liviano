//! The window's part of the update (refer to `updater`): the "Update" chip
//! that shows while a verified update waits, and the restart that a click
//! on it (or the menu item) starts.
//!
//! The restart is a normal close: `WM_CLOSE` saves the session and closes
//! the pages, and `main` then starts the executable again
//! (`updater::relaunch`). The new instance waits until this one is gone,
//! applies the update before its window exists, and restores the tabs.

use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    UI::WindowsAndMessaging::{PostMessageW, WM_APP, WM_CLOSE},
};

use super::App;
use crate::updater;

/// The updater thread posts this when a package becomes ready, or is not
/// ready any more.
pub(super) const WM_APP_UPDATE: u32 = WM_APP + 0x60;

/// The text of the chip's tooltip and of the menu item.
pub(super) fn restart_label(version: &str) -> String {
    format!("Restart to update to {version}")
}

impl App {
    /// The updater changed what waits: the chip shows or goes.
    pub(super) fn update_changed(&mut self) {
        let ready = updater::ready_version();
        if ready != self.update {
            self.update = ready;
            self.relayout_soft();
        }
    }

    /// Closes the window as the person would (the session is saved), and
    /// starts the app again into the update.
    pub(super) fn restart_to_update(&mut self) {
        if self.update.is_none() {
            return;
        }
        updater::request_restart();
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}
