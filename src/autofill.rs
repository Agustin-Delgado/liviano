//! The saved accounts of a site, in a small list under the login field that
//! was clicked, as a browser's autofill does. The list never takes focus:
//! the caret stays in the page, so typing goes on there and narrows the
//! list by user name. The page script sends the arrow keys, Enter and Esc
//! here while the list shows. A click or Enter fills at once.
//!
//! The model (which rows show, which one is picked) and the placement are
//! pure and tested; the window only draws what it is given and says which
//! row was clicked.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint, GetMonitorInfoW,
            InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, PAINTSTRUCT, SelectObject,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

use crate::{
    anim::specs,
    design::{Palette, Rgb, dwm, size},
    fade,
    gfx::{Align, Color, Font, RectF, Surface},
    tabbar::Rect,
};

/// The label of the last row.
pub const MANAGE: &str = "Manage passwords…";

/// A row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// An account, by its index in the list the menu was made with.
    Account(usize),
    Manage,
}

/// Whether a user name is offered for what is typed in the field: the
/// start of it, without regard to case. An empty field offers all.
pub fn matches(name: &str, typed: &str) -> bool {
    let typed = typed.trim();
    typed.is_empty() || name.to_lowercase().starts_with(&typed.to_lowercase())
}

/// Accounts shown at once; more scroll (the last row stays put).
pub const VISIBLE: usize = 6;

/// The accounts on offer and the row picked with the arrow keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    names: Vec<String>,
    /// Indices into `names` of the accounts that match what is typed.
    shown: Vec<usize>,
    /// Index into `rows()`.
    picked: Option<usize>,
    /// The first account in sight, when there are more than `VISIBLE`.
    top: usize,
}

impl Menu {
    /// None when no account matches what the field holds.
    pub fn new(names: Vec<String>, typed: &str) -> Option<Self> {
        let mut menu = Self { names, shown: Vec::new(), picked: None, top: 0 };
        menu.filter(typed).then_some(menu)
    }

    /// The rows top to bottom: the matching accounts, then "Manage passwords…".
    pub fn rows(&self) -> Vec<Row> {
        self.shown.iter().map(|&i| Row::Account(i)).chain(std::iter::once(Row::Manage)).collect()
    }

    pub fn name(&self, index: usize) -> Option<&str> {
        self.names.get(index).map(String::as_str)
    }

    /// What a row says.
    pub fn label(&self, row: Row) -> &str {
        match row {
            Row::Account(i) => self.name(i).map_or("", |n| if n.is_empty() { "(no user name)" } else { n }),
            Row::Manage => MANAGE,
        }
    }

    /// Narrows the list to what is typed. False when no account is left:
    /// the list closes then. The picked account stays picked while it shows.
    pub fn filter(&mut self, typed: &str) -> bool {
        let picked = self.picked_row();
        self.shown = (0..self.names.len()).filter(|&i| matches(&self.names[i], typed)).collect();
        let rows = self.rows();
        self.picked = picked.and_then(|p| rows.iter().position(|r| *r == p));
        self.top = 0;
        self.keep_in_sight();
        !self.shown.is_empty()
    }

    /// The rows in sight, by index into `rows()`: up to `VISIBLE` accounts
    /// from `top`, then "Manage passwords…".
    pub fn in_sight(&self) -> Vec<usize> {
        let end = (self.top + VISIBLE).min(self.shown.len());
        (self.top..end).chain(std::iter::once(self.shown.len())).collect()
    }

    /// Scrolls the accounts by `lines` (down when positive).
    pub fn scroll(&mut self, lines: i32) {
        let last = self.shown.len().saturating_sub(VISIBLE) as i64;
        self.top = (self.top as i64 + lines as i64).clamp(0, last) as usize;
    }

    fn keep_in_sight(&mut self) {
        let Some(p) = self.picked.filter(|&p| p < self.shown.len()) else { return };
        if p < self.top {
            self.top = p;
        } else if p >= self.top + VISIBLE {
            self.top = p + 1 - VISIBLE;
        }
    }

    /// Down (or up) one row, round from the end to the start. With nothing
    /// picked, down picks the first row and up the last.
    pub fn step(&mut self, down: bool) {
        let count = self.rows().len();
        self.picked = Some(match self.picked {
            None if down => 0,
            None => count - 1,
            Some(p) if down => (p + 1) % count,
            Some(p) => (p + count - 1) % count,
        });
        self.keep_in_sight();
    }

    /// The row picked, by its index.
    pub fn picked(&self) -> Option<usize> {
        self.picked
    }

    pub fn picked_row(&self) -> Option<Row> {
        self.picked.and_then(|p| self.rows().get(p).copied())
    }
}

