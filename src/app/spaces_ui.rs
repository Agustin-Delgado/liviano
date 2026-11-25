//! Spaces on screen: the swipe that slides the tab list from one space to
//! the next, the switch itself, the space's tint, its mark and its name,
//! the menus of a space and of a tab, renaming in the single field and the
//! commands. The rules (what a switch moves, what a delete keeps, how far
//! a swipe goes) live in `spaces`.
//!
//! A switch is a tab switch to another set of tabs: the model moves the
//! tabs between two lists, and the engine gets one show and one hide
//! (`views_ui`). On screen the whole list slides sideways: with the
//! fingers during a swipe (the neighbour's list drawn beside it, from the
//! model, with no page), then on its own to its place. Only the strip is
//! drawn during a swipe; the page changes once, when the swipe commits.

use std::time::Instant;

use windows::{
    Win32::{
        Foundation::{HWND, POINT},
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, w},
};

use super::{App, Mode, WebTab, wide, with_app};
use crate::{
    anim::{Tween, specs},
    design::{Palette, group_tone, size},
    gfx::{self, Align, Color, Font, Painter, RectF},
    shortcuts::Command,
    spaces::{MARK_DOT, MARK_GAP, MARK_PAD, MAX_SPACES, SWIPE_TRAVEL, Swipe, swipe_commits, swipe_offset},
    tab_groups::GroupColor,
    tabbar::{self, HoverKey, Metrics, Rect},
    tabs::Tab,
};

/// In a space's menu.
const MENU_RENAME: usize = 300;
const MENU_DELETE: usize = 301;
const MENU_NEW: usize = 302;
/// "Color ▸": the nth colour of `GroupColor::ALL`.
const MENU_COLOR: usize = 310;
/// In a tab's menu: "Move to space ▸ New space".
pub(super) const MENU_MOVE_NEW: usize = 320;
/// "Move to space ▸": the nth space.
const MENU_MOVE_TO: usize = 330;

/// The wheel is still this long: the fingers left the touchpad, and the
/// swipe settles.
const SWIPE_REST_MS: u32 = 120;
const TIMER_SWIPE: usize = 0x4C;

/// The active space's name stays over the list this long after a switch,
/// then fades out in `NAME_FADE_MS`.
const NAME_HOLD_MS: u32 = 800;
const NAME_FADE_MS: u32 = 250;

/// How much of the space's colour the strip's ground takes.
const TINT_LIGHT: f32 = 0.06;
const TINT_DARK: f32 = 0.09;

/// What is drawn beside the sliding list.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Beside {
    Space(u64),
    /// Past the last space: the new space that a commit makes.
    New,
    /// Past the first space (or no room for another): the list only
    /// stretches.
    Nothing,
}

struct Slide {
    /// Where the active space's list is, px sideways (negative: out to the
    /// left, the next space coming in from the right).
    offset: f32,
    /// The fingers on it; None once it settles.
    swipe: Option<Swipe>,
    /// Settling: the offset heading back to 0.
    settle: Option<Tween<f32>>,
    beside: Beside,
    /// The side the other list is on: -1 left, 1 right.
    side: f32,
}

#[derive(Default)]
pub(super) struct SpacesUi {
    slide: Option<Slide>,
    /// A space a swipe past the last one made, until it holds a tab: going
    /// away from it while it is still empty removes it.
    fresh: Option<u64>,
    /// Since when the active space's name rests over the list after a
    /// switch (it fades out); None when it is not shown.
    named: Option<Instant>,
    /// Shift+wheel: this burst of notches already switched.
    notched: bool,
    /// A list at rest, to draw the other space's tabs with: no hover, no
    /// motion.
    still: tabbar::Motion,
    /// The slide and the name's opacity in the last paint: a frame draws
    /// the strip again only when one changed.
    painted: (f32, f32),
}

unsafe extern "system" fn on_swipe_rest(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| app.swipe_rest());
}

impl App {
    /// The width of the active space's name in the mark at the foot of the
    /// column, when the column shows the mark (two spaces or more).
    pub(super) fn space_mark_width(&self) -> Option<i32> {
        if !self.vertical || self.tabs.spaces.len() < 2 {
            return None;
        }
        Some(gfx::measure(&self.tabs.active_space().name, mark_font(self.scale)).ceil() as i32)
    }

