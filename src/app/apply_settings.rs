//! Settings, as the app receives them: each change from the settings window
//! is applied at once — theme, shortcuts, timers — and written to settings.json.

use super::{App, with_app};
use crate::{settings::Settings, settings_window};

fn on_settings_changed(settings: Settings) {
    with_app(|app| app.apply_settings(settings));
}

fn on_tab_layout_changed(vertical: bool, collapsed: bool) {
    with_app(|app| app.set_tab_layout(vertical, collapsed));
}

impl App {
    pub(super) fn open_settings(&mut self) {
        self.close_omnibar();
        settings_window::set_layout_handler(on_tab_layout_changed);
        settings_window::set_tab_layout(self.vertical, self.collapsed);
        settings_window::open(self.hwnd, &self.settings, &self.palette, on_settings_changed);
        self.attach_settings_extensions();
    }

    /// Search address, sleep times and kept-awake sites are read where they
    /// are used, so replacing the settings is enough for them; the theme and
    /// the shortcut table are rebuilt here.
    fn apply_settings(&mut self, settings: Settings) {
        let settings = crate::settings::from_window(&self.settings, settings);
        let theme_changed = settings.theme != self.settings.theme;
        let shortcuts_changed = settings.shortcuts != self.settings.shortcuts;
        let split_changed = settings.split_view != self.settings.split_view;
        let bench_changed = settings.bench_enabled != self.settings.bench_enabled;
        let trackers_changed = settings.block_trackers != self.settings.block_trackers;
        self.settings = settings;
        if trackers_changed {
            crate::shield::set_cosmetic(self.settings.block_trackers);
            for tab in &self.tabs.tabs {
                if let super::Live::Ready(web) = &tab.live {
                    web.apply_privacy(self.settings.block_trackers);
                }
            }
        }
        crate::updater::configure(&self.settings);
        if bench_changed {
            crate::bench::set_enabled(self.hwnd, crate::bench::wanted(self.settings.bench_enabled));
        }
        if shortcuts_changed {
            self.shortcuts.reload(&self.settings.shortcuts);
        }
        if theme_changed {
            self.refresh_theme();
        }
        self.save_settings();
        if split_changed {
            self.relayout();
        }
        self.invalidate();
    }
}