// ---------- measures ----------

/// Before the DPI scale.
const ROW: i32 = 32;
const HEADER: i32 = 28;
const PAD: i32 = 4;
const SEPARATOR: i32 = 9;
const MIN_WIDTH: i32 = 220;
const MAX_WIDTH: i32 = 420;
/// How far the list stands off the field.
const GAP: i32 = 4;

fn px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

/// Where each part of the list goes, in its own pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub header: Rect,
    /// One per row of `Menu::rows`, the last one "Manage passwords…".
    pub rows: Vec<Rect>,
    /// The hairline above the last row.
    pub separator: i32,
}

/// The list for `count` rows (the last is "Manage passwords…"), as wide as
/// the field within bounds.
pub fn layout(scale: f32, count: usize, field_width: i32) -> Layout {
    let count = count.max(1);
    let width = field_width.clamp(px(MIN_WIDTH, scale), px(MAX_WIDTH, scale));
    let pad = px(PAD, scale);
    let header = Rect::new(pad, pad, width - pad, pad + px(HEADER, scale));
    let mut y = header.bottom;
    let mut rows = Vec::with_capacity(count);
    let mut separator = y;
    for i in 0..count {
        if i + 1 == count {
            separator = y + px(SEPARATOR, scale) / 2;
            y += px(SEPARATOR, scale);
        }
        rows.push(Rect::new(pad, y, width - pad, y + px(ROW, scale)));
        y += px(ROW, scale);
    }
    Layout { width, height: y + pad, header, rows, separator }
}

/// The row under a point of the list.
pub fn row_at(layout: &Layout, x: i32, y: i32) -> Option<usize> {
    layout.rows.iter().position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

/// The list's top-left corner on screen for a field at `field` (screen
/// pixels): under it, its left edges lined up; over it when there is no
/// room below; always inside the work area.
pub fn place(field: Rect, size: (i32, i32), work: Rect, gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let below = field.bottom + gap;
    let y = if below + h <= work.bottom || field.top - gap - h < work.top { below } else { field.top - gap - h };
    let x = field.left.min(work.right - w).max(work.left);
    (x, y.min(work.bottom - h).max(work.top))
}

// ---------- the window ----------

/// What the person did on the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// A row clicked, by its index into `Menu::rows`.
    Click(usize),
    /// The wheel turned, in rows (down when positive).
    Wheel(i32),
}

struct Shown {
    header: String,
    /// The rows drawn, by index into `Menu::rows`, and what each says.
    slots: Vec<usize>,
    labels: Vec<String>,
    picked: Option<usize>,
    palette: Palette,
    scale: f32,
    layout: Layout,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static HOVER: Cell<Option<usize>> = const { Cell::new(None) };
    static PRESSED: Cell<Option<usize>> = const { Cell::new(None) };
    static HANDLER: Cell<Option<fn(Input)>> = const { Cell::new(None) };
    static SURFACE: RefCell<Surface> = RefCell::new(Surface::default());
}

pub struct Dropdown {
    pub hwnd: HWND,
    /// The border colour and theme told to DWM: told again only on a change.
    look: Cell<Option<(u32, bool)>>,
}

impl Dropdown {
    pub fn create(owner: HWND, on_input: fn(Input)) -> windows::core::Result<Self> {
        HANDLER.with(|h| h.set(Some(on_input)));
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(proc),
                hInstance: instance,
                lpszClassName: w!("NeaAutofill"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            // Never activated: the page keeps the keyboard and its caret.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("NeaAutofill"),
                None,
                WS_POPUP,
                0,
                0,
                10,
                10,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            dwm::set(hwnd, dwm::CORNERS, dwm::ROUND);
            fade::prepare(hwnd, 255);
            warm_up();
            Ok(Self { hwnd, look: Cell::new(None) })
        }
    }

    /// Shows (or redraws) the list under `field` (screen pixels).
    pub fn show(&self, menu: &Menu, header: &str, field: Rect, palette: &Palette, scale: f32) {
        let _doing = crate::diag::doing(&"autofill:show", 0);
        let rows = menu.rows();
        let slots = menu.in_sight();
        let layout = layout(scale, slots.len(), field.right - field.left);
        let size = (layout.width, layout.height);
        let (x, y) = place(field, size, work_area(field), px(GAP, scale));
        let labels = slots.iter().map(|&i| rows.get(i).map_or("", |r| menu.label(*r)).to_string()).collect();
        let picked = menu.picked().and_then(|p| slots.iter().position(|&i| i == p));
        let look = (palette.line.colorref().0, palette.dark);
        if self.look.replace(Some(look)) != Some(look) {
            dwm::set(self.hwnd, dwm::BORDER_COLOR, look.0);
            dwm::set(self.hwnd, dwm::DARK_MODE, look.1 as u32);
        }
        SHOWN.with(|s| {
            *s.borrow_mut() =
                Some(Shown { header: header.to_string(), slots, labels, picked, palette: *palette, scale, layout })
        });
        fade::fade_in(self.hwnd, specs::MENU_IN, 255, 0);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), x, y, size.0, size.1, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Fades it out. What it showed was only names.
    pub fn hide(&self) {
        HOVER.with(|h| h.set(None));
        PRESSED.with(|p| p.set(None));
        fade::fade_out(self.hwnd, specs::MENU_OUT, 0, None);
    }
}

