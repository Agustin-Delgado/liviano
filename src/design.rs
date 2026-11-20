//! Colours, in light/dark pairs, and the system theme. Every colour the
//! interface draws comes from here.

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
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Behind the tabs and the empty page.
    pub chrome: Rgb,
    pub tab_active: Rgb,
    pub tab_hover: Rgb,
    pub ink: Rgb,
    pub ink_muted: Rgb,
    /// Asleep tabs: no web view behind them.
    pub ink_asleep: Rgb,
    pub accent: Rgb,
    pub field: Rgb,
    pub line: Rgb,
    /// Behind an active private tab, instead of `tab_active`.
    pub private_tab: Rgb,
    /// The stripe every private tab carries.
    pub private_mark: Rgb,
}

const LIGHT: Palette = Palette {
    dark: false,
    chrome: Rgb(0xEE, 0xEE, 0xEC),
    tab_active: Rgb(0xFF, 0xFF, 0xFF),
    tab_hover: Rgb(0xE2, 0xE2, 0xDF),
    ink: Rgb(0x1F, 0x1F, 0x1E),
    ink_muted: Rgb(0x5C, 0x5C, 0x59),
    ink_asleep: Rgb(0x8E, 0x8E, 0x8A),
    accent: Rgb(0x2F, 0x6F, 0xEB),
    field: Rgb(0xFF, 0xFF, 0xFF),
    line: Rgb(0xD4, 0xD4, 0xD0),
    private_tab: Rgb(0xEF, 0xEA, 0xFB),
    private_mark: Rgb(0x75, 0x4C, 0xD4),
};

const DARK: Palette = Palette {
    dark: true,
    chrome: Rgb(0x1E, 0x1E, 0x1F),
    tab_active: Rgb(0x2E, 0x2E, 0x30),
    tab_hover: Rgb(0x28, 0x28, 0x2A),
    ink: Rgb(0xEC, 0xEC, 0xEA),
    ink_muted: Rgb(0xA6, 0xA6, 0xA2),
    ink_asleep: Rgb(0x72, 0x72, 0x6F),
    accent: Rgb(0x6B, 0x9B, 0xFF),
    field: Rgb(0x2A, 0x2A, 0x2C),
    line: Rgb(0x3A, 0x3A, 0x3C),
    private_tab: Rgb(0x35, 0x2E, 0x48),
    private_mark: Rgb(0xA6, 0x8A, 0xFF),
};

pub fn palette(theme: Theme) -> Palette {
    let dark = match theme {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => system_prefers_dark(),
    };
    if dark { DARK } else { LIGHT }
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

