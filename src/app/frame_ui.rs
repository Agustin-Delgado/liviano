//! The window without a title bar. Windows is told the client area reaches
//! the top edge (`WM_NCCALCSIZE`) and what each point is (`WM_NCHITTEST`):
//! the strip's empty space is the caption, the top edge resizes, and the
//! maximize button answers `HTMAXBUTTON` so Windows 11 offers its Snap
//! Layouts there. Minimize and close are plain client buttons; maximize is
//! non-client, so its hover and press arrive as `WM_NC*` messages.
//!
//! The left, right and bottom frames stay Windows' own (invisible resize
//! borders, shadow, rounded corners): only the top one is taken.

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::{
            Dwm::DwmExtendFrameIntoClientArea,
            Gdi::{InvalidateRect, ScreenToClient},
        },
        UI::{
            Controls::MARGINS,
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
            },
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};

use super::{App, wide, with_app, x_y};
use crate::{
    frame::{self, FrameHit, WindowButton},
    shortcuts::Command,
    tabbar::Hit,
};

/// "Tabs on the side / on top" in the window's system menu (right click
/// on the strip's empty space, Alt+Espacio). System command ids are below
/// 0xF000 with the low four bits clear.
const SC_TAB_LAYOUT: usize = 0x0110;
/// "Hide tabs / Show tabs", beside it.
const SC_SIDEBAR: usize = 0x0120;
/// `WM_NCMOUSELEAVE` (not in the bindings' WindowsAndMessaging set).
const WM_NCMOUSELEAVE: u32 = 0x02A2;

#[derive(Default)]
pub(super) struct FrameUi {
    /// A window button held down (by the mouse, until it is released).
    pub(super) pressed: Option<WindowButton>,
    /// Leaving the non-client maximize button is being watched.
    nc_tracking: bool,
    /// The window is in the background (its buttons go quieter).
    pub(super) inactive: bool,
}

/// Takes the title bar away and adds the layout entries to the system menu.
pub(super) fn install(hwnd: HWND, vertical: bool, collapsed: bool) {
    unsafe {
        // Nothing extended: the frame is only asked to keep its shadow and
        // corners, which it does with zero margins as long as the window
        // keeps its caption style.
        let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS::default());
        let menu = GetSystemMenu(hwnd, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, SC_TAB_LAYOUT, &wide(layout_label(vertical)));
        let _ = AppendMenuW(menu, MF_STRING, SC_SIDEBAR, &wide(sidebar_label(collapsed)));
        // The new frame takes effect with the next WM_NCCALCSIZE.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

pub(super) fn layout_label(vertical: bool) -> &'static str {
    if vertical { "Tabs on top" } else { "Tabs on the side" }
}

pub(super) fn sidebar_label(collapsed: bool) -> &'static str {
    if collapsed { "Show tabs" } else { "Hide tabs" }
}

/// The frame Windows puts around a maximized window, off the monitor.
fn frame_thickness(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

/// Maximized with its frame: a maximized window in full screen has no frame
/// off the screen to make up for (refer to `fullscreen_ui`).
fn is_maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() && GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0 }
}

fn ht(hit: FrameHit) -> u32 {
    match hit {
        FrameHit::Client | FrameHit::Minimize | FrameHit::Close => HTCLIENT,
        FrameHit::Caption => HTCAPTION,
        FrameHit::Top => HTTOP,
        FrameHit::TopLeft => HTTOPLEFT,
        FrameHit::TopRight => HTTOPRIGHT,
        FrameHit::Maximize => HTMAXBUTTON,
    }
}

/// The frame's messages; `None` lets the window proc go on with the rest.
pub(super) fn window_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => {
            // Posted by another program, lParam can point anywhere: then
            // not even the default handling reads it.
            let Some(params) = (unsafe { crate::msg_param::write::<NCCALCSIZE_PARAMS>(lparam) }) else {
                return Some(LRESULT(0));
            };
            let top = params.rgrc[0].top;
            // Windows works out the side and bottom frames; the top goes
            // back to the window's edge (past the off-screen frame when maximized).
            let result = default();
            params.rgrc[0].top = frame::client_top(top, is_maximized(hwnd), frame_thickness(hwnd));
            Some(result)
        }
        WM_NCHITTEST => {
            let native = default();
            if native.0 as u32 != HTCLIENT {
                return Some(native);
            }
            let (sx, sy) = x_y(lparam);
            let mut point = POINT { x: sx, y: sy };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut point);
            }
            let hit = with_app(|app| app.frame_hit(point.x, point.y)).unwrap_or(FrameHit::Client);
            Some(LRESULT(ht(hit) as isize))
        }
        WM_NCMOUSEMOVE => {
            let over = (wparam.0 as u32 == HTMAXBUTTON).then_some(WindowButton::Maximize);
            with_app(|app| app.nc_hover(over));
            if over.is_some() { Some(LRESULT(0)) } else { None }
        }
        WM_NCMOUSELEAVE => {
            with_app(|app| app.nc_leave());
            None
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam.0 as u32 == HTMAXBUTTON => {
            // Not passed on: Windows would draw its classic button there.
            with_app(|app| app.press_window_button(WindowButton::Maximize));
            Some(LRESULT(0))
        }
        WM_NCLBUTTONDOWN if wparam.0 as u32 == HTCAPTION => {
            // Grabbing the window puts the field away, as a click on the strip did.
            with_app(|app| {
                if app.mode.is_some() {
                    app.close_omnibar();
                }
            });
            None
        }
        WM_NCLBUTTONUP if wparam.0 as u32 == HTMAXBUTTON => {
            with_app(|app| app.release_nc_button(WindowButton::Maximize));
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let (x, y) = x_y(lparam);
            with_app(|app| app.release_window_button(x, y)).unwrap_or(false).then_some(LRESULT(0))
        }
        WM_SYSCOMMAND => {
            let command = match wparam.0 & 0xFFF0 {
                SC_TAB_LAYOUT => Command::ToggleTabLayout,
                SC_SIDEBAR => Command::ToggleSidebar,
                _ => return None,
            };
            with_app(|app| app.run(command));
            Some(LRESULT(0))
        }
        _ => None,
    }
}

