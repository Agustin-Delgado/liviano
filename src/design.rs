//! The design tokens: colours in light/dark pairs, sizes, radii and the
//! face. Every colour and measure the interface draws comes from here.
//!
//! The look follows Search (the macOS browser Liviano is the Windows twin
//! of): the page is the ground and the browser gets out of its way. Almost
//! everything is a grey a few steps from the ground; the only ink that
//! isn't grey is the private tab's violet and the loading ring's accent.

use windows::{
    Win32::{
        Foundation::COLORREF,
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    },
    core::w,
};

use crate::settings::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn colorref(self) -> COLORREF {
        COLORREF(self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16)
    }

    /// A grey `level` of the way from black to white.
    const fn grey(level: u8) -> Self {
        Self(level, level, level)
    }

    /// `t` of the way from `self` to `other`.
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Behind the tabs and the empty page: a touch off the page's white.
    pub chrome: Rgb,
    /// The live tab on the chrome.
    pub tab_active: Rgb,
    /// A tab under the pointer.
    pub tab_hover: Rgb,
    /// The resting square of a pinned tab, and a tab shrunk to its mark.
    pub tab_tile: Rgb,
    /// The live pinned square, among the tiles.
    pub pin_live: Rgb,
    pub ink: Rgb,
    pub ink_muted: Rgb,
    /// Asleep tabs (no web view behind them), placeholders, "New tab".
    pub ink_asleep: Rgb,
    /// The loading ring and the caret's focus line; used sparingly.
    pub accent: Rgb,
    /// The page's own ground: the field, the list, panels, dialogs.
    pub field: Rgb,
    /// The picked row of a list on `field`.
    pub selection: Rgb,
    /// A row under the pointer on `field`.
    pub row_hover: Rgb,
    /// Separators and outlines.
    pub line: Rgb,
    /// Behind an active private tab, instead of `tab_active`.
    pub private_tab: Rgb,
    /// The mark every private tab carries.
    pub private_mark: Rgb,
}

const LIGHT: Palette = Palette {
    dark: false,
    chrome: Rgb::grey(0xF4),
    tab_active: Rgb::grey(0xE4),
    tab_hover: Rgb::grey(0xEA),
    tab_tile: Rgb::grey(0xEC),
    pin_live: Rgb::grey(0xDB),
    ink: Rgb::grey(0x17),
    ink_muted: Rgb::grey(0x76),
    ink_asleep: Rgb::grey(0xA3),
    accent: Rgb(0x2F, 0x6F, 0xEB),
    field: Rgb::grey(0xFF),
    selection: Rgb::grey(0xEF),
    row_hover: Rgb::grey(0xF6),
    line: Rgb::grey(0xE6),
    private_tab: Rgb(0xEC, 0xE7, 0xF8),
    private_mark: Rgb(0x75, 0x4C, 0xD4),
};

const DARK: Palette = Palette {
    dark: true,
    chrome: Rgb::grey(0x14),
    tab_active: Rgb::grey(0x29),
    tab_hover: Rgb::grey(0x21),
    tab_tile: Rgb::grey(0x1D),
    pin_live: Rgb::grey(0x31),
    ink: Rgb::grey(0xED),
    ink_muted: Rgb::grey(0x94),
    ink_asleep: Rgb::grey(0x60),
    accent: Rgb(0x6B, 0x9B, 0xFF),
    field: Rgb::grey(0x1C),
    selection: Rgb::grey(0x2D),
    row_hover: Rgb::grey(0x25),
    line: Rgb::grey(0x30),
    private_tab: Rgb(0x2E, 0x28, 0x3F),
    private_mark: Rgb(0xA6, 0x8A, 0xFF),
};

/// Measures in device-independent pixels (multiplied by the window's DPI
/// scale where they are used).
pub mod size {
    /// Tabs, rows of the list, buttons: the one corner the interface wears.
    pub const RADIUS: f32 = 8.0;
    /// The small square that stands for a site (its first letter).
    pub const MARK: f32 = 16.0;
    pub const MARK_RADIUS: f32 = 4.0;
    /// Type sizes.
    pub const TEXT: f32 = 13.0;
    pub const TEXT_SMALL: f32 = 12.0;
    pub const TEXT_FIELD: f32 = 15.0;
    pub const TEXT_MARK: f32 = 9.5;
    /// Weights: regular, and medium for what is live.
    pub const REGULAR: u16 = 400;
    pub const MEDIUM: u16 = 500;
    pub const SEMIBOLD: u16 = 600;
}

/// How long the interface takes to move: short, and over before it shows.
pub mod motion {
    /// A hover or press fading in or out.
    pub const QUICK_MS: u64 = 140;
    /// The live tab's ground sliding to the newly chosen one; tabs sliding
    /// into their new places after a reorder.
    pub const SETTLE_MS: u64 = 180;
    /// Frame interval while something moves (60 Hz).
    pub const FRAME_MS: u32 = 16;
    /// Frame interval of the loading ring: it turns, it doesn't need 60 Hz.
    pub const SPIN_FRAME_MS: u32 = 50;

    /// Ease-out cubic: quick to leave, gentle to arrive.
    pub fn ease_out(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }
}

pub fn palette(theme: Theme) -> Palette {
    let dark = match theme {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => system_prefers_dark(),
    };
    if dark { DARK } else { LIGHT }
}

/// The frame attributes Windows 11 lets a window set on itself.
pub mod dwm {
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute},
    };

    pub const DARK_MODE: i32 = 20;
    pub const CORNERS: i32 = 33;
    pub const BORDER_COLOR: i32 = 34;
    pub const CAPTION_COLOR: i32 = 35;
    pub const TEXT_COLOR: i32 = 36;
    pub const ROUND: u32 = 2;

    /// Older Windows ignore what they don't know; nothing to check.
    pub fn set(hwnd: HWND, attribute: i32, value: u32) {
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(attribute),
                &value as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

/// Windows' own "Animation effects" switch (Accessibility > Visual effects).
/// Off, the interface changes at once instead of moving.
pub fn animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut on = windows::core::BOOL(1);
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    read.is_err() || on.as_bool()
}

/// Windows stores the app theme as AppsUseLightTheme (0 = dark).
pub fn system_prefers_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

