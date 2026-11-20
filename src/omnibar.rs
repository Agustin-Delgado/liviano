//! The single field: a small popup over the page with a native edit control
//! and, under it, the list of suggestions. It serves every mode — address,
//! tab switcher, find in page and the bookmarks, history, downloads and
//! passwords panels — so there is one field, not six.
//!
//! The edit stays the system's (IME, selection, clipboard, undo all come
//! with it); everything around it is drawn with Direct2D: the floating
//! panel's ground and its search glass, and the list, an owner-drawn list
//! box whose rows read as Search's (a site mark, the title, the address
//! dimmed, a soft rounded ground for the picked row). Windows 11 rounds the
//! popup's corners and casts its shadow.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HDC, HFONT, InvalidateRect, PAINTSTRUCT,
            SetBkColor, SetTextColor,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                DRAWITEMSTRUCT, EM_GETSEL, EM_SETCUEBANNER, EM_SETMARGINS, EM_SETSEL, ODS_SELECTED, SetWindowTheme,
                WM_MOUSELEAVE,
            },
            Input::KeyboardAndMouse::{
                GetKeyState, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DELETE, VK_DOWN, VK_ESCAPE,
                VK_RETURN, VK_SHIFT, VK_UP,
            },
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    design::{Palette, dwm, size},
    gfx::{Align, Color, Font, Painter, RectF, Surface},
};

/// Whether the foreground window is one of this process's. Taking the focus
/// otherwise would pull the keyboard away from the program the person is
/// typing in.
pub fn owns_foreground() -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(foreground, Some(&mut pid));
        pid == windows::Win32::System::Threading::GetCurrentProcessId()
    }
}

const LIST_ID: usize = 2;
const LBN_SELCHANGE_CODE: u32 = 1;
const EN_CHANGE_CODE: u32 = 0x0300;

/// What the popup asks of the app. The app decides; the popup only draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Changed,
    Submit { row: Option<usize>, shift: bool },
    /// Supr on a selected row of a panel.
    Delete { row: usize },
    Cancel,
}

