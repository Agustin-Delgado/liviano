//! The design tokens: colours in light/dark pairs, the type scale, the
//! named corners and lifts. Every colour and measure the interface draws
//! comes from here.
//!
//! The language is a web design system's (its brand tokens and component
//! recipes; `docs/diseno.md` has the table):
//! neutral greys on a shell, the page's ground a step lighter, one indigo
//! for what is live or chosen, seven type steps and nothing heavier than
//! medium. Where Search's layout ideas don't clash with it (tabs as soft
//! grounds on the strip, the private tab's violet) they stay. The design system
//! has no dark theme; the dark palette keeps the same steps and roles,
//! mirrored, with the indigo lifted to read on a dark ground.

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
    pub const fn grey(level: u8) -> Self {
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
    /// The shell: behind the tabs and around the page (`shell`).
    pub chrome: Rgb,
    /// The live tab: a card on the shell (`canvas`, with the card's ring).
    pub tab_active: Rgb,
    /// A tab or a ghost button under the pointer (`hover`).
    pub tab_hover: Rgb,
    /// The resting square of a pinned tab (`shell-hover`).
    pub tab_tile: Rgb,
    /// A control held down (`line`: one step past the hover).
    pub pin_live: Rgb,
    /// Text (`ink`).
    pub ink: Rgb,
    /// Secondary text, idle icons (`ink-muted`).
    pub ink_muted: Rgb,
    /// Labels a step under the ink (`ink-quiet`).
    pub ink_quiet: Rgb,
    /// Asleep tabs, placeholders, hints (`ink-subtle`).
    pub ink_asleep: Rgb,
    /// What is live or chosen: the loading ring, focus, the split's focus
    /// line, a checked box (`brand`).
    pub accent: Rgb,
    /// The accent under the pointer (`brand-hover`).
    pub accent_hover: Rgb,
    /// Ink on the accent (`on-brand`).
    pub on_accent: Rgb,
    /// The page's own ground: the field, the list, panels, dialogs (`canvas`).
    pub field: Rgb,
    /// The picked row of a list (`hover`, as a menu's highlighted item).
    pub selection: Rgb,
    /// A row under the pointer (`row-hover`).
    pub row_hover: Rgb,
    /// Separators and hairlines (`line`).
    pub line: Rgb,
    /// Outlines that have to read (`line-strong`).
    pub line_strong: Rgb,
    /// The tooltip's pill (`ink`) and its text (`canvas`).
    pub tooltip: Rgb,
    pub tooltip_ink: Rgb,
    /// Behind an active private tab, instead of `tab_active`.
    pub private_tab: Rgb,
    /// The mark every private tab carries.
    pub private_mark: Rgb,
}

const LIGHT: Palette = Palette {
    dark: false,
    chrome: Rgb::grey(0xF5),             // shell oklch(0.97 0 0)
    tab_active: Rgb::grey(0xFA),         // canvas oklch(0.985 0 0)
    tab_hover: Rgb::grey(0xF0),          // hover oklch(0.955 0 0)
    tab_tile: Rgb::grey(0xEF),           // shell-hover oklch(0.952 0 0)
    pin_live: Rgb::grey(0xE8),           // line oklch(0.93 0 0)
    ink: Rgb::grey(0x17),                // ink oklch(0.205 0 0)
    ink_muted: Rgb::grey(0x69),          // ink-muted oklch(0.52 0 0)
    ink_quiet: Rgb::grey(0x52),          // ink-quiet oklch(0.439 0 0)
    ink_asleep: Rgb::grey(0x92),         // ink-subtle oklch(0.66 0 0)
    accent: Rgb(0x64, 0x68, 0xF0),       // brand oklch(0.59 0.2 277)
    accent_hover: Rgb(0x57, 0x56, 0xE4), // brand-hover oklch(0.54 0.21 277)
    on_accent: Rgb::grey(0xFC),          // on-brand oklch(0.99 0 0)
    field: Rgb::grey(0xFA),              // canvas
    selection: Rgb::grey(0xF0),          // hover
    row_hover: Rgb::grey(0xF3),          // row-hover oklch(0.965 0 0)
    line: Rgb::grey(0xE8),               // line
    line_strong: Rgb::grey(0xD7),        // line-strong oklch(0.88 0 0)
    tooltip: Rgb::grey(0x17),            // ink
    tooltip_ink: Rgb::grey(0xFA),        // canvas
    private_tab: Rgb(0xEF, 0xEB, 0xFF),
    private_mark: Rgb(0x7E, 0x4E, 0xD7),
};

