//! Tab groups on screen: what the strip is told about them, the menus of a
//! tab and of a label, the click that folds a group, the drag that carries
//! it along the strip, renaming in the single field and the commands. The
//! rules (contiguity, folding, persistence) live in `tab_groups`.

use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};

use super::{App, Mode, WebTab};
use crate::{
    gfx,
    menu::Menu,
    shortcuts::Command,
    tab_groups::{Drop, GroupColor},
    tabbar::{self, GroupInfo},
    tabs::Tab,
};

/// In a tab's menu.
const MENU_NEW_GROUP: usize = 60;
const MENU_LEAVE_GROUP: usize = 61;
/// "Add to group ▸": the nth group listed.
const MENU_JOIN_GROUP: usize = 100;
/// In a label's menu.
const MENU_RENAME: usize = 70;
const MENU_NEW_TAB_IN: usize = 71;
const MENU_FOLD: usize = 72;
const MENU_UNGROUP: usize = 73;
const MENU_CLOSE_GROUP: usize = 74;
/// "Color ▸": the nth colour of `GroupColor::ALL`.
const MENU_COLOR: usize = 200;

/// How far a press on a label travels before it is a drag, not a click.
const DRAG_SLOP: i32 = 4;

#[derive(Default)]
pub(super) struct GroupsUi {
    /// A press on a label: the group, where it began, and whether it has
    /// become a drag.
    press: Option<(u64, i32, i32, bool)>,
    /// "New tab in group": the group the tab typed in the field joins.
    new_tab_in: Option<u64>,
}

/// The nine colours as a menu: a dot in each colour, and a check mark at
/// `current`. The nth colour of `GroupColor::ALL` has the id `first` + n.
pub(super) fn color_menu(first: usize, current: GroupColor, dark: bool) -> Menu {
    let mut menu = Menu::new();
    for (i, color) in GroupColor::ALL.iter().enumerate() {
        let fill = crate::design::group_tone(*color, dark).fill;
        menu.check(first + i, color.label(), *color == current).swatch(fill);
    }
    menu
}

