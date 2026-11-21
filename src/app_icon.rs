//! The application icon (resource 1, `assets/logo/liviano.ico`, embedded by
//! `build.rs`).
//!
//! A class `hIcon` alone is loaded once at the system DPI and shrunk for the
//! title bar, so the hand-drawn small sizes in the .ico would never be used.
//! Each window instead gets both icons at the sizes its monitor asks for, and
//! asks again when it moves to a monitor with another DPI.

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            WindowsAndMessaging::{
                DestroyIcon, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON,
                SM_CXSMICON, SYSTEM_METRICS_INDEX, SendMessageW, WM_SETICON,
            },
        },
    },
    core::PCWSTR,
};

/// The resource id of the icon, as in `liviano.rc`.
pub const ICON_ID: u16 = 1;

/// Gives `hwnd` the big (Alt+Tab, taskbar) and small (title bar) icons at
/// its current DPI. Call after creating the window and on `WM_DPICHANGED`.
pub fn apply(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let Some(icon) = load(metric, dpi) else { continue };
            let previous = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
            // Only this module sets window icons, so whatever comes back was
            // loaded here (not shared) and is ours to free.
            if previous.0 != 0 {
                let _ = DestroyIcon(HICON(previous.0 as _));
            }
        }
    }
}

fn load(metric: SYSTEM_METRICS_INDEX, dpi: u32) -> Option<HICON> {
    unsafe {
        let size = GetSystemMetricsForDpi(metric, dpi);
        let instance = GetModuleHandleW(None).ok()?;
        let handle =
            LoadImageW(Some(instance.into()), PCWSTR(ICON_ID as usize as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .ok()?;
        Some(HICON(handle.0))
    }
}
