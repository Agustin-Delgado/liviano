//! "¿Guardar contraseña…?": a small floating card over the page's top right
//! corner, with three buttons (drawn by `widgets`, the main one in ink). It never takes focus by itself (the person
//! may be typing on the page that follows a login) and exists only while an
//! offer stands: it is created for it and destroyed after.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HBRUSH, HDC, HFONT, SetBkColor, SetTextColor},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Controls::DRAWITEMSTRUCT, WindowsAndMessaging::*},
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    anim::{TOAST_RISE, specs},
    design::{Palette, dwm},
    fade,
    gfx::{Color, Surface},
    widgets::{self, ButtonKind},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Save,
    Never,
    NotNow,
}

// Static control styles (SystemServices is not among the features).
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;
const BUTTONS: [(i32, Choice); 3] = [(1, Choice::Save), (2, Choice::Never), (3, Choice::NotNow)];
/// Size of the bubble at 100 % scale.
const WIDTH: f32 = 430.0;
const HEIGHT: f32 = 86.0;

thread_local! {
    static HANDLER: Cell<Option<fn(Choice)>> = const { Cell::new(None) };
    /// (background brush, background colour, ink)
    static COLORS: Cell<(isize, u32, u32)> = const { Cell::new((0, 0, 0)) };
    static LOOK: Cell<Option<(Palette, f32)>> = const { Cell::new(None) };
    static SURFACE: RefCell<Surface> = RefCell::new(Surface::default());
}

pub struct VaultBar {
    popup: HWND,
    label: HWND,
    buttons: Vec<HWND>,
    scale: f32,
}

fn px(v: f32, scale: f32) -> i32 {
    (v * scale).round() as i32
}

impl VaultBar {
    /// `labels`: the question, then the three buttons' text.
    pub fn create(
        owner: HWND,
        palette: &Palette,
        font: HFONT,
        scale: f32,
        texts: [&str; 4],
        handler: fn(Choice),
    ) -> Option<Self> {
        HANDLER.with(|h| h.set(Some(handler)));
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(bar_proc),
                hInstance: instance,
                lpszClassName: w!("NeaVaultBar"),
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                ..Default::default()
            };
            RegisterClassW(&class);
            let popup = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("NeaVaultBar"),
                w!("Contraseña — Nea"),
                WS_POPUP | WS_CLIPCHILDREN,
                0,
                0,
                px(WIDTH, scale),
                px(HEIGHT, scale),
                Some(owner),
                None,
                Some(instance),
                None,
            )
            .ok()?;
            let make = |class: PCWSTR, text: &str, style: u32, id: i32| {
                let child = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class,
                    &HSTRING::from(text),
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                    0,
                    0,
                    10,
                    10,
                    Some(popup),
                    Some(HMENU(id as isize as _)),
                    Some(instance),
                    None,
                )
                .unwrap_or_default();
                SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
                child
            };
            let label = make(w!("STATIC"), texts[0], SS_NOPREFIX | SS_ENDELLIPSIS, -1);
            let buttons: Vec<HWND> = BUTTONS
                .iter()
                .zip(&texts[1..])
                .map(|((id, _), text)| make(w!("BUTTON"), text, (WS_TABSTOP.0) | BS_OWNERDRAW as u32, *id))
                .collect();
            for button in &buttons {
                widgets::track_hover(*button);
            }
            dwm::set(popup, dwm::CORNERS, dwm::ROUND);
            fade::prepare(popup, 255);
            LOOK.with(|l| l.set(Some((*palette, scale))));
            let bar = Self { popup, label, buttons, scale };
            bar.set_palette(palette);
            bar.layout();
            Some(bar)
        }
    }

    fn layout(&self) {
        let s = self.scale;
        let (width, height) = (px(WIDTH, s), px(HEIGHT, s));
        let pad = px(14.0, s);
        let button_h = px(30.0, s);
        unsafe {
            let _ =
                SetWindowPos(self.label, None, pad, pad, width - 2 * pad, px(22.0, s), SWP_NOZORDER | SWP_NOACTIVATE);
            // Right-aligned, the main action first.
            let widths = [92.0, 150.0, 92.0].map(|w| px(w, s));
            let gap = px(8.0, s);
            let total: i32 = widths.iter().sum::<i32>() + gap * 2;
            let mut x = width - pad - total;
            for (button, w) in self.buttons.iter().zip(widths) {
                let _ =
                    SetWindowPos(*button, None, x, height - pad - button_h, w, button_h, SWP_NOZORDER | SWP_NOACTIVATE);
                x += w + gap;
            }
        }
    }

    pub fn set_palette(&self, palette: &Palette) {
        COLORS.with(|c| {
            let (old, _, _) = c.get();
            unsafe {
                if old != 0 {
                    let _ = DeleteObject(HBRUSH(old as _).into());
                }
                let brush = CreateSolidBrush(palette.field.colorref());
                c.set((brush.0 as isize, palette.field.colorref().0, palette.ink.colorref().0));
            }
        });
        LOOK.with(|l| l.set(Some((*palette, self.scale))));
        dwm::set(self.popup, dwm::BORDER_COLOR, palette.line.colorref().0);
        dwm::set(self.popup, dwm::DARK_MODE, palette.dark as u32);
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                Some(self.popup),
                None,
                None,
                windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                    | windows::Win32::Graphics::Gdi::RDW_ERASE
                    | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN,
            );
        }
    }

    /// Shows the bubble with its top right corner at (right, top), screen
    /// coordinates, without activating it.
    /// Arriving, it comes down into place as a toast at the top does
    /// (6 px, on the toast's spring) while it fades in.
    pub fn place(&self, right: i32, top: i32) {
        let width = px(WIDTH, self.scale);
        let arriving = unsafe { !IsWindowVisible(self.popup).as_bool() } || fade::closing(self.popup);
        unsafe {
            let _ = SetWindowPos(
                self.popup,
                Some(HWND_TOP),
                right - width,
                top,
                width,
                px(HEIGHT, self.scale),
                SWP_NOACTIVATE,
            );
        }
        if arriving {
            fade::fade_in(self.popup, specs::TOAST_IN, 255, -(TOAST_RISE * self.scale).round() as i32);
        }
        unsafe {
            let _ = ShowWindow(self.popup, SW_SHOWNA);
        }
    }

    pub fn hide(&self) {
        fade::fade_out(self.popup, specs::TOAST_OUT, -(TOAST_RISE * self.scale).round() as i32, None);
    }
}

