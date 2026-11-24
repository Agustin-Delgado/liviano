//! Spaces on screen: the switcher in the strip (drawn, hit and animated
//! here), the switch itself, the menus of a space and of a tab, renaming
//! in the single field and the commands. The rules (what a switch moves,
//! what a delete keeps) live in `spaces`.
//!
//! A switch is a tab switch to another set of tabs: the model moves the
//! tabs between two lists, the strip fades the new tabs in while the live
//! card slides, and the engine gets one show and one hide (`views_ui`).

use std::time::Instant;

use windows::{
    Win32::{Foundation::POINT, UI::WindowsAndMessaging::*},
    core::{PCWSTR, w},
};

use super::{App, Mode, wide};
use crate::{
    anim::{Tween, specs},
    design::{group_tone, size},
    gfx::{self, Align, Color, Font, Painter, RectF},
    shortcuts::Command,
    spaces::{CHIP_DOT, CHIP_PAD, DOT_GAP, MAX_SPACES, SpaceInfo},
    tab_groups::GroupColor,
    tabbar::{self, HoverKey, Rect, Sheet, SheetKey},
};

/// In a space's menu.
const MENU_RENAME: usize = 300;
const MENU_DELETE: usize = 301;
const MENU_NEW: usize = 302;
/// "Color ▸": the nth colour of `GroupColor::ALL`.
const MENU_COLOR: usize = 310;
/// In a tab's menu: "Move to space ▸ New space".
pub(super) const MENU_MOVE_NEW: usize = 320;
/// "Move to space ▸": the nth space of the switcher.
const MENU_MOVE_TO: usize = 330;

/// The sheet key of the active space's ground: no tab or group gets it
/// (the + button has `u64::MAX`).
const GROUND_KEY: u64 = u64::MAX - 1;

#[derive(Default)]
pub(super) struct SpacesUi {
    /// The active space's ground sliding from the chip it left.
    slide: Option<(RectF, Tween<f32>)>,
    /// Where the ground was drawn last, and for which space.
    drawn: Option<(u64, RectF)>,
}