thread_local! {
    static HANDLER: Cell<Option<fn(Signal)>> = const { Cell::new(None) };
    /// The edit's ground brush (WM_CTLCOLOREDIT), its colour and the ink.
    static BRUSHES: Cell<(isize, u32, u32)> = const { Cell::new((0, 0, 0)) };
    static LOOK: Cell<Option<(Palette, f32)>> = const { Cell::new(None) };
    /// One surface for the panel's ground and one for the list's rows.
    static SURFACES: RefCell<(Surface, Surface)> = RefCell::new(Default::default());
    /// The row under the pointer, drawn with a quieter ground than the picked one.
    static HOVERED: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Space around the list inside the panel.
const LIST_PAD: f32 = 6.0;
/// The glass's column at the field's left.
const GLASS_COLUMN: f32 = 44.0;

fn signal(s: Signal) {
    if let Some(handler) = HANDLER.with(|h| h.get()) {
        handler(s);
    }
}

pub struct Omnibar {
    pub popup: HWND,
    pub edit: HWND,
    pub list: HWND,
    rows: usize,
    row_height: i32,
    scale: f32,
    edit_height: i32,
    /// Rows shown at once; the list scrolls past them.
    visible_rows: usize,
    /// Supr deletes the selected row instead of a character.
    rows_deletable: bool,
}

impl Omnibar {
    pub fn create(owner: HWND, handler: fn(Signal)) -> windows::core::Result<Self> {
        HANDLER.with(|h| h.set(Some(handler)));
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                // The shadow under the rounded panel.
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(popup_proc),
                hInstance: instance,
                lpszClassName: w!("LivianoOmnibar"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            let popup = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("LivianoOmnibar"),
                PCWSTR::null(),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                100,
                40,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                100,
                30,
                Some(popup),
                Some(HMENU(1 as _)),
                Some(instance),
                None,
            )?;
            let list = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("LISTBOX"),
                PCWSTR::null(),
                WS_CHILD
                    | WS_VSCROLL
                    | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_OWNERDRAWFIXED | LBS_HASSTRINGS) as u32),
                0,
                30,
                100,
                10,
                Some(popup),
                Some(HMENU(LIST_ID as _)),
                Some(instance),
                None,
            )?;
            let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
            let _ = SetWindowSubclass(list, Some(list_proc), 2, 0);
            dwm::set(popup, dwm::CORNERS, dwm::ROUND);
            Ok(Self {
                popup,
                edit,
                list,
                rows: 0,
                row_height: 36,
                scale: 1.0,
                edit_height: 34,
                visible_rows: usize::MAX,
                rows_deletable: false,
            })
        }
    }

    /// The edit is set in a larger cut of the interface's face (`font` is
    /// the one other controls use; this one is the field's own).
    pub fn set_font(&mut self, font: HFONT, scale: f32) {
        let edit_font = crate::app::make_font_sized(size::TEXT_FIELD, scale);
        unsafe {
            let old = SendMessageW(self.edit, WM_GETFONT, None, None).0;
            SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(edit_font.0 as usize)), Some(LPARAM(1)));
            if old != 0 && old != font.0 as isize {
                let _ = DeleteObject(HFONT(old as _).into());
            }
            // Owner-drawn rows: their height is ours, not the font's.
            self.row_height = (36.0 * scale).round() as i32;
            SendMessageW(self.list, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(self.row_height as isize)));
        }
        self.edit_height = (48.0 * scale).round() as i32;
        self.scale = scale;
        LOOK.with(|l| l.set(l.get().map(|(p, _)| (p, scale))));
    }

    pub fn set_palette(&self, palette: &Palette) {
        BRUSHES.with(|b| {
            let (old, _, _) = b.get();
            unsafe {
                if old != 0 {
                    let _ = DeleteObject(HBRUSH(old as _).into());
                }
                let field = CreateSolidBrush(palette.field.colorref());
                b.set((field.0 as isize, palette.field.colorref().0, palette.ink.colorref().0));
            }
        });
        LOOK.with(|l| l.set(Some((*palette, self.scale))));
        dwm::set(self.popup, dwm::BORDER_COLOR, palette.line.colorref().0);
        dwm::set(self.popup, dwm::DARK_MODE, palette.dark as u32);
        unsafe {
            let theme = if palette.dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
            let _ = SetWindowTheme(self.list, theme, PCWSTR::null());
            let _ = InvalidateRect(Some(self.popup), None, true);
            let _ = InvalidateRect(Some(self.list), None, true);
        }
    }

    /// How many rows show at once and whether Supr deletes one: a panel is
    /// a taller list whose rows can be removed.
    pub fn set_list_mode(&mut self, visible_rows: usize, rows_deletable: bool) {
        self.visible_rows = visible_rows.max(1);
        self.rows_deletable = rows_deletable;
    }

    /// Opens at `anchor` (screen coordinates: left, top, width).
    pub fn open(&mut self, anchor: (i32, i32, i32), cue: &str, text: &str, select_all: bool) {
        unsafe {
            let cue = HSTRING::from(cue);
            SendMessageW(self.edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
            let _ = SetWindowTextW(self.edit, &HSTRING::from(text));
            let len = text.encode_utf16().count() as isize;
            let start = if select_all { 0 } else { len };
            SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(start as usize)), Some(LPARAM(len)));
        }
        self.set_rows(&[]);
        self.place(anchor);
        unsafe {
            if owns_foreground() {
                let _ = ShowWindow(self.popup, SW_SHOW);
                let _ = SetFocus(Some(self.edit));
            } else {
                // Another program has the keyboard (a scripted run, or the
                // person switched away): the field shows without taking it,
                // so what is typed there never lands here. A click on the
                // window gives the field the focus as usual.
                let _ = ShowWindow(self.popup, SW_SHOWNOACTIVATE);
            }
        }
    }

    pub fn place(&self, (left, top, width): (i32, i32, i32)) {
        let shown = self.rows.min(self.visible_rows);
        let pad = (LIST_PAD * self.scale).round() as i32;
        let list_h = shown as i32 * self.row_height;
        let below = if shown > 0 { list_h + 2 * pad + 1 } else { 0 };
        let glass = (GLASS_COLUMN * self.scale).round() as i32;
        let text_h = (self.edit_height as f32 * 0.5).round() as i32;
        unsafe {
            let _ = SetWindowPos(self.popup, Some(HWND_TOP), left, top, width, self.edit_height + below, SWP_NOACTIVATE);
            let _ = SetWindowPos(
                self.edit,
                None,
                glass,
                (self.edit_height - text_h) / 2,
                width - glass - (16.0 * self.scale).round() as i32,
                text_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // EC_LEFTMARGIN | EC_RIGHTMARGIN, both zero: the glass column is the margin.
            SendMessageW(self.edit, EM_SETMARGINS, Some(WPARAM(3)), Some(LPARAM(0)));
            let _ = SetWindowPos(
                self.list,
                None,
                pad,
                self.edit_height + 1 + pad,
                width - 2 * pad,
                list_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.list, if self.rows > 0 { SW_SHOWNA } else { SW_HIDE });
            let _ = InvalidateRect(Some(self.popup), None, false);
        }
    }

    pub fn close(&mut self) {
        unsafe {
            let _ = ShowWindow(self.popup, SW_HIDE);
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = GetWindowTextW(self.edit, &mut buffer) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        }
    }

    /// Caret position and whether text is selected after it.
    pub fn selection(&self) -> (usize, usize) {
        let (mut start, mut end) = (0u32, 0u32);
        unsafe {
            SendMessageW(
                self.edit,
                EM_GETSEL,
                Some(WPARAM(&mut start as *mut u32 as usize)),
                Some(LPARAM(&mut end as *mut u32 as isize)),
            );
        }
        (start as usize, end as usize)
    }

    /// Puts `completion` after the typed text, selected, so typing on
    /// replaces it and Enter accepts it.
    pub fn complete_inline(&self, typed: &str, completion: &str) {
        let full = format!("{typed}{completion}");
        let start = typed.encode_utf16().count();
        let end = full.encode_utf16().count();
        unsafe {
            let _ = SetWindowTextW(self.edit, &HSTRING::from(full));
            SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(start)), Some(LPARAM(end as isize)));
        }
    }

    pub fn set_rows(&mut self, rows: &[String]) {
        HOVERED.with(|h| h.set(None));
        unsafe {
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
            for row in rows {
                let text = HSTRING::from(row.as_str());
                SendMessageW(self.list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
            }
        }
        self.rows = rows.len();
    }

    pub fn selected_row(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None).0 };
        (index >= 0).then_some(index as usize)
    }

    pub fn select_row(&self, row: Option<usize>) {
        let index = row.map_or(usize::MAX, |r| r);
        unsafe {
            SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(index)), None);
        }
    }

    fn move_selection(&self, delta: isize) {
        if self.rows == 0 {
            return;
        }
        let next = match self.selected_row() {
            None if delta > 0 => Some(0),
            None => Some(self.rows - 1),
            Some(r) => {
                let n = r as isize + delta;
                if n < 0 || n >= self.rows as isize { None } else { Some(n as usize) }
            }
        };
        self.select_row(next);
    }
}