impl App {
    pub(super) fn set_window_active(&mut self, active: bool) {
        if self.frame.inactive == !active {
            return;
        }
        self.frame.inactive = !active;
        self.invalidate_frame();
    }

    pub(super) fn is_maximized(&self) -> bool {
        is_maximized(self.hwnd)
    }

    fn frame_hit(&self, x: i32, y: i32) -> FrameHit {
        if self.fullscreen.on() {
            return FrameHit::Client;
        }
        let (width, _) = self.client_size();
        frame::hit(&self.layout, width, x, y, self.is_maximized(), self.scale)
    }

    /// The system menu's entries follow the layout they would change.
    pub(super) fn sync_frame_menu(&self) {
        unsafe {
            let menu = GetSystemMenu(self.hwnd, false);
            let _ = ModifyMenuW(
                menu,
                SC_TAB_LAYOUT as u32,
                MF_BYCOMMAND | MF_STRING,
                SC_TAB_LAYOUT,
                &wide(layout_label(self.vertical)),
            );
            let _ = ModifyMenuW(
                menu,
                SC_SIDEBAR as u32,
                MF_BYCOMMAND | MF_STRING,
                SC_SIDEBAR,
                &wide(sidebar_label(self.collapsed)),
            );
        }
    }

    /// Tabs on top or at the side, column shown or hidden: from the
    /// shortcut, the menus or Settings, which all show the result.
    pub(super) fn set_tab_layout(&mut self, vertical: bool, collapsed: bool) {
        if (self.vertical, self.collapsed) == (vertical, collapsed) {
            return;
        }
        self.vertical = vertical;
        self.collapsed = collapsed;
        self.session_dirty = true;
        self.relayout();
        self.sync_frame_menu();
        crate::settings_window::set_tab_layout(vertical, collapsed);
    }

    /// Right click on the column's empty space (in a row of tabs that space
    /// is the caption, and Windows opens the system menu there instead,
    /// which carries the same entries).
    pub(super) fn layout_menu(&mut self, x: i32, y: i32) {
        const TAB_LAYOUT: usize = 1;
        const SIDEBAR: usize = 2;
        const NEW_TAB: usize = 3;
        const RESTART: usize = 4;
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, NEW_TAB, &wide("New tab"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, TAB_LAYOUT, &wide(layout_label(self.vertical)));
            let _ = AppendMenuW(menu, MF_STRING, SIDEBAR, &wide(sidebar_label(self.collapsed)));
            if let Some(version) = &self.update {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, RESTART, &wide(&super::update_ui::restart_label(version)));
            }
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
            TAB_LAYOUT => self.run(Command::ToggleTabLayout),
            SIDEBAR => self.run(Command::ToggleSidebar),
            NEW_TAB => self.run(Command::NewTab),
            RESTART => self.restart_to_update(),
            _ => {}
        }
    }

    fn invalidate_frame(&self) {
        for r in self.layout.frame.buttons {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&r.win()), false);
            }
        }
    }

    pub(super) fn press_window_button(&mut self, button: WindowButton) {
        self.frame.pressed = Some(button);
        if button != WindowButton::Maximize {
            unsafe {
                SetCapture(self.hwnd);
            }
        }
        self.invalidate_frame();
    }

    /// The mouse came up in the client area: a held button acts if the
    /// pointer is still on it. True when a button was held.
    fn release_window_button(&mut self, x: i32, y: i32) -> bool {
        let Some(button) = self.frame.pressed.take() else { return false };
        if button != WindowButton::Maximize {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if self.layout.hit(x, y) == Some(Hit::Window(button)) {
            self.window_button(button);
        }
        self.invalidate_frame();
        true
    }

    fn release_nc_button(&mut self, button: WindowButton) {
        if self.frame.pressed.take() == Some(button) {
            self.window_button(button);
        }
        self.invalidate_frame();
    }

    /// Through WM_SYSCOMMAND, as the title bar's own buttons do: Windows
    /// animates them and the restore size is kept.
    fn window_button(&self, button: WindowButton) {
        let command = match button {
            WindowButton::Minimize => SC_MINIMIZE,
            WindowButton::Maximize if self.is_maximized() => SC_RESTORE,
            WindowButton::Maximize => SC_MAXIMIZE,
            WindowButton::Close => SC_CLOSE,
        };
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_SYSCOMMAND, WPARAM(command as usize), LPARAM(0));
        }
    }

    fn nc_hover(&mut self, over: Option<WindowButton>) {
        if over.is_some() && !self.frame.nc_tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.frame.nc_tracking = true;
        }
        self.set_hover(over.map(Hit::Window));
    }

    fn nc_leave(&mut self) {
        self.frame.nc_tracking = false;
        if self.frame.pressed == Some(WindowButton::Maximize) {
            self.frame.pressed = None;
        }
        if self.hover == Some(Hit::Window(WindowButton::Maximize)) {
            self.set_hover(None);
        }
    }
}