impl App {
    /// The switcher's spaces; none with one space, which needs no switcher.
    pub(super) fn space_infos(&self) -> Vec<SpaceInfo<'_>> {
        if self.tabs.spaces.len() < 2 {
            return Vec::new();
        }
        let font = tabbar::label_font(self.scale);
        self.tabs
            .spaces
            .iter()
            .map(|s| SpaceInfo {
                id: s.id,
                name: &s.name,
                color: s.color,
                active: s.id == self.tabs.space,
                name_w: gfx::measure(&s.name, font).ceil() as i32,
            })
            .collect()
    }

    /// Goes to the space `id`: its tabs come on the strip and its live
    /// page on screen. An empty space opens the field for a new tab.
    pub(super) fn switch_space(&mut self, id: u64) {
        if id == self.tabs.space || self.tabs.space_by_id(id).is_none() {
            return;
        }
        if self.mode.is_some() {
            self.close_omnibar();
        }
        if let Some(leaving) = self.tabs.active {
            self.capture_thumb(leaving);
            if let Some(crate::tabs::Live::Ready(web)) = self.tabs.get(leaving).map(|t| &t.live) {
                web.read_scroll(leaving);
            }
            self.fullscreen_tab_leaves(leaving);
        }
        self.hide_card();
        self.tabs.switch_space(id);
        self.motion.swap_tabs();
        self.relayout();
        self.session_dirty = true;
        match self.tabs.active {
            Some(tab) => self.focus_when_settled(tab),
            None => self.open_omnibar(Mode::NewTab),
        }
    }

    /// Selects a tab of any space (Ctrl+K): the space comes first.
    pub(super) fn select_anywhere(&mut self, tab: u64) {
        if let Some(space) = self.tabs.space_of(tab).filter(|s| *s != self.tabs.space) {
            self.switch_space(space);
        }
        self.select(tab);
    }

    /// A new space at the end of the switcher, made the active one; the
    /// field opens to name it.
    pub(super) fn new_space(&mut self) {
        let Some(id) = self.tabs.add_space() else { return };
        self.switch_space(id);
        self.open_omnibar(Mode::RenameSpace(id));
    }

    pub(super) fn space_command(&mut self, command: Command) {
        let target = match command {
            Command::GoToSpace(n) => self.tabs.space_by_number(n),
            Command::NextSpace => self.tabs.space_neighbour(1),
            Command::PrevSpace => self.tabs.space_neighbour(-1),
            Command::NewSpace => {
                self.new_space();
                return;
            }
            _ => None,
        };
        if let Some(id) = target {
            self.switch_space(id);
        }
    }

    pub(super) fn space_cue(&self, space: u64) -> (&'static str, String) {
        let name = self.tabs.space_by_id(space).map(|s| s.name.clone()).unwrap_or_default();
        ("Space name (Enter saves it)", name)
    }

    /// Enter in the field while renaming. A new space is still empty: the
    /// field then opens again for its first tab.
    pub(super) fn space_rename_submit(&mut self, space: u64) {
        let name = self.omnibar.text();
        self.close_omnibar();
        if self.tabs.rename_space(space, &name) {
            self.session_dirty = true;
            self.relayout();
        }
        if space == self.tabs.space && self.tabs.tabs.is_empty() {
            self.open_omnibar(Mode::NewTab);
        }
    }

    fn delete_space(&mut self, space: u64) {
        let active = space == self.tabs.space;
        if active {
            self.motion.swap_tabs();
        }
        if self.tabs.delete_space(space).is_some() {
            self.session_dirty = true;
            self.relayout();
            if active && let Some(tab) = self.tabs.active {
                self.focus_when_settled(tab);
            }
        }
    }

    /// Moves a tab, and its split partner with it (a pair is in one
    /// space), in the model only.
    fn move_pair(&mut self, tab: u64, space: u64) -> bool {
        let partner = self.split.splits.partner(tab);
        let moved = self.tabs.move_to_space(tab, space);
        if let Some(partner) = partner {
            self.tabs.move_to_space(partner, space);
        }
        moved
    }

    fn move_tab_to_space(&mut self, tab: u64, space: u64) {
        let was_live = self.tabs.active;
        if self.move_pair(tab, space) {
            self.session_dirty = true;
            self.relayout();
            match self.tabs.active {
                None => self.open_omnibar(Mode::NewTab),
                Some(live) if Some(live) != was_live => self.focus_when_settled(live),
                Some(_) => {}
            }
        }
    }

    // ---------- menus ----------

    /// "Move to space ▸" in a tab's menu: the other spaces and "New space".
    pub(super) fn append_space_items(&self, menu: HMENU, tab: u64) {
        let Some(own) = self.tabs.space_of(tab) else { return };
        unsafe {
            let Ok(sub) = CreatePopupMenu() else { return };
            for (i, space) in self.tabs.spaces.iter().enumerate().filter(|(_, s)| s.id != own) {
                let _ = AppendMenuW(sub, MF_STRING, MENU_MOVE_TO + i, &wide(&space.name));
            }
            if self.tabs.spaces.len() > 1 {
                let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
            }
            let full = if self.tabs.spaces.len() >= MAX_SPACES { MF_GRAYED } else { MF_ENABLED };
            let _ = AppendMenuW(sub, MF_STRING | full, MENU_MOVE_NEW, w!("New space"));
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Move to space"));
        }
    }

    /// Runs a space item chosen in a tab's menu; false when `chosen` is not one.
    pub(super) fn space_menu_choice(&mut self, tab: u64, chosen: usize) -> bool {
        match chosen {
            MENU_MOVE_NEW => {
                let Some(space) = self.tabs.add_space() else { return true };
                // The new space is where the tab went: it is named there.
                self.move_pair(tab, space);
                self.switch_space(space);
                self.open_omnibar(Mode::RenameSpace(space));
            }
            n if (MENU_MOVE_TO..MENU_MOVE_TO + MAX_SPACES).contains(&n) => {
                if let Some(space) = self.tabs.spaces.get(n - MENU_MOVE_TO).map(|s| s.id) {
                    self.move_tab_to_space(tab, space);
                }
            }
            _ => return false,
        }
        true
    }

    /// A right click on a space in the switcher.
    pub(super) fn space_menu(&mut self, x: i32, y: i32, space: u64) {
        let Some(s) = self.tabs.space_by_id(space).cloned() else { return };
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, MENU_RENAME, w!("Rename space…"));
            if let Ok(colors) = CreatePopupMenu() {
                for (i, color) in GroupColor::ALL.iter().enumerate() {
                    let check = if *color == s.color { MF_CHECKED } else { MF_UNCHECKED };
                    let _ = AppendMenuW(colors, MF_STRING | check, MENU_COLOR + i, &wide(color.label()));
                }
                let _ = AppendMenuW(menu, MF_POPUP, colors.0 as usize, w!("Color"));
            }
            let full = if self.tabs.spaces.len() >= MAX_SPACES { MF_GRAYED } else { MF_ENABLED };
            let _ = AppendMenuW(menu, MF_STRING | full, MENU_NEW, w!("New space"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            // Its tabs are kept: they go to the space before it.
            let _ = AppendMenuW(menu, MF_STRING, MENU_DELETE, w!("Delete space (keep its tabs)"));
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
        match chosen {
            MENU_RENAME => self.open_omnibar(Mode::RenameSpace(space)),
            MENU_NEW => self.new_space(),
            MENU_DELETE => self.delete_space(space),
            n if (MENU_COLOR..MENU_COLOR + GroupColor::ALL.len()).contains(&n) => {
                self.tabs.set_space_color(space, GroupColor::ALL[n - MENU_COLOR]);
                self.session_dirty = true;
                self.invalidate_strip();
            }
            _ => {}
        }
    }

    // ---------- the switcher on screen ----------

    /// The tooltip over a space: its name and its shortcut.
    pub(super) fn space_tip(&self, space: u64) -> Option<String> {
        let index = self.tabs.space_index(space)?;
        let name = &self.tabs.spaces[index].name;
        let chord = self.chord_hint(Command::GoToSpace(index as u8 + 1));
        Some(if chord.is_empty() { name.clone() } else { format!("{name} ({chord})") })
    }

    pub(super) fn space_tip_anchor(&self, space: u64) -> Option<Rect> {
        self.layout.spaces.iter().find(|c| c.id == space).map(|c| c.rect)
    }

    /// A press on a space: it becomes the active one.
    pub(super) fn space_mouse_down(&mut self, space: u64) {
        self.motion.press(HoverKey::Space(space));
        self.switch_space(space);
        self.invalidate_strip();
    }

    /// Brings the active ground's slide up to `now`; Some(frame interval)
    /// while it moves.
    pub(super) fn spaces_advance(&mut self, now: Instant) -> Option<u32> {
        let target = self.layout.spaces.iter().find(|c| c.id == self.tabs.space).map(|c| RectF::from(c.rect));
        let Some(target) = target else {
            self.spaces.slide = None;
            self.spaces.drawn = None;
            return None;
        };
        match self.spaces.drawn {
            Some((space, from)) if space != self.tabs.space => {
                self.spaces.slide = Some((from, Tween::new(0.0, 1.0, now, specs::LIVE_CARD)));
            }
            _ => {}
        }
        if self.spaces.slide.as_ref().is_some_and(|(_, t)| !t.moving(now)) {
            self.spaces.slide = None;
        }
        let ground = self.space_ground(target, now);
        self.spaces.drawn = Some((self.tabs.space, ground));
        self.spaces.slide.is_some().then_some(crate::anim::FRAME_MS)
    }

    /// Where the active space's ground is drawn at `now`, heading to `to`.
    fn space_ground(&self, to: RectF, now: Instant) -> RectF {
        match &self.spaces.slide {
            Some((from, t)) => from.lerp(&to, t.value(now)),
            None => to,
        }
    }

    /// The switcher's pieces for `changed_area`: each chip with its hover
    /// and press, and the sliding ground.
    pub(super) fn spaces_sheet(&self, sheet: &mut Sheet, now: Instant) {
        let margin = (2.0 * self.scale).ceil();
        let grow = |r: RectF| RectF::new(r.left - margin, r.top - margin, r.right + margin, r.bottom + margin);
        for chip in &self.layout.spaces {
            let key = HoverKey::Space(chip.id);
            let look = [self.motion.hover(key), self.motion.pressed(key), f32::from(u8::from(chip.named)), 0.0];
            sheet.insert(SheetKey::Hover(key), tabbar::Piece { bounds: grow(chip.rect.into()), look });
        }
        if let Some(chip) = self.layout.spaces.iter().find(|c| c.id == self.tabs.space) {
            let ground = self.space_ground(chip.rect.into(), now);
            let look = [ground.left, ground.right, 0.0, 0.0];
            sheet.insert(SheetKey::Item(GROUND_KEY), tabbar::Piece { bounds: grow(ground), look });
        }
    }

    /// Draws the switcher, over the strip that `tabbar::paint` drew.
    pub(super) fn paint_spaces(&self, g: &Painter, now: Instant) {
        if self.layout.spaces.is_empty() {
            return;
        }
        let pal = &self.palette;
        let px = |v: f32| v * self.scale;
        let radius = px(size::RADIUS_CONTROL);
        let font = tabbar::label_font(self.scale);
        let font = Font::new(font.size, font.weight, Align::Leading);
        // The column coming back slides in with everything in it.
        let dx =
            if self.layout.vertical { -(1.0 - self.motion.rail()) * self.layout.strip.width() as f32 } else { 0.0 };
        g.offset(dx, 0.0, |g| {
            if let Some(chip) = self.layout.spaces.iter().find(|c| c.id == self.tabs.space) {
                let ground = self.space_ground(chip.rect.into(), now);
                let tint = if pal.dark { 0.2 } else { 0.12 };
                g.round(ground, radius, Color::from(pal.accent).with_alpha(tint));
            }
            for chip in &self.layout.spaces {
                let Some(space) = self.tabs.space_by_id(chip.id) else { continue };
                let rect = RectF::from(chip.rect);
                let key = HoverKey::Space(chip.id);
                let active = chip.id == self.tabs.space;
                let over = self.motion.hover(key);
                let pressed = self.motion.pressed(key);
                if !active && over + pressed > 0.0 {
                    let ground = pal.tab_hover.mix(pal.pin_live, pressed);
                    g.round(rect, radius, Color::from(ground).with_alpha(over.max(pressed)));
                }
                let dot = px(CHIP_DOT);
                let (cx, cy) = rect.center();
                let dot_x = if chip.named { rect.left + px(CHIP_PAD as f32) + dot / 2.0 } else { cx };
                let tone = group_tone(space.color, pal.dark);
                g.round(
                    RectF::new(dot_x - dot / 2.0, cy - dot / 2.0, dot_x + dot / 2.0, cy + dot / 2.0),
                    dot / 2.0,
                    tone.fill,
                );
                if chip.named {
                    let ink = if active { pal.accent } else { pal.ink_muted.mix(pal.ink, over) };
                    let left = dot_x + dot / 2.0 + px(DOT_GAP as f32);
                    let text = RectF::new(left, rect.top, rect.right - px(CHIP_PAD as f32) + px(1.0), rect.bottom);
                    g.text(&space.name, text, font, ink);
                }
            }
        });
    }

    /// The `spaces` key of the script control's state.
    pub(super) fn spaces_state(&self) -> serde_json::Value {
        let spaces: Vec<serde_json::Value> = self
            .tabs
            .spaces
            .iter()
            .map(|s| {
                let chip = self.layout.spaces.iter().find(|c| c.id == s.id);
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "color": s.color.name(),
                    "active": s.id == self.tabs.space,
                    "tabs": self.tabs.space_tabs(s.id),
                    "last_active": if s.id == self.tabs.space { self.tabs.active } else { s.last_active },
                    "chip": chip.map(|c| [c.rect.left, c.rect.top, c.rect.right, c.rect.bottom]),
                    "named": chip.map(|c| c.named),
                })
            })
            .collect();
        serde_json::Value::Array(spaces)
    }
}
