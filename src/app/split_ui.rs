//! Split view on screen: two tabs of a pair side by side, the divider that
//! resizes them, a tab dropped on the page, the field opened to choose the
//! other side, and focus moving between the two pages. The rules (pairs,
//! widths, persistence) live in `split`; this is the window side of them.

use std::cell::Cell;

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HDC, PAINTSTRUCT,
            ScreenToClient,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{GetFocus, ReleaseCapture, SetCapture},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

use super::{App, Mode, Row, fuzzy_match, unix_now};
use crate::{
    design::Rgb,
    omnibox,
    split::{self, Pair, SavedPair, Side, Splits},
    tab_groups::Drop,
    tabbar::Rect,
};

/// Divider width, the narrowest a page gets and the focus line above the
/// page that has the keyboard, in logical pixels.
const DIVIDER: i32 = 6;
const MIN_SIDE: i32 = 240;
const FOCUS_LINE: i32 = 2;
/// Opacity of the hint over the half a dragged tab would land on.
const HINT_ALPHA: u8 = 70;

thread_local! {
    static HINT_COLOR: Cell<u32> = const { Cell::new(0) };
}

enum Holder {
    Page(u64),
    /// The field, another window, or no page on screen.
    Elsewhere,
    Unknown,
}

#[derive(Default)]
pub(super) struct SplitUi {
    pub(super) splits: Splits,
    /// The divider is being dragged.
    resizing: bool,
    /// The active tab when a tab drag began: the page a tab dropped on the
    /// content area is paired with (the click already selected the dragged one).
    pub(super) drag_origin: Option<u64>,
    /// The translucent window over the half a dragged tab would land on.
    hint: Option<(HWND, Side)>,
}

impl SplitUi {
    pub(super) fn new(splits: Splits) -> Self {
        Self { splits, ..Default::default() }
    }
}