    /// The palette the chrome is drawn with: with two spaces or more, the
    /// ground of the strip takes a little of the active space's colour.
    pub(super) fn space_palette(&self) -> Palette {
        let mut pal = self.palette;
        if self.tabs.spaces.len() < 2 {
            return pal;
        }
        let tone = group_tone(self.tabs.active_space().color, pal.dark).fill;
        pal.chrome = pal.chrome.mix(tone, if pal.dark { TINT_DARK } else { TINT_LIGHT });
        pal
    }

    // ---------- switching ----------

    /// Goes to the space `id` with a short slide: the new list comes in
    /// from the side it is on (from the right for a later space).
    pub(super) fn switch_space(&mut self, id: u64) {
        let (Some(from), Some(to)) = (self.tabs.space_index(self.tabs.space), self.tabs.space_index(id)) else {
            return;
        };
        let side = if to > from { 1.0 } else { -1.0 };
        self.switch_space_from(id, side);
    }

    /// Goes to `id`, its list coming in from `side` (1: the right).
    fn switch_space_from(&mut self, id: u64, side: f32) {
        if id == self.tabs.space {
            return;
        }
        let leaving = self.tabs.space;
        let width = self.slide_width();
        if !self.swap_space(id) {
            return;
        }
        let now = Instant::now();
        let from = side * width;
        self.spaces.slide = Some(Slide {
            offset: from,
            swipe: None,
            settle: Some(Tween::new(from, 0.0, now, specs::SPACE_SLIDE)),
            beside: Beside::Space(leaving),
            side: -side,
        });
        self.spaces.named = None;
        self.invalidate_strip();
    }

