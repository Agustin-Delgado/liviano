//! The bookmarks bar on screen: the copy of the list it draws, what a
//! click, a middle click, a drag and a right click on a bookmark do, the
//! overflow menu, renaming in the field and the command that shows or hides
//! the bar. The geometry and the drawing live in `bookmark_bar`.
//!
//! The list is small and the bar is on screen, so it stays in memory. Each
//! write of bookmarks.json gives the bar the new list (`bookmarks_changed`).

use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

use super::{App, Mode};
use crate::{
    bookmark_bar::{self, Entry},
    bookmarks::{Bookmark, Bookmarks},
    gfx,
    menu::Menu,
    shortcuts::Command,
    tabbar::{self, Hit, Rect},
};

const MENU_OPEN_NEW_TAB: usize = 80;
const MENU_OPEN_PRIVATE: usize = 81;
const MENU_RENAME: usize = 82;
const MENU_REMOVE: usize = 83;
const MENU_HIDE_BAR: usize = 84;
/// "»": the nth bookmark of the list.
const MENU_MORE: usize = 1000;
/// Bookmarks the overflow menu lists at most; the panel has the rest.
const MORE_LIMIT: usize = 60;
/// How far a press on a bookmark travels before it is a drag, not a click.
const DRAG_SLOP: i32 = 4;

/// A press on a bookmark: which one now (it moves with a drag), where the
/// press began and whether it has become a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Press {
    index: usize,
    x: i32,
    y: i32,
    dragging: bool,
}

#[derive(Default)]
pub(super) struct BarUi {
    entries: Vec<Entry>,
    /// The chips' widths for the first `bookmark_bar::MEASURED` entries, and
    /// the scale they were measured at. None: measure again.
    widths: Option<(f32, Vec<i32>)>,
    press: Option<Press>,
    /// The bookmark that the field renames.
    renaming: Option<String>,
    /// Ctrl was held at the last button release.
    ctrl: bool,
}

impl BarUi {
    pub(super) fn load() -> Self {
        let started = std::time::Instant::now();
        let mut bar = Self::default();
        bar.set(&Bookmarks::load().items);
        crate::diag::trace(|| {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            format!("startup: {} bookmarks read in {ms:.2} ms", bar.entries.len())
        });
        bar
    }

    fn set(&mut self, items: &[Bookmark]) {
        self.entries = items.iter().map(|b| Entry::new(&b.url, &b.title)).collect();
        self.widths = None;
    }

    /// A drag moves a bookmark from `from` to `to`; its measured width goes
    /// with it, so a drag measures nothing.
    fn move_entry(&mut self, from: usize, to: usize) {
        if from >= self.entries.len() || to >= self.entries.len() {
            return;
        }
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        match &mut self.widths {
            Some((_, w)) if from < w.len() && to < w.len() => {
                let width = w.remove(from);
                w.insert(to, width);
            }
            _ => self.widths = None,
        }
    }

    /// The chips' widths at `scale`, measured once per list and scale.
    fn widths(&mut self, scale: f32) -> &[i32] {
        if self.widths.as_ref().is_none_or(|(at, _)| *at != scale) {
            let font = bookmark_bar::chip_font(scale);
            let widths = self
                .entries
                .iter()
                .take(bookmark_bar::MEASURED)
                .map(|e| bookmark_bar::chip_width(gfx::measure(&e.label, font), scale))
                .collect();
            self.widths = Some((scale, widths));
        }
        self.widths.as_ref().map_or(&[], |(_, w)| w)
    }

    /// The widths `widths` measured at `scale`, without measuring.
    fn measured(&self, scale: f32) -> &[i32] {
        self.widths.as_ref().filter(|(at, _)| *at == scale).map_or(&[], |(_, w)| w)
    }
}

/// Where a bookmark opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    Here,
    Background,
    Private,
}

/// A click opens the bookmark in the current tab; with Ctrl, in a new tab
/// behind it (as a middle click).
fn click_target(ctrl: bool) -> Open {
    if ctrl { Open::Background } else { Open::Here }
}

impl App {
    /// The bar goes into the layout of now (called by `lay_out`).
    pub(super) fn place_bookmarks(&mut self) {
        if !self.vertical {
            self.bookmarks.widths(self.scale);
        }
        let mut layout = std::mem::take(&mut self.layout);
        self.place_bookmarks_in(&mut layout);
        self.layout = layout;
    }

    /// Places the bar in `layout` (also the other space's list during a
    /// swipe, so that its tabs sit where this space's do).
    pub(super) fn place_bookmarks_in(&self, layout: &mut tabbar::Layout) {
        let wanted = self.settings.show_bookmarks_bar && !self.collapsed;
        let count = if wanted { self.bookmarks.entries.len() } else { 0 };
        let widths: &[i32] = if count > 0 && !self.vertical { self.bookmarks.measured(self.scale) } else { &[] };
        bookmark_bar::place(layout, count, widths, self.scale);
    }

    /// What the bar draws this paint.
    pub(super) fn bookmark_entries(&self) -> &[Entry] {
        &self.bookmarks.entries
    }