fn destroy(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

impl Drop for VaultBar {
    fn drop(&mut self) {
        // It leaves as a toast does, and is destroyed once it is gone.
        fade::fade_out(self.popup, specs::TOAST_OUT, -(TOAST_RISE * self.scale).round() as i32, Some(destroy));
        let (brush, _, _) = COLORS.with(|c| c.replace((0, 0, 0)));
        if brush != 0 {
            unsafe {
                let _ = DeleteObject(HBRUSH(brush as _).into());
            }
        }
    }
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            if let Some((_, choice)) = BUTTONS.iter().find(|(button, _)| *button == id)
                && let Some(handler) = HANDLER.with(|h| h.get())
            {
                handler(*choice);
            }
            LRESULT(0)
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some((palette, scale)) = LOOK.with(|l| l.get()) {
                let kind = if item.CtlID == 1 { ButtonKind::Primary } else { ButtonKind::Secondary };
                SURFACE.with(|s| {
                    widgets::draw_button_item(
                        &mut s.borrow_mut(),
                        item,
                        kind,
                        Color::from(palette.field),
                        &palette,
                        scale,
                    )
                });
            }
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let (brush, background, ink) = COLORS.with(|c| c.get());
            if brush == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let hdc = HDC(wparam.0 as _);
            unsafe {
                SetTextColor(hdc, COLORREF(ink));
                SetBkColor(hdc, COLORREF(background));
            }
            LRESULT(brush)
        }
        WM_ERASEBKGND => {
            let (brush, _, _) = COLORS.with(|c| c.get());
            if brush == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let mut rect = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut rect);
                FillRect(HDC(wparam.0 as _), &rect, HBRUSH(brush as _));
            }
            LRESULT(1)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