const DARK: Palette = Palette {
    dark: true,
    chrome: Rgb::grey(0x14),
    tab_active: Rgb::grey(0x2A),
    tab_hover: Rgb::grey(0x22),
    tab_tile: Rgb::grey(0x1F),
    pin_live: Rgb::grey(0x30),
    ink: Rgb::grey(0xED),
    ink_muted: Rgb::grey(0x9A),
    ink_quiet: Rgb::grey(0xB4),
    ink_asleep: Rgb::grey(0x6E),
    accent: Rgb(0x81, 0x8C, 0xF9),       // brand lifted: oklch(0.68 0.16 277)
    accent_hover: Rgb(0x91, 0x9D, 0xFF), // oklch(0.73 0.15 277)
    on_accent: Rgb::grey(0x14),
    field: Rgb::grey(0x1C),
    selection: Rgb::grey(0x2A),
    row_hover: Rgb::grey(0x24),
    line: Rgb::grey(0x2E),
    line_strong: Rgb::grey(0x3C),
    tooltip: Rgb::grey(0xED),
    tooltip_ink: Rgb::grey(0x17),
    private_tab: Rgb(0x30, 0x28, 0x44),
    private_mark: Rgb(0xAC, 0x8F, 0xF8),
};

/// A tab group's colour: its mark (the dot, the line that joins its tabs,
/// the label's faint tint) and its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    pub fill: Rgb,
    /// The group's name written in its colour, on the shell or on the
    /// faint tint of the label: the fill a fifth (a quarter in the dark) of
    /// the way to the ink.
    pub ink: Rgb,
}

/// The group palette: the design system's tones (`--color-tone-*`): the
/// tone's `ink` step on the light shell, its `solid` step on the dark one.
/// Every fill stands 3:1 or more off its shell, and the name (the fill a
/// fifth or a quarter of the way to the ink) reads at 4.5:1 or more on the
/// shell and on the label's tint.
pub fn group_tone(color: crate::tab_groups::GroupColor, dark: bool) -> Tone {
    use crate::tab_groups::GroupColor::*;
    let (light, dark_fill) = match color {
        Grey => (Rgb::grey(0x55), Rgb::grey(0x9E)), // gray-ink 0.45 0 0 / gray-solid 0.7 0 0
        Blue => (Rgb(0x27, 0x50, 0xC7), Rgb(0x28, 0x85, 0xEF)), // blue 0.48 0.19 265 / 0.62 0.18 255
        Red => (Rgb(0xC2, 0x15, 0x2F), Rgb(0xF1, 0x44, 0x45)), // red 0.52 0.2 22 / 0.64 0.21 25
        Yellow => (Rgb(0x91, 0x6B, 0x00), Rgb(0xF3, 0xB0, 0x1D)), // amber 0.55 0.13 88 / 0.8 0.16 80
        Green => (Rgb(0x29, 0x76, 0x13), Rgb(0x2E, 0xB4, 0x5C)), // green 0.5 0.15 140 / 0.68 0.17 150
        Pink => (Rgb(0xB1, 0x2B, 0x6E), Rgb(0xEA, 0x50, 0x98)), // pink 0.52 0.18 355 / 0.66 0.2 355
        Purple => (Rgb(0x76, 0x3E, 0xBD), Rgb(0x9F, 0x61, 0xF2)), // violet 0.5 0.19 300 / 0.63 0.21 300
        Cyan => (Rgb(0x00, 0x7B, 0xB2), Rgb(0x00, 0xB2, 0xD1)), // cyan 0.55 0.13 235 / 0.7 0.13 215
        Orange => (Rgb(0xBE, 0x4C, 0x00), Rgb(0xF4, 0x74, 0x1E)), // orange 0.56 0.17 50 / 0.7 0.18 48
    };
    if dark {
        Tone { fill: dark_fill, ink: dark_fill.mix(Rgb::grey(0xED), 0.25) }
    } else {
        Tone { fill: light, ink: light.mix(Rgb::grey(0x17), 0.2) }
    }
}