thread_local! {
    static INSTANCE: Cell<Option<*const Omnibar>> = const { Cell::new(None) };
}

/// The edit's key handling needs to move the list selection; the app keeps
/// the `Omnibar` alive for the whole run and registers it here.
pub fn register(omnibar: &Omnibar) {
    INSTANCE.with(|i| i.set(Some(omnibar as *const Omnibar)));
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
            if key == VK_DOWN.0 || key == VK_UP.0 {
                if let Some(omnibar) = INSTANCE.with(|i| i.get()) {
                    unsafe { (*omnibar).move_selection(if key == VK_DOWN.0 { 1 } else { -1 }) };
                }
                return LRESULT(0);
            }
            if key == VK_RETURN.0 {
                let row = INSTANCE.with(|i| i.get()).and_then(|o| unsafe { (*o).selected_row() });
                signal(Signal::Submit { row, shift });
                return LRESULT(0);
            }
            if key == VK_ESCAPE.0 {
                signal(Signal::Cancel);
                return LRESULT(0);
            }
            if key == VK_DELETE.0 {
                let row = INSTANCE
                    .with(|i| i.get())
                    .and_then(|o| unsafe { if (*o).rows_deletable { (*o).selected_row() } else { None } });
                if let Some(row) = row {
                    signal(Signal::Delete { row });
                    return LRESULT(0);
                }
            }
        }
        // Swallow the characters of Enter and Esc so the edit doesn't beep.
        WM_CHAR if wparam.0 == 0x0D || wparam.0 == 0x1B => return LRESULT(0),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let code = (wparam.0 >> 16) as u32 & 0xFFFF;
            let id = wparam.0 & 0xFFFF;
            if id == LIST_ID && code == LBN_SELCHANGE_CODE {
                let row = INSTANCE.with(|i| i.get()).and_then(|o| unsafe { (*o).selected_row() });
                signal(Signal::Submit { row, shift: false });
            } else if id == 1 && code == EN_CHANGE_CODE {
                signal(Signal::Changed);
            }
            LRESULT(0)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let (field, field_color, ink) = BRUSHES.with(|b| b.get());
            if field == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let hdc = HDC(wparam.0 as _);
            unsafe {
                SetTextColor(hdc, COLORREF(ink));
                SetBkColor(hdc, COLORREF(field_color));
            }
            LRESULT(field)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let list = GetDlgItem(Some(hwnd), LIST_ID as i32).unwrap_or_default();
                let list_shown = IsWindowVisible(list).as_bool();
                let edit_h = INSTANCE.with(|i| i.get()).map_or(rect.bottom, |o| (*o).edit_height);
                if let Some((palette, scale)) = LOOK.with(|l| l.get()) {
                    SURFACES.with(|s| {
                        s.borrow_mut().0.paint(hdc, rect, |g| paint_panel(g, rect, edit_h, list_shown, &palette, scale))
                    });
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if item.itemID == u32::MAX {
                return LRESULT(1);
            }
            let text = item_text(item.hwndItem, item.itemID as usize);
            let picked = item.itemState.0 & ODS_SELECTED.0 != 0;
            let hovered = HOVERED.with(|h| h.get()) == Some(item.itemID as usize);
            if let Some((palette, scale)) = LOOK.with(|l| l.get()) {
                SURFACES.with(|s| {
                    s.borrow_mut().1.paint(item.hDC, item.rcItem, |g| {
                        paint_row(g, item.rcItem.into(), &text, picked, hovered, &palette, scale)
                    })
                });
            }
            LRESULT(1)
        }
        // Losing activation does not close the field: the main window taking
        // focus back (first launch, Alt+Tab, a click on the title bar) would
        // otherwise close it. It closes on Esc, on Enter, or on a click on
        // the page or the tab strip, which the app handles.
        WM_ACTIVATE => LRESULT(0),
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The panel's ground: the field's colour, the search glass, and a hairline
/// between the field and the list.
fn paint_panel(g: &Painter, rect: RECT, edit_h: i32, list_shown: bool, pal: &Palette, scale: f32) {
    g.fill(rect.into(), pal.field);
    let column = GLASS_COLUMN * scale;
    let (cx, cy) = (column * 0.55 - scale, edit_h as f32 / 2.0 - scale);
    let r = 5.5 * scale;
    let ink = Color::from(pal.ink_muted);
    g.arc(cx, cy, r, 0.0, std::f32::consts::TAU - 0.01, 1.5 * scale, ink);
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    let from = (cx + d, cy + d);
    g.line(from, (from.0 + 4.0 * scale, from.1 + 4.0 * scale), 1.6 * scale, ink);
    if list_shown {
        let y = edit_h as f32;
        g.fill(RectF::new(0.0, y, rect.right as f32, y + 1.0), pal.line);
    }
}