impl App {
    /// The groups as the strip draws them, in strip order.
    pub(super) fn group_infos(&self) -> Vec<GroupInfo<'_>> {
        let tabs: Vec<&Tab<WebTab>> = self.tabs.tabs.iter().collect();
        self.group_infos_of(&tabs)
    }

    /// The groups of `tabs` (one space's, in strip order) as the strip
    /// draws them.
    pub(super) fn group_infos_of(&self, tabs: &[&Tab<WebTab>]) -> Vec<GroupInfo<'_>> {
        let font = tabbar::label_font(self.scale);
        let mut ids: Vec<u64> = Vec::new();
        for tab in tabs {
            if let Some(g) = tab.group.filter(|g| !ids.contains(g)) {
                ids.push(g);
            }
        }
        ids.into_iter()
            .filter_map(|id| self.tabs.group(id))
            .map(|g| {
                let text_w = if g.name.is_empty() { 0.0 } else { gfx::measure(&g.name, font) };
                GroupInfo {
                    id: g.id,
                    name: &g.name,
                    title: self.group_heading(g.id, &g.name),
                    color: g.color,
                    collapsed: g.collapsed,
                    count: tabs.iter().filter(|t| t.group == Some(g.id)).count(),
                    label_w: tabbar::label_width(text_w, self.scale),
                }
            })
            .collect()
    }

    /// What a group's header says in the column: its name or, unnamed, the
    /// site of its first tab, so two unnamed groups still read apart.
    fn group_heading(&self, group: u64, name: &str) -> String {
        if !name.trim().is_empty() {
            return name.to_string();
        }
        let first = self.tabs.members(group).first().and_then(|id| self.tabs.get(*id));
        let host = first.map(|t| crate::omnibox::display_host(&t.url)).unwrap_or("");
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.is_empty() { "Group".to_string() } else { host.to_string() }
    }

    /// After a group changed: the strip is laid out again and saved.
    fn groups_changed(&mut self) {
        self.session_dirty = true;
        self.relayout();
    }

    /// The live tab may have changed (a group folded over it): its page
    /// shows; with nothing left on show, the field opens for a new tab, as
    /// Chrome opens one.
    fn after_fold(&mut self, before: Option<u64>) {
        self.groups_changed();
        if self.tabs.active == before {
            return;
        }
        match self.tabs.active {
            Some(_) => self.focus_page(),
            None => self.open_omnibar(Mode::NewTab),
        }
    }

    pub(super) fn toggle_group(&mut self, group: u64) {
        let Some(folded) = self.tabs.group(group).map(|g| g.collapsed) else { return };
        let before = self.tabs.active;
        // No picture of the page is taken here, unlike a tab switch: a
        // folded tab has no card to show it on, and decoding the capture
        // on this thread landed in the middle of the fold.
        self.tabs.set_collapsed(group, !folded);
        self.after_fold(before);
    }

    /// A new group with `tab` in it; the field opens to name it (Escape
    /// leaves it unnamed).
    pub(super) fn group_new(&mut self, tab: u64) {
        let Some(group) = self.tabs.create_group(tab) else { return };
        self.groups_changed();
        self.open_rename(group);
    }

    pub(super) fn open_rename(&mut self, group: u64) {
        if self.tabs.group(group).is_some() {
            self.open_omnibar(Mode::RenameGroup(group));
        }
    }

    /// Enter in the field while renaming.
    pub(super) fn rename_submit(&mut self, group: u64) {
        let name = self.omnibar.text();
        self.close_omnibar();
        self.tabs.rename_group(group, &name);
        self.groups_changed();
    }

    pub(super) fn group_cue(&self, group: u64) -> (&'static str, String) {
        let name = self.tabs.group(group).map(|g| g.name.clone()).unwrap_or_default();
        ("Group name (Enter saves it; empty: color only)", name)
    }

    /// "New tab in group": the field opens; what is typed there
    /// opens at the end of the group, in it.
    fn new_tab_in(&mut self, group: u64) {
        self.open_omnibar(Mode::NewTab);
        self.groups.new_tab_in = Some(group);
    }

    /// Whatever opened the field before, the next tab joins no group.
    pub(super) fn forget_group_target(&mut self) {
        self.groups.new_tab_in = None;
    }

    /// For `go`: the group the tab about to open joins, if the field was
    /// opened for it.
    pub(super) fn take_group_target(&mut self) -> Option<u64> {
        self.groups.new_tab_in.take().filter(|g| self.tabs.group(*g).is_some())
    }

    pub(super) fn open_tab_in_group(&mut self, url: &str, group: u64) {
        let at = self.tabs.members(group).last().and_then(|last| self.tabs.index_of(*last)).map(|i| i + 1);
        let id = self.tabs.insert(url, "", false, at);
        self.tabs.add_to_group(id, group);
        self.tabs.select(id);
        self.relayout();
        self.focus_page();
        self.session_dirty = true;
    }

    fn close_group(&mut self, group: u64) {
        let members = self.tabs.members(group);
        self.close_tabs(&members);
    }

    /// The commands of the selector and the shortcuts, on the live tab.
    pub(super) fn group_command(&mut self, command: Command) {
        let Some(id) = self.tabs.active else { return };
        match command {
            Command::GroupTab => self.group_new(id),
            Command::UngroupTab => {
                if self.tabs.group_of(id).is_some() {
                    self.tabs.remove_from_group(id);
                    self.groups_changed();
                }
            }
            Command::ToggleGroup => {
                if let Some(group) = self.tabs.group_of(id) {
                    self.toggle_group(group);
                }
            }
            _ => {}
        }
    }

    // ---------- menus ----------

    /// The group part of a tab's menu. Nothing for a pinned tab.
    pub(super) fn append_group_items(&self, menu: &mut Menu, tab: u64) {
        let Some(t) = self.tabs.get(tab) else { return };
        if t.pinned {
            return;
        }
        let own = t.group;
        menu.separator();
        menu.item(MENU_NEW_GROUP, &self.with_chord("Add tab to new group", Command::GroupTab));
        let others: Vec<(usize, String)> = self
            .group_order()
            .into_iter()
            .enumerate()
            .filter(|(_, g)| Some(*g) != own)
            .filter_map(|(i, g)| self.tabs.group(g).map(|g| (i, g.title())))
            .collect();
        if !others.is_empty() {
            let mut sub = Menu::new();
            for (i, title) in others {
                sub.item(MENU_JOIN_GROUP + i, &title);
            }
            menu.submenu("Add to group", sub);
        }
        if own.is_some() {
            menu.item(MENU_LEAVE_GROUP, "Remove tab from group");
        }
    }

    /// The groups in strip order (what the menus number).
    fn group_order(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        for tab in &self.tabs.tabs {
            if let Some(g) = tab.group.filter(|g| !ids.contains(g)) {
                ids.push(g);
            }
        }
        ids
    }

    /// Runs a group item chosen in a tab's menu; false when `chosen` is not one.
    pub(super) fn group_menu_choice(&mut self, tab: u64, chosen: usize) -> bool {
        match chosen {
            MENU_NEW_GROUP => self.group_new(tab),
            MENU_LEAVE_GROUP => {
                self.tabs.remove_from_group(tab);
                self.groups_changed();
            }
            n if (MENU_JOIN_GROUP..MENU_JOIN_GROUP + 100).contains(&n) => {
                if let Some(&group) = self.group_order().get(n - MENU_JOIN_GROUP) {
                    self.tabs.add_to_group(tab, group);
                    self.groups_changed();
                }
            }
            _ => return false,
        }
        true
    }

    /// A right click on a label.
    pub(super) fn group_menu(&mut self, x: i32, y: i32, group: u64) {
        let Some(g) = self.tabs.group(group).cloned() else { return };
        let mut menu = Menu::new();
        menu.item(MENU_RENAME, "Rename group…");
        menu.submenu("Color", color_menu(MENU_COLOR, g.color, self.palette.dark));
        menu.item(MENU_NEW_TAB_IN, "New tab in group");
        menu.item(MENU_FOLD, if g.collapsed { "Expand group" } else { "Collapse group" });
        menu.separator();
        menu.item(MENU_UNGROUP, "Ungroup");
        menu.item(MENU_CLOSE_GROUP, "Close group");
        let chosen = self.show_menu(menu, x, y);
        match chosen {
            MENU_RENAME => self.open_rename(group),
            MENU_NEW_TAB_IN => self.new_tab_in(group),
            MENU_FOLD => self.toggle_group(group),
            MENU_UNGROUP => {
                self.tabs.ungroup(group);
                self.groups_changed();
            }
            MENU_CLOSE_GROUP => self.close_group(group),
            n if (MENU_COLOR..MENU_COLOR + GroupColor::ALL.len()).contains(&n) => {
                self.tabs.set_group_color(group, GroupColor::ALL[n - MENU_COLOR]);
                self.groups_changed();
            }
            _ => {}
        }
    }

    // ---------- mouse ----------

    /// A press on a label: a click folds the group, a drag carries it.
    pub(super) fn group_mouse_down(&mut self, group: u64, x: i32, y: i32) {
        self.close_omnibar();
        self.groups.press = Some((group, x, y, false));
        unsafe {
            SetCapture(self.hwnd);
        }
    }

    /// While a label is held: past a few pixels it is a drag, and the group
    /// goes, whole, where the pointer is.
    pub(super) fn group_mouse_move(&mut self, x: i32, y: i32) -> bool {
        let Some((group, x0, y0, dragging)) = self.groups.press else { return false };
        if !dragging {
            if (x - x0).abs() < DRAG_SLOP && (y - y0).abs() < DRAG_SLOP {
                return true;
            }
            self.groups.press = Some((group, x0, y0, true));
        }
        let Some(first) = self.tabs.members(group).first().and_then(|t| self.tabs.index_of(*t)) else { return true };
        let target = match self.layout.drop_at(x, y) {
            Some(Drop::Tab(i)) => self.tabs.tabs.get(i).map(|t| (t.id, i > first)),
            Some(Drop::BeforeGroup(other)) if other != group => {
                let anchor = self.tabs.members(other).first().copied();
                anchor.and_then(|a| self.tabs.index_of(a).map(|i| (a, i > first)))
            }
            Some(Drop::End) => self.tabs.tabs.last().map(|t| (t.id, true)),
            _ => None,
        };
        let Some((anchor, after)) = target else { return true };
        let before: Vec<u64> = self.tabs.tabs.iter().map(|t| t.id).collect();
        self.tabs.move_group(group, anchor, after);
        self.arrange_tabs(None);
        if self.tabs.tabs.iter().map(|t| t.id).ne(before) {
            self.session_dirty = true;
            self.relayout_soft();
        }
        true
    }

    /// The press ends: a click (no drag) folds or unfolds.
    pub(super) fn group_mouse_up(&mut self) -> bool {
        let Some((group, _, _, dragging)) = self.groups.press.take() else { return false };
        unsafe {
            let _ = ReleaseCapture();
        }
        if !dragging {
            self.toggle_group(group);
        }
        true
    }

    pub(super) fn group_capture_lost(&mut self) {
        self.groups.press = None;
    }

    /// The tooltip over a label: its name and how many tabs it holds.
    pub(super) fn group_tip(&self, group: u64) -> Option<String> {
        let g = self.tabs.group(group)?;
        let count = self.tabs.members(group).len();
        let tabs = if count == 1 { "1 tab".to_string() } else { format!("{count} tabs") };
        let action = if g.collapsed { "click to expand" } else { "click to collapse" };
        Some(format!("{} · {tabs} · {action}", g.title()))
    }

    pub(super) fn group_tip_anchor(&self, group: u64) -> Option<tabbar::Rect> {
        self.layout.labels.iter().find(|l| l.id == group).map(|l| l.rect)
    }
}
