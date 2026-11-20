//! The single field: a small popup over the page with a native edit control
//! and, under it, a native list of suggestions. It serves three modes —
//! address, tab switcher, find in page and the bookmarks, history and
//! downloads panels — so there is one field, not six.

use std::cell::Cell;

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH, HDC, HFONT, SetBkColor, SetTextColor},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{EM_GETSEL, EM_SETCUEBANNER, EM_SETSEL},
            Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_UP},
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::design::Palette;

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
    static BRUSHES: Cell<(isize, isize, u32, u32)> = const { Cell::new((0, 0, 0, 0)) };
}

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
                WS_POPUP | WS_BORDER,
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
                WS_CHILD | WS_VSCROLL | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
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
            Ok(Self {
                popup,
                edit,
                list,
                rows: 0,
                row_height: 24,
                edit_height: 34,
                visible_rows: usize::MAX,
                rows_deletable: false,
            })
        }
    }

    pub fn set_font(&mut self, font: HFONT, scale: f32) {
        unsafe {
            for hwnd in [self.edit, self.list] {
                SendMessageW(hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
            let item = SendMessageW(self.list, LB_GETITEMHEIGHT, Some(WPARAM(0)), None).0 as i32;
            self.row_height = item.max(1);
        }
        self.edit_height = (34.0 * scale).round() as i32;
    }

    pub fn set_palette(&self, palette: &Palette) {
        BRUSHES.with(|b| {
            let (old_field, old_chrome, _, _) = b.get();
            unsafe {
                if old_field != 0 {
                    let _ = DeleteObject(HBRUSH(old_field as _).into());
                    let _ = DeleteObject(HBRUSH(old_chrome as _).into());
                }
                let field = CreateSolidBrush(palette.field.colorref());
                let chrome = CreateSolidBrush(palette.tab_active.colorref());
                b.set((field.0 as isize, chrome.0 as isize, palette.ink.colorref().0, palette.field.colorref().0));
            }
        });
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.popup), None, true);
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
        let list_h = shown as i32 * self.row_height + if shown > 0 { 6 } else { 0 };
        unsafe {
            let _ = SetWindowPos(
                self.popup,
                Some(HWND_TOP),
                left,
                top,
                width,
                self.edit_height + list_h + 2,
                SWP_NOACTIVATE,
            );
            let pad = (self.edit_height as f32 * 0.3) as i32;
            let text_h = (self.row_height + 4).min(self.edit_height);
            let _ = SetWindowPos(
                self.edit,
                None,
                pad,
                (self.edit_height - text_h) / 2,
                width - 2 * pad - 2,
                text_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = SetWindowPos(self.list, None, 0, self.edit_height, width - 2, list_h, SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = ShowWindow(self.list, if self.rows > 0 { SW_SHOWNA } else { SW_HIDE });
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
            let (field, _, ink, field_color) = BRUSHES.with(|b| b.get());
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
        WM_ERASEBKGND => {
            let (field, _, _, _) = BRUSHES.with(|b| b.get());
            if field != 0 {
                let mut rect = RECT::default();
                unsafe {
                    let _ = GetClientRect(hwnd, &mut rect);
                    windows::Win32::Graphics::Gdi::FillRect(HDC(wparam.0 as _), &rect, HBRUSH(field as _));
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