/// One row: a soft ground when picked (or, quieter, under the pointer), the
/// site's mark, the title in ink and what follows it dimmed.
fn paint_row(g: &Painter, rect: RectF, text: &str, picked: bool, hovered: bool, pal: &Palette, scale: f32) {
    g.fill(rect, pal.field);
    let ground = rect.inset(0.0, 1.0);
    if picked {
        g.round(ground, size::RADIUS * scale, pal.selection);
    } else if hovered {
        g.round(ground, size::RADIUS * scale, pal.row_hover);
    }
    let parts = RowParts::of(text);
    let x = rect.left + 10.0 * scale;
    let (_, cy) = rect.center();
    if let Some(initial) = parts.mark {
        let m = size::MARK * scale;
        let mark = RectF::new(x, cy - m / 2.0, x + m, cy + m / 2.0);
        crate::tabbar::draw_mark(g, mark, initial, false, false, pal, scale);
    }
    let x = x + (size::MARK + 10.0) * scale;
    let right = rect.right - 12.0 * scale;
    let title_font = Font::new(size::TEXT * scale, size::REGULAR, Align::Leading);
    let detail_font = Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Leading);
    if parts.detail.is_empty() {
        g.text(parts.title, RectF::new(x, rect.top, right, rect.bottom), title_font, pal.ink);
        return;
    }
    // The title takes what it needs, up to two thirds of the row; the
    // detail what is left.
    let room = right - x;
    let title_w = g.measure(parts.title, title_font).min(room * 0.66);
    g.text(parts.title, RectF::new(x, rect.top, x + title_w + 1.0, rect.bottom), title_font, pal.ink);
    let detail_x = x + title_w + 10.0 * scale;
    if detail_x < right {
        g.text(&parts.detail, RectF::new(detail_x, rect.top, right, rect.bottom), detail_font, pal.ink_muted);
    }
}

