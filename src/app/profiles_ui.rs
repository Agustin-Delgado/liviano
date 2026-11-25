//! Profiles on screen: the round mark in the strip and its menu, the
//! profile's name in the window title, and the Profiles group of Settings.
//! All of it shows only with two profiles or more; with one, only the
//! Settings group does. The registry and the rules are in `profiles`, the
//! windows and processes in `profile_launch`.

use windows::{
    Win32::{Foundation::POINT, UI::WindowsAndMessaging::*},
    core::{PCWSTR, w},
};

use super::{App, wide, with_app};
use crate::{
    design::{Palette, Rgb, group_tone, size},
    gfx::{Align, Font, Painter, RectF},
    profile_launch,
    profiles::{self, Registry},
    settings_window::{self, ProfileCommand},
    tabbar::{self, HoverKey},
};

/// In the profile's menu: the nth profile of the list, to open it.
const MENU_OPEN: usize = 500;
const MENU_NEW: usize = 498;
const MENU_SETTINGS: usize = 499;
/// The list is short; past this many the menu stops.
const MAX_LISTED: usize = 100;
/// The round mark's size in the button, in DIPs.
const AVATAR: f32 = 18.0;

impl App {
    /// The strip shows the profile's button and the title its name.
    pub(super) fn profiles_shown(&self) -> bool {
        self.profiles.shown()
    }

    /// The app's name in the title: with the profile's when profiles show.
    pub(super) fn app_name(&self) -> String {
        let name = self.profiles_shown().then(|| self.profiles.get(profile_launch::current()).name);
        profiles::app_name(crate::storage::DISPLAY_NAME, name.as_deref())
    }

    /// Reads the registry again (another window may have changed it). What
    /// shows follows when it changed.
    pub(super) fn reload_profiles(&mut self) {
        let registry: Registry = profile_launch::load();
        if registry.list() == self.profiles.list() {
            self.profiles = registry;
            return;
        }
        self.profiles = registry;
        self.relayout();
        self.invalidate();
        self.update_title();
        self.sync_settings_profiles();
    }

    // ---------- the strip ----------

    /// The profile's button: its colour's disc with its initial, on the
    /// ghost ground of the gear and the space mark.
    pub(super) fn paint_profile(&self, g: &Painter, pal: &Palette) {
        let Some(rect) = self.layout.profile else { return };
        let profile = self.profiles.get(profile_launch::current());
        let draw = |g: &Painter| {
            let px = |v: f32| v * self.scale;
            let rect = RectF::from(rect);
            let over = self.motion.hover(HoverKey::Profile);
            let pressed = self.motion.pressed(HoverKey::Profile);
            tabbar::ghost_ground(g, rect, px(size::RADIUS), over, pressed, pal);
            let (cx, cy) = rect.center();
            let d = px(AVATAR);
            let disc = RectF::new(cx - d / 2.0, cy - d / 2.0, cx + d / 2.0, cy + d / 2.0);
            g.round(disc, d / 2.0, group_tone(profile.color, pal.dark).fill);
            // The dark palette's tones are light: dark ink reads on them.
            let ink = if pal.dark { Rgb::grey(0x17) } else { Rgb::grey(0xFF) };
            let mut buffer = [0u8; 4];
            let letter: &str = profiles::initial(&profile.name).encode_utf8(&mut buffer);
            g.text(letter, disc, Font::new(size::TEXT_MARK * self.scale, size::MEDIUM, Align::Center), ink);
        };
        if self.layout.vertical {
            // The column coming back slides in with everything in it.
            let rail = -(1.0 - self.motion.rail()) * self.layout.strip.width() as f32;
            g.offset(rail, 0.0, draw);
        } else {
            draw(g);
        }
    }

    pub(super) fn profile_tip(&self) -> Option<String> {
        Some("Profile".to_owned())
    }

    pub(super) fn profile_tip_anchor(&self) -> Option<tabbar::Rect> {
        self.layout.profile
    }

    /// A press on the button opens the profiles' menu.
    pub(super) fn profile_mouse_down(&mut self, x: i32, y: i32) {
        self.motion.press(HoverKey::Profile);
        self.invalidate_strip();
        self.profile_menu(x, y);
    }

    /// This profile (checked, not a choice), the others to open, then "New
    /// profile…" and "Profile settings".
    pub(super) fn profile_menu(&mut self, x: i32, y: i32) {
        self.reload_profiles();
        let list = self.profiles.list();
        let current = profile_launch::current();
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (i, p) in list.iter().enumerate().take(MAX_LISTED) {
                let state = if p.id == current { MF_CHECKED | MF_GRAYED } else { MF_UNCHECKED };
                let _ = AppendMenuW(menu, MF_STRING | state, MENU_OPEN + i, &wide(&p.name));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_NEW, w!("New profile…"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Profile settings"));
            let mut point = POINT { x, y };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut point);
            // Its time is the person reading the menu.
            self.save_before_modal();
            let modal = crate::diag::modal();
            let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, self.hwnd, None)
                .0 as usize;
            drop(modal);
            let _ = DestroyMenu(menu);
            chosen
        };
        // The menu held the mouse: the hover is wherever the pointer is now.
        self.rehover();
        match chosen {
            MENU_NEW | MENU_SETTINGS => self.open_settings(),
            n if (MENU_OPEN..MENU_OPEN + MAX_LISTED).contains(&n) => {
                if let Some(p) = list.get(n - MENU_OPEN).filter(|p| p.id != current) {
                    let _ = profile_launch::open(p.id);
                }
            }
            _ => {}
        }
    }

    // ---------- Settings ----------

    pub(super) fn attach_settings_profiles(&mut self) {
        settings_window::set_profile_handler(on_profile_command);
        self.sync_settings_profiles();
    }

    fn sync_settings_profiles(&self) {
        let rows: Vec<(u32, String)> = self.profiles.list().into_iter().map(|p| (p.id, p.name)).collect();
        settings_window::set_profiles(&rows, profile_launch::current());
    }

    fn profile_command(&mut self, command: ProfileCommand) {
        let result = match command {
            ProfileCommand::New { name, shortcut } => profile_launch::create(&name, shortcut)
                .map(|(_, shortcut)| shortcut.map(|why| format!("{name} is open. The desktop shortcut failed: {why}"))),
            ProfileCommand::Rename { id, name } => profile_launch::rename(id, &name).map(|()| None),
            ProfileCommand::Open(id) => profile_launch::open(id).map(|()| None),
        };
        match result {
            Ok(Some(note)) | Err(note) => settings_window::show_status(&note),
            Ok(None) => {}
        }
        self.reload_profiles();
    }
}

fn on_profile_command(command: ProfileCommand) {
    with_app(|app| app.profile_command(command));
}