impl App {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }

    /// The pair on screen: the active tab's, when split view is on.
    pub(super) fn shown_pair(&self) -> Option<Pair> {
        if !self.settings.split_view {
            return None;
        }
        self.tabs.active.and_then(|id| self.split.splits.pair_of(id)).copied()
    }

    fn panes_of(&self, pair: &Pair) -> split::Panes {
        let c = self.layout.content;
        let content = Rect::new(c.left, c.top + self.px(FOCUS_LINE), c.right, c.bottom);
        split::panes(content, pair.ratio, self.px(DIVIDER), self.px(MIN_SIDE))
    }

    /// Where each page on screen goes: the active tab alone, or both of its pair.
    pub(super) fn shown_panes(&self) -> Vec<(u64, RECT)> {
        match self.shown_pair() {
            Some(pair) => {
                let panes = self.panes_of(&pair);
                vec![(pair.left, panes.left.win()), (pair.right, panes.right.win())]
            }
            None => self.tabs.active.map(|id| (id, self.layout.content.win())).into_iter().collect(),
        }
    }

    /// The tabs whose pages are on screen: the active one and its partner.
    pub(super) fn visible_tabs(&self) -> Vec<u64> {
        self.shown_panes().into_iter().map(|(id, _)| id).collect()
    }

    /// The page with the keyboard: the field opens over it.
    pub(super) fn focused_pane(&self) -> Rect {
        let Some(pair) = self.shown_pair() else { return self.layout.content };
        let side = self.tabs.active.and_then(|id| pair.side_of(id)).unwrap_or(Side::Left);
        self.panes_of(&pair).of(side)
    }

    /// What the strip draws on one ground: every pair, while split view is on.
    pub(super) fn split_pairs(&self) -> Vec<(u64, u64)> {
        if !self.settings.split_view {
            return Vec::new();
        }
        self.split.splits.pairs().iter().map(|p| (p.left, p.right)).collect()
    }

    /// The divider (with a line down its middle) and the focus line over the
    /// page that has the keyboard.
    pub(super) fn split_fills(&self) -> Vec<(Rect, Rgb)> {
        let Some(pair) = self.shown_pair() else { return Vec::new() };
        let panes = self.panes_of(&pair);
        let d = panes.divider;
        let middle = (d.left + d.right) / 2;
        let top = self.layout.content.top;
        let focused = self.focused_pane();
        vec![
            (Rect::new(middle, d.top, middle + 1, d.bottom), self.palette.line),
            (Rect::new(focused.left, top, focused.right, focused.top), self.palette.accent),
        ]
    }

    // ---------- keeping the strip and the pairs consistent ----------

    /// Puts partners side by side in the strip (see `split::arrange`), in
    /// the same tab group, and keeps every group's tabs together.
    pub(super) fn arrange_tabs(&mut self, anchor: Option<u64>) -> bool {
        let regrouped = self.pair_groups(anchor);
        let order: Vec<(u64, bool)> = self.tabs.tabs.iter().map(|t| (t.id, t.pinned)).collect();
        let arranged = split::arrange(&order, self.split.splits.pairs(), anchor);
        let moved = arranged != order;
        if moved {
            for tab in &mut self.tabs.tabs {
                if let Some(&(_, pinned)) = arranged.iter().find(|(id, _)| *id == tab.id) {
                    tab.pinned = pinned;
                }
            }
            self.tabs.tabs.sort_by_key(|t| arranged.iter().position(|(id, _)| *id == t.id));
        }
        let gathered = self.tabs.normalize_groups();
        if moved || regrouped || gathered {
            self.session_dirty = true;
        }
        moved || regrouped || gathered
    }

    /// A pair sits in one tab group or in none: the partner takes the
    /// group of the anchor (the tab being dragged or the page that stays),
    /// or else of the pair's first tab in the strip.
    fn pair_groups(&mut self, anchor: Option<u64>) -> bool {
        let mut changed = false;
        for pair in self.split.splits.pairs().to_vec() {
            let (a, b) = (self.tabs.group_of(pair.left), self.tabs.group_of(pair.right));
            if a == b {
                continue;
            }
            let lead = match anchor {
                Some(x) if pair.contains(x) => x,
                _ => match (self.tabs.index_of(pair.left), self.tabs.index_of(pair.right)) {
                    (Some(l), Some(r)) if r < l => pair.right,
                    _ => pair.left,
                },
            };
            let group = self.tabs.group_of(lead);
            let partner = pair.partner(lead).unwrap_or(lead);
            if let Some(t) = self.tabs.get_mut(partner) {
                t.group = group;
                changed = true;
            }
        }
        changed
    }

    /// Before every layout: pairs that lost a tab are gone, partners sit together.
    pub(super) fn tidy_pairs(&mut self) {
        let tabs = &self.tabs;
        self.split.splits.retain(|id| tabs.get(id).is_some());
        self.arrange_tabs(None);
    }

    /// Closing a tab undoes its pair. When it was the page with focus, its
    /// partner is the tab to show next.
    pub(super) fn unpair_closing(&mut self, id: u64) -> Option<u64> {
        let partner = self.split.splits.split_off(id)?;
        (self.tabs.active == Some(id)).then_some(partner)
    }

    /// Pinning one tab of a pair pins both.
    pub(super) fn toggle_pin_with_pair(&mut self, id: u64) {
        self.tabs.toggle_pin(id);
        let Some(partner) = self.split.splits.partner(id) else { return };
        let pinned = self.tabs.get(id).is_some_and(|t| t.pinned);
        if self.tabs.get(partner).is_some_and(|t| t.pinned != pinned) {
            self.tabs.toggle_pin(partner);
        }
        self.arrange_tabs(Some(id));
    }

    /// The pairs for session.json, by position among the tabs written there:
    /// the same tabs `saved_tabs` keeps (no private, test or empty ones).
    pub(super) fn saved_splits(&self) -> Vec<SavedPair> {
        self.split.splits.to_saved(&super::private_tabs::kept_ids(&self.tabs))
    }

    // ---------- commands ----------

    /// Ctrl+Alt+N: the field opens to choose what goes beside the current page.
    pub(super) fn split_command(&mut self) {
        if self.settings.split_view && self.tabs.active.is_some() {
            self.open_omnibar(Mode::Split);
        }
    }

    pub(super) fn unsplit(&mut self, id: u64) {
        if self.split.splits.split_off(id).is_some() {
            self.session_dirty = true;
            self.relayout();
            self.focus_page();
        }
    }

    /// Alt+Shift+Left/Right: the keyboard goes to that page of the pair.
    pub(super) fn focus_side(&mut self, side: Side) {
        if let Some(pair) = self.shown_pair() {
            if self.mode.is_some() {
                self.close_omnibar();
            }
            self.select(pair.at(side));
        }
    }

    /// Pairs `other` with the active tab: it takes the side the active tab's
    /// partner had (the active tab stays where it is), or the right.
    pub(super) fn split_with(&mut self, other: u64) {
        let Some(anchor) = self.tabs.active else { return };
        let side =
            self.split.splits.pair_of(anchor).and_then(|p| p.side_of(anchor)).map_or(Side::Right, Side::opposite);
        if !self.split.splits.join(other, side, anchor) {
            return;
        }
        self.arrange_tabs(Some(anchor));
        self.tabs.select(other);
        self.session_dirty = true;
        self.relayout();
        self.focus_page();
    }

    fn split_open(&mut self, url: &str) {
        let at = self.tabs.after_active();
        // Beside a private page the new one is private too: what is typed
        // there must not reach the history.
        let private = self.tabs.active_tab().is_some_and(|t| t.private);
        let id = if private { self.tabs.insert_private(url, at) } else { self.tabs.insert(url, "", false, at) };
        self.split_with(id);
    }

    /// The field in split mode: open tabs first, then the history.
    pub(super) fn split_rows(&self, text: &str) -> Vec<(String, Row)> {
        let shown = self.visible_tabs();
        let tabs = self
            .tabs
            .tabs
            .iter()
            .filter(|t| !shown.contains(&t.id) && (fuzzy_match(text, t.label()) || fuzzy_match(text, &t.url)))
            .take(6)
            .map(|t| (format!("Pestaña: {}    {}", t.label(), omnibox::display_host(&t.url)), Row::Switch(t.id)));
        let history = self.history.suggest(text, 6, unix_now()).into_iter().map(|e| {
            let address = omnibox::strip_for_match(&e.url);
            let title = if e.title.is_empty() { address } else { &e.title };
            (format!("{title}    {address}"), Row::Go(e.url.clone()))
        });
        tabs.chain(history).collect()
    }

    /// Enter in split mode: a listed tab or address, what was typed, or —
    /// with nothing typed — the same page again.
    pub(super) fn split_submit(&mut self, row: Option<usize>) {
        let chosen = row.and_then(|r| self.rows.get(r)).cloned();
        let text = self.omnibar.text();
        self.close_omnibar();
        match chosen {
            Some(Row::Switch(id)) => self.split_with(id),
            Some(Row::Go(url)) => self.split_open(&url),
            Some(Row::Info | Row::Panel(_) | Row::InstallExtension(_)) => {}
            None if text.trim().is_empty() => {
                if let Some(url) = self.tabs.active_tab().map(|t| t.url.clone()) {
                    self.split_open(&url);
                }
            }
            None => {
                if let Some(url) = omnibox::resolve(&text, &self.settings.search_url) {
                    self.split_open(&url);
                }
            }
        }
    }

    /// Where the keyboard is right now. Focus events arrive queued, so the one
    /// being handled can be stale: the app may have moved focus to the other
    /// page since (the field closing, then a new page selected).
    fn keyboard_holder(&self) -> Holder {
        unsafe {
            let focus = GetFocus();
            if focus.is_invalid() {
                return Holder::Unknown;
            }
            if !IsChild(self.hwnd, focus).as_bool() {
                return Holder::Elsewhere;
            }
            let mut rect = RECT::default();
            let _ = GetWindowRect(focus, &mut rect);
            let mut center = POINT { x: (rect.left + rect.right) / 2, y: (rect.top + rect.bottom) / 2 };
            let _ = ScreenToClient(self.hwnd, &mut center);
            let inside =
                |r: &RECT| center.x >= r.left && center.x < r.right && center.y >= r.top && center.y < r.bottom;
            match self.shown_panes().into_iter().find(|(_, r)| inside(r)) {
                Some((id, _)) => Holder::Page(id),
                None => Holder::Elsewhere,
            }
        }
    }

    /// A page took focus. When it is the other page of the pair on screen,
    /// that page becomes the active tab: the field, the search and the
    /// commands follow the keyboard.
    pub(super) fn page_focused(&mut self, event_id: u64) {
        let id = match self.keyboard_holder() {
            Holder::Page(id) => id,
            // A stale event: the keyboard went somewhere else since.
            Holder::Elsewhere => return,
            Holder::Unknown => event_id,
        };
        let switches = self.tabs.active != Some(id) && self.shown_pair().is_some_and(|p| p.contains(id));
        if switches {
            if self.mode == Some(Mode::Find) {
                self.with_active_web(|w| w.find_stop());
            }
            self.tabs.select(id);
            self.update_title();
            self.session_dirty = true;
            self.invalidate();
        }
        if self.mode.is_some() {
            self.close_omnibar();
        }
    }

    // ---------- mouse ----------

    fn divider_rect(&self) -> Option<Rect> {
        let pair = self.shown_pair()?;
        let d = self.panes_of(&pair).divider;
        // The visible gap is thin: the grip reaches a little past it (the
        // pages cover the rest, so only the gap itself gets the cursor).
        Some(Rect::new(d.left - self.px(2), self.layout.content.top, d.right + self.px(2), d.bottom))
    }

    /// A press on the divider starts resizing the pair.
    pub(super) fn divider_mouse_down(&mut self, x: i32, y: i32) -> bool {
        if !self.divider_rect().is_some_and(|r| r.contains(x, y)) {
            return false;
        }
        if self.mode.is_some() {
            self.close_omnibar();
        }
        self.split.resizing = true;
        unsafe {
            SetCapture(self.hwnd);
            SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
        }
        true
    }

    /// While resizing, the divider follows the pointer.
    pub(super) fn divider_drag(&mut self, x: i32) -> bool {
        if !self.split.resizing {
            return false;
        }
        let Some(id) = self.tabs.active else { return true };
        let c = self.layout.content;
        let ratio = split::ratio_at(c, x, self.px(DIVIDER), self.px(MIN_SIDE));
        self.split.splits.set_ratio(id, ratio);
        unsafe {
            SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
        }
        self.show_active();
        self.invalidate();
        true
    }

    /// The resize cursor over the divider (WM_SETCURSOR).
    pub(super) fn cursor_on_divider(&self) -> bool {
        let Some(rect) = self.divider_rect() else { return false };
        let mut point = POINT::default();
        unsafe {
            if GetCursorPos(&mut point).is_err() {
                return false;
            }
            let _ = ScreenToClient(self.hwnd, &mut point);
        }
        rect.contains(point.x, point.y)
    }

    /// A dragged tab over the page shows where it would land; over the
    /// strip it moves along it, its partner with it.
    pub(super) fn drag_tab(&mut self, dragged: u64, x: i32, y: i32) {
        if self.settings.split_view && self.layout.content.contains(x, y) {
            self.show_drop_hint(split::side_at(self.layout.content, x));
            return;
        }
        self.hide_drop_hint();
        let Some(at) = self.layout.drop_at(x, y) else { return };
        if at == Drop::Tab(self.tabs.index_of(dragged).unwrap_or(usize::MAX)) {
            return;
        }
        // Dropped onto a tab it takes its place and its group; over a
        // label it goes right before that group; past the end, out of any.
        let before: Vec<(u64, Option<u64>)> = self.tabs.tabs.iter().map(|t| (t.id, t.group)).collect();
        self.tabs.drop_tab(dragged, at);
        self.arrange_tabs(Some(dragged));
        if self.tabs.tabs.iter().map(|t| (t.id, t.group)).ne(before) {
            self.session_dirty = true;
            self.relayout_soft();
        }
    }

    pub(super) fn mouse_up(&mut self, x: i32, y: i32) {
        if self.nav_mouse_up(x, y) || self.group_mouse_up() {
            return;
        }
        if self.split.resizing {
            self.split.resizing = false;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.session_dirty = true;
            self.focus_page();
            return;
        }
        let Some(dragged) = self.drag.take() else { return };
        unsafe {
            let _ = ReleaseCapture();
        }
        self.hide_drop_hint();
        let origin = self.split.drag_origin.take();
        if self.settings.split_view && self.layout.content.contains(x, y) {
            self.drop_on_page(dragged, origin, split::side_at(self.layout.content, x));
        }
    }

    /// Someone else took the mouse mid-drag: nothing is dropped.
    pub(super) fn capture_lost(&mut self) {
        self.group_capture_lost();
        self.nav_capture_lost();
        self.split.resizing = false;
        self.drag = None;
        self.split.drag_origin = None;
        self.hide_drop_hint();
    }

    /// The dragged tab goes on `side`, beside the page that was on screen
    /// when the drag began. Dragging the page already on screen to the other
    /// half of its own pair swaps the two.
    fn drop_on_page(&mut self, dragged: u64, origin: Option<u64>, side: Side) {
        match origin.filter(|o| *o != dragged) {
            Some(origin) => {
                self.split.splits.join(dragged, side, origin);
                self.arrange_tabs(Some(origin));
            }
            None => match self.split.splits.pair_of(dragged).and_then(|p| p.side_of(dragged)) {
                Some(current) if current != side => self.split.splits.swap_sides(dragged),
                _ => return,
            },
        }
        self.session_dirty = true;
        self.relayout();
        self.focus_page();
    }

    fn show_drop_hint(&mut self, side: Side) {
        if self.split.hint.is_some_and(|(_, s)| s == side) {
            return;
        }
        let half = split::half(self.layout.content, side);
        let mut origin = POINT { x: half.left, y: half.top };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut origin);
        }
        let hwnd = match self.split.hint {
            Some((hwnd, _)) => hwnd,
            None => {
                HINT_COLOR.with(|c| c.set(self.palette.accent.colorref().0));
                match create_hint(self.hwnd) {
                    Some(hwnd) => hwnd,
                    None => return,
                }
            }
        };
        // A new hint fades in as a popover (100 ms); one moving to the
        // other half just moves.
        crate::fade::fade_in(hwnd, crate::anim::specs::POPOVER_IN, HINT_ALPHA, 0);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                origin.x,
                origin.y,
                half.width(),
                half.bottom - half.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.split.hint = Some((hwnd, side));
    }

    fn hide_drop_hint(&mut self) {
        if let Some((hwnd, _)) = self.split.hint.take() {
            crate::fade::fade_out(hwnd, crate::anim::specs::POPOVER_OUT, 0, Some(destroy_hint));
        }
    }
}

fn destroy_hint(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// A click-through, never-activated, translucent popup over the page (the
/// pages are child windows: the main window can't draw over them).
fn create_hint(owner: HWND) -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(hint_proc),
            hInstance: instance,
            lpszClassName: w!("NeaSplitHint"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("NeaSplitHint"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(owner),
            None,
            Some(instance),
            None,
        )
        .ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), HINT_ALPHA, LWA_ALPHA);
        Some(hwnd)
    }
}

unsafe extern "system" fn hint_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut rect);
                let brush = CreateSolidBrush(COLORREF(HINT_COLOR.with(|c| c.get())));
                FillRect(HDC(wparam.0 as _), &rect, brush);
                let _ = DeleteObject(brush.into());
            }
            LRESULT(1)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