    /// The bookmark held in a drag, drawn as held.
    pub(super) fn bookmark_held(&self) -> Option<usize> {
        self.bookmarks.press.filter(|p| p.dragging).map(|p| p.index)
    }

    /// bookmarks.json changed: the bar shows the new list. The page can move
    /// (the bar comes or goes), so the views are placed again.
    pub(super) fn bookmarks_changed(&mut self, items: &[Bookmark]) {
        self.bookmarks.set(items);
        self.bookmarks.press = None;
        self.relayout();
    }

    /// Reads bookmarks.json, changes it, writes it and gives the bar the
    /// result. The file, not the bar's copy, is the base: the panel can have
    /// written it meanwhile.
    fn edit_bookmarks(&mut self, change: impl FnOnce(&mut Bookmarks) -> bool) {
        let mut bookmarks = Bookmarks::load();
        if change(&mut bookmarks) && bookmarks.save().is_ok() {
            self.bookmarks_changed(&bookmarks.items);
        }
    }

    /// The command (and the menu item): the bar shows or hides, and the
    /// choice is saved.
    pub(super) fn toggle_bookmarks_bar(&mut self) {
        self.settings.show_bookmarks_bar = !self.settings.show_bookmarks_bar;
        self.save_settings();
        crate::settings_window::sync(&self.settings);
        self.relayout();
    }

    fn open_bookmark(&mut self, index: usize, how: Open) {
        let Some(entry) = self.bookmarks.entries.get(index) else { return };
        let (url, title) = (entry.url.clone(), entry.title.clone());
        self.close_omnibar();
        match how {
            Open::Here => self.navigate_active(&url),
            Open::Background => {
                // Asleep until it is chosen, as a restored tab: it costs
                // nothing until then.
                let at = self.tabs.after_active();
                self.tabs.insert(&url, &title, false, at);
                self.session_dirty = true;
                self.relayout_soft();
            }
            Open::Private => {
                let at = self.tabs.after_active();
                self.open_private_tab(&url, at);
            }
        }
    }

    // ---------- mouse ----------

    /// The keys of the button release that comes next (`WM_LBUTTONUP`).
    pub(super) fn bookmark_keys(&mut self, ctrl: bool) {
        self.bookmarks.ctrl = ctrl;
    }

