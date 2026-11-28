//! A select: an owner-drawn button that shows one value of a short list and
//! opens the app's own menu to pick another. It answers the combo box
//! messages a window and its test scripts already send (CB_ADDSTRING,
//! CB_GETCOUNT, CB_SETCURSEL, CB_GETCURSEL) and tells its parent of a pick
//! with CBN_SELCHANGE, so code written for a combo box keeps working. The
//! system's combo box ignored the palette: its closed field and its list
//! stayed the system's grey and blue.

use std::{cell::RefCell, collections::HashMap};

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::InvalidateRect,
    UI::{
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::{
            CB_ADDSTRING, CB_ERR, CB_GETCOUNT, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE,
            GetDlgCtrlID, GetParent, GetWindowRect, SendMessageW, SetWindowTextW, WM_COMMAND, WM_NCDESTROY,
        },
    },
};
use windows::core::HSTRING;

use crate::menu::{self, Anchor, HAlign, Menu, VAlign};

/// The values of one select and the one it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    pub items: Vec<String>,
    pub current: Option<usize>,
}

impl Choices {
    /// Adds a value at the end and says at which index.
    pub fn add(&mut self, item: &str) -> usize {
        self.items.push(item.to_string());
        self.items.len() - 1
    }

    /// Shows the value at `index`; an index past the end shows none.
    pub fn set(&mut self, index: usize) -> Option<usize> {
        self.current = (index < self.items.len()).then_some(index);
        self.current
    }

    /// What the select shows.
    pub fn text(&self) -> &str {
        self.current.and_then(|i| self.items.get(i)).map_or("", String::as_str)
    }

    /// The menu of the values, the shown one checked. Item ids are the
    /// indexes plus one: zero is "nothing picked".
    pub fn menu(&self) -> Menu {
        let mut menu = Menu::new();
        for (i, item) in self.items.iter().enumerate() {
            menu.check(i + 1, item, self.current == Some(i));
        }
        menu
    }
}

thread_local! {
    static CHOICES: RefCell<HashMap<isize, Choices>> = RefCell::new(HashMap::new());
}

const SUBCLASS_ID: usize = 0x5E1C;

/// Makes the owner-drawn button `button` a select. Call once after creating it.
pub fn attach(button: HWND) {
    CHOICES.with(|c| c.borrow_mut().insert(button.0 as isize, Choices::default()));
    unsafe {
        let _ = SetWindowSubclass(button, Some(select_proc), SUBCLASS_ID, 0);
    }
}

/// Whether `hwnd` is a select.
pub fn is_select(hwnd: HWND) -> bool {
    CHOICES.with(|c| c.borrow().contains_key(&(hwnd.0 as isize)))
}

/// The values of a select.
pub fn choices(hwnd: HWND) -> Choices {
    CHOICES.with(|c| c.borrow().get(&(hwnd.0 as isize)).cloned().unwrap_or_default())
}

fn with_choices<R>(hwnd: HWND, f: impl FnOnce(&mut Choices) -> R) -> Option<R> {
    CHOICES.with(|c| c.borrow_mut().get_mut(&(hwnd.0 as isize)).map(f))
}

/// Shows the text of the value picked, as the button's own text (what a
/// screen reader reads).
fn show(hwnd: HWND) {
    let text = choices(hwnd).text().to_string();
    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(text));
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

unsafe extern "system" fn select_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    let err = LRESULT(CB_ERR as isize);
    match msg {
        CB_ADDSTRING => {
            // The text comes from this thread's own window code: a select
            // is never filled from another program.
            let text = unsafe { windows::core::PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
            with_choices(hwnd, |c| LRESULT(c.add(&text) as isize)).unwrap_or(err)
        }
        CB_RESETCONTENT => {
            with_choices(hwnd, |c| *c = Choices::default());
            show(hwnd);
            LRESULT(0)
        }
        CB_GETCOUNT => with_choices(hwnd, |c| LRESULT(c.items.len() as isize)).unwrap_or(err),
        CB_GETCURSEL => {
            with_choices(hwnd, |c| LRESULT(c.current.map_or(CB_ERR as isize, |i| i as isize))).unwrap_or(err)
        }
        CB_SETCURSEL => {
            let shown = with_choices(hwnd, |c| c.set(wparam.0)).flatten();
            show(hwnd);
            LRESULT(shown.map_or(CB_ERR as isize, |i| i as isize))
        }
        WM_NCDESTROY => {
            CHOICES.with(|c| c.borrow_mut().remove(&(hwnd.0 as isize)));
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// Opens the menu of `select` under it and waits. A new pick is shown and
/// told to the parent as a combo box tells it (CBN_SELCHANGE). Call it with
/// no state of the window borrowed: the menu's loop dispatches messages.
pub fn pick(select: HWND) {
    let all = choices(select);
    if all.items.is_empty() {
        return;
    }
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(select, &mut rect);
    }
    let at = Anchor::aligned(rect.left, rect.bottom + 4, HAlign::Left, VAlign::Top);
    let chosen = menu::track(select, all.menu(), at);
    if chosen == 0 || all.current == Some(chosen - 1) {
        return;
    }
    unsafe {
        SendMessageW(select, CB_SETCURSEL, Some(WPARAM(chosen - 1)), None);
        let id = GetDlgCtrlID(select) as usize & 0xFFFF;
        if let Ok(parent) = GetParent(select) {
            let w = WPARAM(id | ((CBN_SELCHANGE as usize) << 16));
            SendMessageW(parent, WM_COMMAND, Some(w), Some(LPARAM(select.0 as isize)));
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/select.rs"]
#[rustfmt::skip]
mod tests;