    /// The switch in the model and on the page: the tabs change lists, the
    /// new space's live page shows (an empty space opens the field). The
    /// list lands at once; the caller slides it. A space a swipe made and
    /// left empty goes away.
    fn swap_space(&mut self, id: u64) -> bool {
        if id == self.tabs.space || self.tabs.space_by_id(id).is_none() {
            return false;
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
        self.hide_tip();
        let left = self.tabs.space;
        self.tabs.switch_space(id);
        if self.spaces.fresh == Some(left) {
            self.spaces.fresh = None;
            if self.tabs.space_tabs(left).is_empty() {
                self.tabs.delete_space(left);
            }
        }
        self.motion.swap_tabs();
        self.relayout();
        self.session_dirty = true;
        match self.tabs.active {
            Some(tab) => self.focus_when_settled(tab),
            None => self.open_omnibar(Mode::NewTab),
        }
        true
    }

    /// Selects a tab of any space (Ctrl+K): the space comes first.
    pub(super) fn select_anywhere(&mut self, tab: u64) {
        if let Some(space) = self.tabs.space_of(tab).filter(|s| *s != self.tabs.space) {
            self.switch_space(space);
        }
        self.select(tab);
    }

    /// A new space after the others, made the active one; the field opens
    /// to name it.
    pub(super) fn new_space(&mut self) {
        let Some(id) = self.tabs.add_space() else { return };
        self.switch_space(id);
        self.open_omnibar(Mode::RenameSpace(id));
    }

    pub(super) fn space_command(&mut self, command: Command) {
        match command {
            Command::GoToSpace(n) => {
                if let Some(id) = self.tabs.space_by_number(n) {
                    self.switch_space(id);
                }
            }
            // Past the end they go round, and the list still comes in from
            // the side the key names.
            Command::NextSpace => {
                if let Some(id) = self.tabs.space_neighbour(1) {
                    self.switch_space_from(id, 1.0);
                }
            }
            Command::PrevSpace => {
                if let Some(id) = self.tabs.space_neighbour(-1) {
                    self.switch_space_from(id, -1.0);
                }
            }
            Command::NewSpace => self.new_space(),
            _ => {}
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
            // Named, it is kept even while it is empty.
            if self.spaces.fresh == Some(space) {
                self.spaces.fresh = None;
            }
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
            if self.spaces.fresh == Some(space) {
                self.spaces.fresh = None;
            }
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

    // ---------- the swipe ----------

    /// How far the list slides for a whole switch: the width it is drawn
    /// in.
    fn slide_width(&self) -> f32 {
        tabbar::tabs_area(&self.layout, self.scale).width().max(0.0)
    }

    /// What a swipe toward `side` pulls in: the space there, a new one past
    /// the last space that has tabs, or nothing.
    fn beside_for(&self, side: f32) -> Beside {
        let step = if side > 0.0 { 1 } else { -1 };
        match self.tabs.space_beside(step) {
            Some(id) => Beside::Space(id),
            None if step > 0 && !self.tabs.tabs.is_empty() && self.tabs.spaces.len() < MAX_SPACES => Beside::New,
            None => Beside::Nothing,
        }
    }

    /// A wheel message at the client point (x, y): a horizontal wheel
    /// (`horizontal`, a two-finger swipe on a precision touchpad) or
    /// Shift+wheel (a mouse). Over the strip it moves between spaces, and
    /// true says so; elsewhere false, and the message goes on.
    pub(super) fn space_wheel(&mut self, x: i32, y: i32, delta: i32, horizontal: bool) -> bool {
        if !self.layout.strip.contains(x, y) || self.fullscreen.on() {
            return false;
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SWIPE, SWIPE_REST_MS, Some(on_swipe_rest));
        }
        if horizontal {
            self.swipe_by(delta as f32);
        } else if !self.spaces.notched {
            // A mouse's notch is a step, not a distance: one space per
            // burst of them. Down (a negative delta) goes on, as on a page.
            self.spaces.notched = true;
            let side = if delta < 0 { 1.0 } else { -1.0 };
            if let Beside::Space(id) = self.beside_for(side) {
                self.switch_space_from(id, side);
            }
        }
        true
    }

    /// The fingers moved `delta` wheel units (positive: to the left, the
    /// next space coming in).
    fn swipe_by(&mut self, delta: f32) {
        let width = self.slide_width();
        if width <= 0.0 {
            return;
        }
        if self.mode.is_some() {
            self.close_omnibar();
        }
        self.hide_card();
        self.hide_tip();
        let now = Instant::now();
        let slide = self.spaces.slide.get_or_insert(Slide {
            offset: 0.0,
            swipe: None,
            settle: None,
            beside: Beside::Nothing,
            side: 1.0,
        });
        // A new swipe takes the list from where it is, settling or not.
        let swipe = slide.swipe.get_or_insert_with(|| {
            let offset = slide.settle.take().map_or(slide.offset, |t| t.value(now));
            Swipe::from(offset)
        });
        swipe.pull -= delta * width / SWIPE_TRAVEL;
        let pull = swipe.pull;
        let side = if pull < 0.0 { 1.0 } else { -1.0 };
        let beside = self.beside_for(side);
        let offset = swipe_offset(pull, width, beside != Beside::Nothing);
        if let Some(slide) = &mut self.spaces.slide {
            slide.beside = beside;
            slide.side = side;
            slide.offset = offset;
            if let Some(swipe) = &mut slide.swipe {
                swipe.sample(now, offset);
            }
        }
        self.spaces.named = None;
        // The frame clock draws the move on its next frame; a paint per
        // wheel message would draw more often than the screen shows.
        if !self.motion_was_running {
            self.invalidate_strip();
        }
    }

    /// The wheel stopped: a swipe settles on the space it pulled in, or
    /// back on this one.
    fn swipe_rest(&mut self) {
        self.spaces.notched = false;
        let width = self.slide_width();
        let Some(slide) = &mut self.spaces.slide else { return };
        let Some(swipe) = slide.swipe.take() else { return };
        let now = Instant::now();
        let (offset, side, beside) = (slide.offset, slide.side, slide.beside);
        let target = match beside {
            _ if !swipe_commits(offset, swipe.speed(), width) => None,
            Beside::Space(id) => Some(id),
            Beside::New => self.tabs.add_space().inspect(|id| self.spaces.fresh = Some(*id)),
            Beside::Nothing => None,
        };
        let leaving = self.tabs.space;
        match target {
            Some(id) if self.swap_space(id) => {
                // The new list carries on from where the fingers left it.
                let from = offset + side * width;
                self.spaces.slide = Some(Slide {
                    offset: from,
                    swipe: None,
                    settle: Some(Tween::new(from, 0.0, now, specs::SPACE_SLIDE)),
                    beside: Beside::Space(leaving),
                    side: -side,
                });
            }
            _ => {
                if let Some(slide) = &mut self.spaces.slide {
                    slide.settle = Some(Tween::new(offset, 0.0, now, specs::SPACE_SLIDE));
                }
            }
        }
        self.invalidate_strip();
    }

    /// Where the active list is drawn: with reduced motion it does not
    /// follow the fingers, and the switch is at once.
    fn list_dx(&self) -> f32 {
        match &self.spaces.slide {
            Some(slide) if !crate::anim::reduced() => slide.offset,
            _ => 0.0,
        }
    }

    /// How opaque the active space's name is over the list at `now`.
    fn name_opacity(&self, now: Instant) -> f32 {
        if self.spaces.slide.is_some() {
            return 1.0;
        }
        let Some(at) = self.spaces.named else { return 0.0 };
        let ms = now.saturating_duration_since(at).as_millis() as f32;
        let (hold, fade) = (NAME_HOLD_MS as f32, NAME_FADE_MS as f32);
        if ms < hold {
            1.0
        } else if ms >= hold + fade {
            0.0
        } else if crate::anim::reduced() {
            1.0
        } else {
            1.0 - (ms - hold) / fade
        }
    }

    /// Brings the slide and the name up to `now`; Some(the next frame's
    /// interval, ms) while something will change.
    pub(super) fn spaces_advance(&mut self, now: Instant) -> Option<u32> {
        if let Some(slide) = &mut self.spaces.slide
            && let Some(settle) = &slide.settle
        {
            slide.offset = settle.value(now);
            if !settle.moving(now) {
                self.spaces.slide = None;
                self.spaces.named = (self.tabs.spaces.len() > 1).then_some(now);
            }
        }
        if self.spaces.slide.is_some() {
            return Some(crate::anim::FRAME_MS);
        }
        let at = self.spaces.named?;
        let ms = now.saturating_duration_since(at).as_millis() as u32;
        if ms < NAME_HOLD_MS {
            Some((NAME_HOLD_MS - ms).max(crate::anim::FRAME_MS))
        } else if ms < NAME_HOLD_MS + NAME_FADE_MS {
            Some(crate::anim::FRAME_MS)
        } else {
            self.spaces.named = None;
            None
        }
    }

    /// The tab list slides between spaces (or follows the fingers).
    pub(super) fn spaces_sliding(&self) -> bool {
        self.spaces.slide.is_some()
    }

    /// The slide or the name changed since the last paint: the frame draws
    /// the whole strip.
    pub(super) fn spaces_moving(&self, now: Instant) -> bool {
        (self.list_dx(), self.name_opacity(now)) != self.spaces.painted
    }

    // ---------- drawing ----------

    /// The tabs' slide, for `tabbar::Paint`; the paint is noted for
    /// `spaces_moving`.
    pub(super) fn paint_slide(&mut self, now: Instant) -> f32 {
        self.spaces.painted = (self.list_dx(), self.name_opacity(now));
        self.spaces.painted.0
    }

    /// Draws, over the strip that `tabbar::paint` drew: the other list of a
    /// slide, the spaces' names, the line in the space's colour under a top
    /// strip and the mark at the foot of the column.
    pub(super) fn paint_spaces(&self, g: &Painter, now: Instant, pal: &Palette, base: &tabbar::Paint) {
        let dx = base.tabs_dx;
        let width = self.slide_width();
        let name = self.name_opacity(now);
        if let Some(slide) = &self.spaces.slide
            && !crate::anim::reduced()
        {
            let beside_dx = dx + slide.side * width;
            // A new space left empty is gone already: nothing slides out.
            if let Beside::Space(id) = slide.beside
                && self.tabs.space_by_id(id).is_some()
            {
                self.paint_beside(g, id, beside_dx, pal, base);
            }
            let beside_name = match slide.beside {
                Beside::Space(id) => self.tabs.space_by_id(id).map(|s| (s.name.clone(), s.color)),
                Beside::New => Some((self.tabs.next_space_name(), self.tabs.next_space_color())),
                Beside::Nothing => None,
            };
            if let Some((text, color)) = beside_name {
                self.paint_name(g, &text, color, beside_dx, 1.0, pal);
            }
        }
        let space = self.tabs.active_space();
        if name > 0.0 {
            self.paint_name(g, &space.name, space.color, dx, name, pal);
        }
        if self.tabs.spaces.len() < 2 {
            return;
        }
        let tone = group_tone(space.color, pal.dark).fill;
        let strip = self.layout.strip;
        if !self.layout.vertical && strip.width() > 0 {
            let px2 = (2.0 * self.scale).round();
            let bottom = strip.bottom as f32;
            g.fill(RectF::new(0.0, bottom - px2, strip.right as f32, bottom), Color::from(tone).with_alpha(0.8));
        }
        if let Some(mark) = self.layout.space_mark {
            // The column coming back slides in with everything in it.
            let rail = -(1.0 - self.motion.rail()) * strip.width() as f32;
            g.offset(rail, 0.0, |g| self.paint_mark(g, mark, &space.name, tone, pal));
        }
    }

    /// The other space's list during a slide, from the model: the same
    /// layout as the active list's, with no hover and no motion.
    fn paint_beside(&self, g: &Painter, id: u64, dx: f32, pal: &Palette, base: &tabbar::Paint) {
        let tabs: Vec<&Tab<WebTab>> = self.tabs.parked.iter().filter(|t| t.space == id).collect();
        let infos: Vec<tabbar::TabInfo> = tabs.iter().map(|t| super::tab_info(t)).collect();
        let groups = self.group_infos_of(&tabs);
        let (width, height) = self.client_size();
        let mut layout = tabbar::layout_chrome(
            &infos,
            &groups,
            width,
            height,
            self.vertical,
            self.collapsed,
            &Metrics { scale: self.scale },
            self.extension_slots(),
            self.zoom_shown(),
            self.update.is_some(),
            self.space_mark_width(),
        );
        self.place_bookmarks_in(&mut layout);
        let active = self.tabs.space_by_id(id).and_then(|s| s.last_active);
        let paint = tabbar::Paint {
            layout: &layout,
            tabs: &infos,
            active,
            palette: pal,
            hint: None,
            scale: self.scale,
            pairs: &[],
            tab_groups: &groups,
            fills: &[],
            extension_initials: &[],
            motion: &self.spaces.still,
            maximized: false,
            pressed: None,
            window_active: base.window_active,
            nav: base.nav,
            tabs_dx: dx,
        };
        tabbar::paint_tab_list(g, &paint);
    }

    /// A space's name on a small card in the middle of its list, slid by
    /// `dx`: during a slide, and for a moment after a switch.
    fn paint_name(&self, g: &Painter, text: &str, color: GroupColor, dx: f32, opacity: f32, pal: &Palette) {
        let area = tabbar::tabs_area(&self.layout, self.scale);
        if area.width() <= 0.0 {
            return;
        }
        let px = |v: f32| v * self.scale;
        let font = Font::new(px(size::TEXT), size::MEDIUM, Align::Leading);
        let text_w = gfx::measure(text, font).ceil().min(px(160.0));
        let (pad, dot, gap, h) = (px(12.0), px(MARK_DOT), px(8.0), px(28.0));
        let w = pad + dot + gap + text_w + pad;
        let cx = area.left + area.width() / 2.0 + dx;
        // On a top strip, on the tabs' row; in the column, in its middle.
        let cy = if self.layout.vertical {
            area.top + area.height() / 2.0
        } else {
            RectF::from(self.layout.nav[0]).center().1
        };
        let card = RectF::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
        g.clip(area, |g| {
            g.ring_card(card, h / 2.0, Color::from(pal.tab_active).with_alpha(opacity), pal.dark, opacity, self.scale);
            let tone = group_tone(color, pal.dark).fill;
            let dot_x = card.left + pad;
            g.round(
                RectF::new(dot_x, cy - dot / 2.0, dot_x + dot, cy + dot / 2.0),
                dot / 2.0,
                Color::from(tone).with_alpha(opacity),
            );
            let left = dot_x + dot + gap;
            g.text(text, RectF::new(left, card.top, card.right - pad + px(1.0), card.bottom), font, {
                Color::from(pal.ink).with_alpha(opacity)
            });
        });
    }

    /// The mark at the foot of the column: the active space's dot and name.
    fn paint_mark(&self, g: &Painter, mark: Rect, name: &str, tone: crate::design::Rgb, pal: &Palette) {
        let px = |v: f32| v * self.scale;
        let rect = RectF::from(mark);
        let over = self.motion.hover(HoverKey::SpaceMark);
        let pressed = self.motion.pressed(HoverKey::SpaceMark);
        if over + pressed > 0.0 {
            let ground = pal.tab_hover.mix(pal.pin_live, pressed);
            g.round(rect, px(size::RADIUS_CONTROL), Color::from(ground).with_alpha(over.max(pressed)));
        }
        let dot = px(MARK_DOT);
        let (_, cy) = rect.center();
        let dot_x = rect.left + px(MARK_PAD as f32);
        g.round(RectF::new(dot_x, cy - dot / 2.0, dot_x + dot, cy + dot / 2.0), dot / 2.0, tone);
        let left = dot_x + dot + px(MARK_GAP as f32);
        let text = RectF::new(left, rect.top, rect.right - px(MARK_PAD as f32) + px(1.0), rect.bottom);
        g.text(name, text, mark_font(self.scale), pal.ink_muted.mix(pal.ink, over));
    }

    // ---------- the mark ----------

    /// The tooltip over the mark: the space, and how to go to another.
    pub(super) fn space_tip(&self) -> Option<String> {
        let name = &self.tabs.active_space().name;
        Some(format!("{name}. Swipe sideways on the tabs to go to a different space"))
    }

    pub(super) fn space_tip_anchor(&self) -> Option<Rect> {
        self.layout.space_mark
    }

    /// A press on the mark opens the active space's menu.
    pub(super) fn space_mouse_down(&mut self, x: i32, y: i32) {
        self.motion.press(HoverKey::SpaceMark);
        self.invalidate_strip();
        self.space_menu(x, y, self.tabs.space);
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

    /// The items of the active space ("Rename space…", "Color ▸", "Delete
    /// space"), as a "This space ▸" menu in `menu`: the window menu's.
    pub(super) fn append_this_space(&self, menu: HMENU) {
        unsafe {
            let Ok(sub) = CreatePopupMenu() else { return };
            self.append_space_menu_items(sub, self.tabs.space);
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("This space"));
        }
    }

    /// Runs an item of `append_this_space`; false when `chosen` is not one.
    pub(super) fn this_space_choice(&mut self, chosen: usize) -> bool {
        self.space_menu_chosen(self.tabs.space, chosen)
    }

    fn append_space_menu_items(&self, menu: HMENU, space: u64) {
        let Some(s) = self.tabs.space_by_id(space) else { return };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, MENU_RENAME, w!("Rename space…"));
            if let Ok(colors) = CreatePopupMenu() {
                for (i, color) in GroupColor::ALL.iter().enumerate() {
                    let check = if *color == s.color { MF_CHECKED } else { MF_UNCHECKED };
                    let _ = AppendMenuW(colors, MF_STRING | check, MENU_COLOR + i, &wide(color.label()));
                }
                let _ = AppendMenuW(menu, MF_POPUP, colors.0 as usize, w!("Color"));
            }
            // Its tabs are kept: they go to the space before it. The last
            // space cannot go.
            let only = if self.tabs.spaces.len() < 2 { MF_GRAYED } else { MF_ENABLED };
            let _ = AppendMenuW(menu, MF_STRING | only, MENU_DELETE, w!("Delete space (keep its tabs)"));
        }
    }