/// Draws once off screen, so the first list shown doesn't pay for the
/// Direct2D target and the text formats (about 14 ms).
fn warm_up() {
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bitmap = CreateCompatibleBitmap(dc, 8, 8);
        let old = SelectObject(dc, bitmap.into());
        let rect = RECT { left: 0, top: 0, right: 8, bottom: 8 };
        SURFACE.with(|s| {
            s.borrow_mut().paint(dc, rect, |g| {
                let r: RectF = rect.into();
                g.round(r, size::RADIUS_CONTROL, Color::from(Rgb(0, 0, 0)));
                g.text("a", r, Font::new(size::CAPTION, size::REGULAR, Align::Leading), Rgb(0, 0, 0));
                g.text("a", r, Font::new(size::TEXT, size::REGULAR, Align::Leading), Rgb(0, 0, 0));
            })
        });
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
    }
}

fn work_area(near: Rect) -> Rect {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: near.left, y: near.top }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            return Rect::new(r.left, r.top, r.right, r.bottom);
        }
    }
    Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
}

fn paint(hwnd: HWND) {
    let _doing = crate::diag::doing(&"autofill:paint", 0);
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        let hover = HOVER.with(|h| h.get());
        SHOWN.with(|s| {
            let shown = s.borrow();
            let Some(shown) = shown.as_ref() else { return };
            let (pal, scale, layout) = (&shown.palette, shown.scale, &shown.layout);
            SURFACE.with(|surface| {
                surface.borrow_mut().paint(hdc, rect, |g| {
                    g.fill(rect.into(), pal.field);
                    let inset = 10.0 * scale;
                    let caption = Font::new(size::CAPTION * scale, size::REGULAR, Align::Leading);
                    let h: RectF = layout.header.into();
                    g.text(
                        &shown.header,
                        RectF::new(h.left + inset, h.top, h.right - inset, h.bottom),
                        caption,
                        pal.ink_muted,
                    );
                    let line = layout.separator as f32;
                    g.fill(RectF::new(h.left, line, h.right, line + scale.max(1.0)), pal.line);
                    let text = Font::new(size::TEXT * scale, size::REGULAR, Align::Leading);
                    for (i, (r, label)) in layout.rows.iter().zip(&shown.labels).enumerate() {
                        let r: RectF = (*r).into();
                        if shown.picked == Some(i) {
                            g.round(r, size::RADIUS_CONTROL * scale, pal.selection);
                        } else if hover == Some(i) {
                            g.round(r, size::RADIUS_CONTROL * scale, Color::from(pal.row_hover));
                        }
                        let manage = i + 1 == shown.labels.len();
                        let ink = if manage { pal.ink_quiet } else { pal.ink };
                        g.text(label, RectF::new(r.left + inset, r.top, r.right - inset, r.bottom), text, ink);
                    }
                })
            });
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

/// The slot (row drawn) under a point of the client area.
fn slot_under(lparam: LPARAM) -> Option<usize> {
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
    SHOWN.with(|s| s.borrow().as_ref().and_then(|s| row_at(&s.layout, x, y)))
}

fn send(input: Input) {
    if let Some(handler) = HANDLER.with(|h| h.get()) {
        handler(input);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let row = slot_under(lparam);
            if HOVER.with(|h| h.replace(row)) != row {
                unsafe {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            HOVER.with(|h| h.set(None));
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            PRESSED.with(|p| p.set(slot_under(lparam)));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let slot = slot_under(lparam);
            if slot.is_some() && PRESSED.with(|p| p.take()) == slot {
                let row = SHOWN.with(|s| s.borrow().as_ref().and_then(|s| slot.and_then(|i| s.slots.get(i).copied())));
                if let Some(row) = row {
                    send(Input::Click(row));
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            send(Input::Wheel(-delta.signum()));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/autofill.rs"]
#[rustfmt::skip]
mod tests;