/// How much of a group's colour its label's ground takes: a tint, never a
/// saturated pill.
pub fn group_tint(dark: bool) -> f32 {
    if dark { 0.14 } else { 0.10 }
}

/// Measures in device-independent pixels (multiplied by the window's DPI
/// scale where they are used).
#[allow(dead_code)] // the scale is kept whole
pub mod size {
    /// The named corners (brand.css `--radius-*`).
    pub const RADIUS_MARK: f32 = 2.0;
    pub const RADIUS_CHIP: f32 = 4.0;
    pub const RADIUS_CONTROL: f32 = 6.0;
    pub const RADIUS_FIELD: f32 = 8.0;
    pub const RADIUS_PANEL: f32 = 12.0;
    pub const RADIUS_DIALOG: f32 = 16.0;
    /// Tabs, rows, buttons: a field's corner, as a Button's `rounded-field`.
    pub const RADIUS: f32 = RADIUS_FIELD;
    /// The small square that stands for a site (its first letter).
    pub const MARK: f32 = 16.0;
    pub const MARK_RADIUS: f32 = RADIUS_CHIP;
    /// Type steps in use (`text-caption`, `-small`, `-body`, `-emphasis`;
    /// heading 16, title 20 and display 28 complete the scale).
    pub const CAPTION: f32 = 11.0;
    pub const TEXT_SMALL: f32 = 12.0;
    pub const TEXT: f32 = 13.0;
    /// The field's own text: the emphasis step, a size up from the body.
    pub const TEXT_FIELD: f32 = 14.0;
    /// A site's initial in its mark (the `initials` size).
    pub const TEXT_MARK: f32 = 10.0;
    /// Weights: regular, and medium for what is live. Nothing heavier.
    pub const REGULAR: u16 = 400;
    pub const MEDIUM: u16 = 500;
    /// A button's height (`h-6.5`).
    pub const BUTTON: f32 = 26.0;
    /// The tooltip's padding (8 × 2) around caption text, and how far a
    /// tooltip stands off its trigger.
    pub const TOOLTIP_PAD_X: f32 = 8.0;
    pub const TOOLTIP_PAD_Y: f32 = 2.0;
    pub const OFFSET: f32 = 8.0;
}

/// The lifts, as alphas of black over what they stand on (brand.css
/// `--shadow-*`).
pub mod lift {
    /// `shadow-card`'s 1 px ring and its drop (0 2px 6px -2px).
    pub const CARD_RING: f32 = 0.08;
    pub const CARD_DROP: f32 = 0.12;
    /// `shadow-focus`'s halo, 4 px of brand at 0.18 around a 1 px line.
    pub const FOCUS_HALO: f32 = 0.18;
    /// `shadow-pop`, what floats over the page (the field): a 1 px ring and
    /// a long soft drop (0 12px 32px -8px).
    pub const POP_RING: f32 = 0.08;
    pub const POP_DROP: f32 = 0.18;
}

/// How long the interface takes to move. The movements themselves (curves
/// and durations from the design system) are in `anim::specs`.
pub mod motion {
    /// Frame interval of the loading ring: it turns, it doesn't need 60 Hz.
    pub const SPIN_FRAME_MS: u32 = 50;
    /// One turn of the loading ring (`animate-spin`: 1 s linear).
    pub const SPIN_TURN_S: f32 = 1.0;
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
    pub const DO_NOT_ROUND: u32 = 1;
    /// DWMWA_COLOR_NONE: no border at all.
    pub const COLOR_NONE: u32 = 0xFFFF_FFFE;

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

/// The interface's scale for `hwnd`: its monitor's DPI over 96. The tests
/// can ask for another one with `NEA_SCALE` (a percentage: 125, 150),
/// to see a layout at a scale the screen they run on doesn't have.
pub fn window_scale(hwnd: windows::Win32::Foundation::HWND) -> f32 {
    let forced =
        std::env::var("NEA_SCALE").ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|p| (100..=300).contains(p));
    match forced {
        Some(percent) => percent as f32 / 100.0,
        None => (unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }) as f32 / 96.0,
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