    fn space_menu_chosen(&mut self, space: u64, chosen: usize) -> bool {
        match chosen {
            MENU_RENAME => self.open_omnibar(Mode::RenameSpace(space)),
            MENU_NEW => self.new_space(),
            MENU_DELETE => self.delete_space(space),
            n if (MENU_COLOR..MENU_COLOR + GroupColor::ALL.len()).contains(&n) => {
                self.tabs.set_space_color(space, GroupColor::ALL[n - MENU_COLOR]);
                self.session_dirty = true;
                self.invalidate();
            }
            _ => return false,
        }
        true
    }

    /// The menu of a space: a click on the mark, or a right click on it.
    pub(super) fn space_menu(&mut self, x: i32, y: i32, space: u64) {
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            self.append_space_menu_items(menu, space);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let full = if self.tabs.spaces.len() >= MAX_SPACES { MF_GRAYED } else { MF_ENABLED };
            let _ = AppendMenuW(menu, MF_STRING | full, MENU_NEW, w!("New space"));
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
        self.space_menu_chosen(space, chosen);
    }

    /// The `spaces` key of the script control's state.
    pub(super) fn spaces_state(&self) -> serde_json::Value {
        let spaces: Vec<serde_json::Value> = self
            .tabs
            .spaces
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "name": s.name,
                    "color": s.color.name(),
                    "active": s.id == self.tabs.space,
                    "tabs": self.tabs.space_tabs(s.id),
                    "last_active": if s.id == self.tabs.space { self.tabs.active } else { s.last_active },
                })
            })
            .collect();
        serde_json::json!({
            "list": spaces,
            "mark": self.layout.space_mark.map(|r| [r.left, r.top, r.right, r.bottom]),
            "slide": self.spaces.slide.as_ref().map(|s| serde_json::json!({
                "offset": s.offset,
                "fingers": s.swipe.is_some(),
                "beside": match s.beside {
                    Beside::Space(id) => serde_json::json!(id),
                    Beside::New => serde_json::json!("new"),
                    Beside::Nothing => serde_json::Value::Null,
                },
            })),
            "fresh": self.spaces.fresh,
        })
    }
}

/// The mark's type: the small size, as the group labels.
fn mark_font(scale: f32) -> Font {
    let font = tabbar::label_font(scale);
    Font::new(font.size, font.weight, Align::Leading)
}