/// How a row's label splits for drawing. The app writes labels as
/// "title    address    more" (four spaces between parts), the shape the
/// script control and the tests read back unchanged.
#[derive(Debug, PartialEq)]
struct RowParts<'a> {
    title: &'a str,
    detail: String,
    mark: Option<char>,
}

impl<'a> RowParts<'a> {
    fn of(label: &'a str) -> Self {
        let mut parts = label.split("    ").map(str::trim).filter(|p| !p.is_empty());
        let title = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        let detail = rest.join(" · ");
        // The site's letter, from the address when the row has one.
        let mark = rest.first().filter(|a| a.contains('.') && !a.contains(' ')).and_then(|address| {
            let host = address.trim_start_matches("www.");
            host.chars().find(|c| c.is_alphanumeric())
        });
        Self { title, detail, mark }
    }
}

fn item_text(list: HWND, index: usize) -> String {
    unsafe {
        let len = SendMessageW(list, LB_GETTEXTLEN, Some(WPARAM(index)), None).0;
        if len <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; len as usize + 1];
        SendMessageW(list, LB_GETTEXT, Some(WPARAM(index)), Some(LPARAM(buffer.as_mut_ptr() as isize)));
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

/// Tracks the row under the pointer for its quieter ground.
unsafe extern "system" fn list_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let found = unsafe { SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lparam)).0 as u32 };
            // The high word says the point is past every row.
            let row = ((found >> 16) == 0).then_some((found & 0xFFFF) as usize);
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            set_hovered(hwnd, row);
        }
        WM_MOUSELEAVE => set_hovered(hwnd, None),
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn set_hovered(list: HWND, row: Option<usize>) {
    let before = HOVERED.with(|h| h.replace(row));
    if before == row {
        return;
    }
    for index in [before, row].into_iter().flatten() {
        let mut rect = RECT::default();
        unsafe {
            SendMessageW(list, LB_GETITEMRECT, Some(WPARAM(index)), Some(LPARAM(&mut rect as *mut RECT as isize)));
            let _ = InvalidateRect(Some(list), Some(&rect), false);
        }
    }
}