    /// A press on a bookmark: a click opens it, a drag moves it.
    pub(super) fn bookmark_mouse_down(&mut self, index: usize, x: i32, y: i32) {
        self.close_omnibar();
        self.motion.press(tabbar::HoverKey::Bookmark(index));
        self.bookmarks.press = Some(Press { index, x, y, dragging: false });
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    /// While a bookmark is held: past a few pixels it follows the pointer
    /// along the bar. True while a press is held.
    pub(super) fn bookmark_mouse_move(&mut self, x: i32, y: i32) -> bool {
        let Some(press) = self.bookmarks.press else { return false };
        if !press.dragging {
            if (x - press.x).abs() < DRAG_SLOP && (y - press.y).abs() < DRAG_SLOP {
                return true;
            }
            self.hide_tip();
        }
        let to = self.layout.bookmarks.drop_index(x, y).unwrap_or(press.index);
        self.bookmarks.press = Some(Press { index: to, dragging: true, ..press });
        if to != press.index {
            self.bookmarks.move_entry(press.index, to);
            self.relayout_soft();
        } else if !press.dragging {
            self.invalidate_strip();
        }
        true
    }

    /// The press ends: a click opens the bookmark, a drag saves the new
    /// order. True when a press was held.
    pub(super) fn bookmark_mouse_up(&mut self, x: i32, y: i32) -> bool {
        let Some(press) = self.bookmarks.press.take() else { return false };
        unsafe {
            let _ = ReleaseCapture();
        }
        if press.dragging {
            self.save_bookmark_order();
            self.invalidate_strip();
        } else if self.layout.hit(x, y) == Some(Hit::Bookmark(press.index)) {
            self.open_bookmark(press.index, click_target(self.bookmarks.ctrl));
        }
        true
    }

    /// The drag ended (or was cut short): the file takes the order of the bar.
    fn save_bookmark_order(&mut self) {
        let order: Vec<String> = self.bookmarks.entries.iter().map(|e| e.url.clone()).collect();
        self.edit_bookmarks(|b| b.arrange(&order));
    }

    pub(super) fn bookmark_capture_lost(&mut self) {
        if self.bookmarks.press.take().is_some_and(|p| p.dragging) {
            self.save_bookmark_order();
        }
    }

    /// A middle click on a bookmark opens it in a new tab behind this one.
    pub(super) fn bookmark_middle_click(&mut self, x: i32, y: i32) -> bool {
        let Some(Hit::Bookmark(index)) = self.layout.hit(x, y) else { return false };
        self.open_bookmark(index, Open::Background);
        true
    }

    // ---------- menus ----------

    /// The right-click menu of a bookmark.
    pub(super) fn bookmark_menu(&mut self, x: i32, y: i32, index: usize) {
        if index >= self.bookmarks.entries.len() {
            return;
        }
        self.close_omnibar();
        let mut menu = Menu::new();
        menu.item(MENU_OPEN_NEW_TAB, "Open in new tab");
        menu.item(MENU_OPEN_PRIVATE, "Open in new private tab");
        menu.separator();
        menu.item(MENU_RENAME, "Rename…");
        menu.item(MENU_REMOVE, "Remove");
        menu.separator();
        menu.item(MENU_HIDE_BAR, &self.with_chord("Hide bookmarks bar", Command::ToggleBookmarksBar));
        let chosen = self.show_menu(menu, x, y);
        match chosen {
            MENU_OPEN_NEW_TAB => self.open_bookmark(index, Open::Background),
            MENU_OPEN_PRIVATE => self.open_bookmark(index, Open::Private),
            MENU_RENAME => self.start_bookmark_rename(index),
            MENU_REMOVE => {
                let url = self.bookmarks.entries[index].url.clone();
                self.edit_bookmarks(|b| b.remove(&url));
            }
            MENU_HIDE_BAR => self.toggle_bookmarks_bar(),
            _ => {}
        }
    }

    /// "»" (or the "…" tile): the bookmarks that do not fit. A choice opens
    /// in the current tab.
    pub(super) fn bookmarks_more_menu(&mut self) {
        let Some(more) = self.layout.bookmarks.more else { return };
        self.close_omnibar();
        self.motion.press(tabbar::HoverKey::BookmarksMore);
        let first = self.layout.bookmarks.items.len();
        let hidden: Vec<(usize, String)> = self
            .bookmarks
            .entries
            .iter()
            .enumerate()
            .skip(first)
            .take(MORE_LIMIT)
            .map(|(i, e)| (i, menu_text(&e.label)))
            .collect();
        let mut menu = Menu::new();
        for (i, label) in &hidden {
            menu.item(MENU_MORE + i, label);
        }
        // Under the button, as a menu of a toolbar opens.
        let (x, y) = if self.layout.bookmarks.tiles { (more.left, more.bottom) } else { (more.right, more.bottom) };
        let chosen = self.show_menu(menu, x, y);
        if chosen >= MENU_MORE {
            self.open_bookmark(chosen - MENU_MORE, Open::Here);
        }
    }

    // ---------- rename ----------

    fn start_bookmark_rename(&mut self, index: usize) {
        let Some(entry) = self.bookmarks.entries.get(index) else { return };
        self.bookmarks.renaming = Some(entry.url.clone());
        self.open_omnibar(Mode::RenameBookmark);
    }

    /// The field's cue and starting text while it renames a bookmark.
    pub(super) fn bookmark_rename_cue(&self) -> (&'static str, String) {
        let url = self.bookmarks.renaming.as_deref().unwrap_or("");
        let title = self.bookmarks.entries.iter().find(|e| e.url == url).map(|e| e.label.clone()).unwrap_or_default();
        ("Rename bookmark: type a name, then press Enter", title)
    }

    /// Enter in the field: the bookmark takes the name typed. An empty name
    /// changes nothing.
    pub(super) fn bookmark_rename_submit(&mut self) {
        let name = self.omnibar.text().trim().to_string();
        let url = self.bookmarks.renaming.take();
        self.close_omnibar();
        if let Some(url) = url.filter(|_| !name.is_empty()) {
            self.edit_bookmarks(|b| b.rename(&url, &name));
        }
    }

    // ---------- tooltips and the script control ----------

    pub(super) fn bookmark_tip(&self, hit: Hit) -> Option<String> {
        match hit {
            Hit::Bookmark(i) => self.bookmarks.entries.get(i).map(Entry::tip),
            Hit::BookmarksMore => Some("More bookmarks".to_string()),
            _ => None,
        }
    }

    pub(super) fn bookmark_tip_anchor(&self, hit: Hit) -> Option<Rect> {
        match hit {
            Hit::Bookmark(i) => self.layout.bookmarks.items.get(i).copied(),
            Hit::BookmarksMore => self.layout.bookmarks.more,
            _ => None,
        }
    }

    pub(super) fn bookmarks_bar_state(&self) -> serde_json::Value {
        let bar = &self.layout.bookmarks;
        let rect = |r: &Rect| [r.left, r.top, r.right, r.bottom];
        serde_json::json!({
            "setting": self.settings.show_bookmarks_bar,
            "shown": bar.shown(),
            "tiles": bar.tiles,
            "area": rect(&bar.area),
            "items": bar.items.iter().zip(&self.bookmarks.entries).map(|(r, e)| serde_json::json!({
                "url": e.url,
                "label": e.label,
                "rect": rect(r),
            })).collect::<Vec<_>>(),
            "more": bar.more.as_ref().map(rect),
            "count": self.bookmarks.entries.len(),
        })
    }
}

/// A title as a menu item: cut to a length that a menu can show.
fn menu_text(label: &str) -> String {
    let short: String = label.chars().take(60).collect();
    if short.len() < label.len() { format!("{short}…") } else { short }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/bookmarks_ui.rs"]
#[rustfmt::skip]
mod tests;
