//! Settings (Ctrl+,): one small native window with plain controls — no web
//! view for the interface. The controls are the system's, framed in the
//! app's look: the page's ground, edits and lists sitting in soft grey wells
//! instead of a sunken edge, and buttons drawn by `widgets`. It edits a copy of the settings and hands every
//! committed change to the app, which applies it at once and writes
//! settings.json. Everything it holds (window, fonts, brushes) is freed when
//! it closes.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HDC, HFONT, MapWindowPoints, PAINTSTRUCT,
            RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, SetBkColor, SetTextColor,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_SELECTED,
                ODT_BUTTON, SetWindowTheme,
            },
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            Input::KeyboardAndMouse::{
                GetFocus, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, SetFocus,
                UnregisterHotKey,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{
    design::{Palette, dwm, size},
    gfx::{Align, Color, Font, RectF, Surface},
    settings::{self, FieldPosition, Settings, Theme},
    shortcuts::{self, Command},
    updater,
    widgets::{self, ButtonKind},
};

const ID_THEME: i32 = 101;
const ID_SEARCH: i32 = 102;
const ID_SUSPEND: i32 = 103;
const ID_DISCARD: i32 = 104;
const ID_PRESSURE: i32 = 105;
const ID_AWAKE_LIST: i32 = 106;
const ID_AWAKE_EDIT: i32 = 107;
const ID_AWAKE_ADD: i32 = 108;
const ID_AWAKE_REMOVE: i32 = 109;
const ID_KEYS_LIST: i32 = 110;
const ID_KEY_EDIT: i32 = 111;
const ID_KEY_ASSIGN: i32 = 112;
const ID_KEY_RESET: i32 = 113;
const ID_CLOSE: i32 = 114;
const ID_SPLIT: i32 = 115;
const ID_SPLIT_LABEL: i32 = 116;
const ID_UPDATES: i32 = 117;
const ID_UPDATES_LABEL: i32 = 118;
const ID_BENCH: i32 = 119;
const ID_PASSWORDS_OFFER: i32 = 120;
const ID_PASSWORDS_IMPORT: i32 = 121;
const ID_PASSWORDS_LIST: i32 = 122;
const ID_EXT_LIST: i32 = 123;
const ID_EXT_TOGGLE: i32 = 124;
const ID_EXT_PIN: i32 = 125;
const ID_EXT_RELOAD: i32 = 126;
const ID_EXT_REMOVE: i32 = 127;
const ID_EXT_LOAD: i32 = 128;
const ID_BENCH_LABEL: i32 = 129;
const ID_TRACKERS: i32 = 130;
const ID_TRACKERS_LABEL: i32 = 131;
const ID_TAB_LAYOUT: i32 = 132;
const ID_COLLAPSED: i32 = 133;
const ID_PREVIEW: i32 = 135;
const ID_PREVIEW_LABEL: i32 = 136;
const ID_COLLAPSED_LABEL: i32 = 134;
const ID_FIELD_POSITION: i32 = 137;
const ID_EXT_NOTE: i32 = 138;
const ID_STATUS: i32 = 139;
const ID_VERSION: i32 = 140;

const BN_CLICKED_CODE: u32 = 0;
// Static control styles (their constants live behind another `windows` feature).
const SS_NOTIFY: u32 = 0x0100;
const SS_ENDELLIPSIS: u32 = 0x4000;
const CLIENT_WIDTH: i32 = 780;
const CLIENT_HEIGHT: i32 = 898;
/// Where the tabs go: the first thing in Settings, since the shortcut alone
/// went unnoticed (and another program may hold it).
const TAB_LAYOUTS: [(bool, &str); 2] = [(false, "On top"), (true, "On the side")];
/// Where the field floats over the page.
const FIELD_POSITIONS: [(FieldPosition, &str); 3] =
    [(FieldPosition::Top, "Top"), (FieldPosition::Middle, "Middle"), (FieldPosition::Bottom, "Bottom")];
const THEMES: [(Theme, &str); 3] = [(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")];
const OFFER_CHOICES: [(bool, &str); 2] = [(true, "When I sign in"), (false, "Never")];

/// What the Extensiones section asks of the app (with the selected row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionCommand {
    Toggle,
    Pin,
    Reload,
    Remove,
    LoadUnpacked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Label,
    Edit,
    Combo,
    List,
    Button,
}

struct State {
    hwnd: HWND,
    settings: Settings,
    commands: Vec<Command>,
    children: Vec<(HWND, Kind)>,
    theme: HWND,
    search: HWND,
    suspend: HWND,
    discard: HWND,
    pressure: HWND,
    split: HWND,
    preview: HWND,
    awake_list: HWND,
    awake_edit: HWND,
    keys_list: HWND,
    key_edit: HWND,
    updates: HWND,
    version: HWND,
    bench: HWND,
    trackers: HWND,
    passwords_offer: HWND,
    tab_layout: HWND,
    collapsed: HWND,
    field_position: HWND,
    /// Shortcuts another program registered system-wide: they never reach us.
    taken: Vec<shortcuts::Chord>,
    /// "Import…" was pressed: its menu opens after the state is released.
    status_after_import: bool,
    status: HWND,
    ext_list: HWND,
    ext_note: HWND,
    font: HFONT,
    heading_font: HFONT,
    scale: f32,
}

/// Brushes and colours the window proc paints with. Kept apart from `State`
/// so painting never needs the state borrowed.
#[derive(Clone, Copy, Default)]
struct Colors {
    background: isize,
    field: isize,
    selection: isize,
    background_color: u32,
    field_color: u32,
    ink: u32,
}

/// Adds one child control: class, text, style, extended style, rect, id,
/// kind and whether it takes the heading font.
type AddChild<'a> = dyn FnMut(PCWSTR, &str, u32, WINDOW_EX_STYLE, (i32, i32, i32, i32), i32, Kind, bool) + 'a;
/// Who handles an extension command chosen in Settings.
type ExtensionHandler = fn(ExtensionCommand, Option<usize>);
/// Who applies the tab layout (vertical, collapsed).
type LayoutHandler = fn(bool, bool);

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static HANDLER: Cell<Option<fn(Settings)>> = const { Cell::new(None) };
    static CLOSED: Cell<Option<fn()>> = const { Cell::new(None) };
    static EXTENSION_HANDLER: Cell<Option<ExtensionHandler>> = const { Cell::new(None) };
    /// Who applies the tab layout (vertical, collapsed): the app, which keeps
    /// it in the session rather than in the settings.
    static LAYOUT_HANDLER: Cell<Option<LayoutHandler>> = const { Cell::new(None) };
    /// The layout in force, for the controls (also before the window opens).
    static TAB_LAYOUT: Cell<(bool, bool)> = const { Cell::new((false, false)) };
    static COLORS: Cell<Colors> = Cell::new(Colors::default());
    /// The palette and scale, and the wells: read while painting, which can
    /// happen while the state is borrowed.
    static LOOK: Cell<Option<(Palette, f32)>> = const { Cell::new(None) };
    static WELLS: RefCell<Vec<RECT>> = const { RefCell::new(Vec::new()) };
    /// The window's ground, its buttons, and the shortcuts list's rows.
    static SURFACES: RefCell<(Surface, Surface, Surface)> = RefCell::new(Default::default());
}

enum Outcome {
    Nothing,
    Changed,
    Close,
    /// Modal steps, run once the state is no longer borrowed.
    ImportPasswords,
    /// Handed to the app once the state is no longer borrowed (it calls back
    /// into `set_extensions`).
    Extension(ExtensionCommand, Option<usize>),
    /// A tab layout (vertical, collapsed) for the app, applied at once.
    Layout(bool, bool),
}

fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

fn make_font(scale: f32, weight: i32) -> HFONT {
    crate::app::make_font_weighted(size::TEXT, weight, scale)
}

fn text_of(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd) as usize;
        let mut buffer = vec![0u16; len + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer) as usize;
        String::from_utf16_lossy(&buffer[..copied])
    }
}

fn set_text(hwnd: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, &wide(text));
    }
}

fn list_selection(list: HWND) -> Option<usize> {
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, None, None).0 };
    (index >= 0).then_some(index as usize)
}

fn list_fill(list: HWND, rows: &[String], select: Option<usize>) {
    unsafe {
        let top = SendMessageW(list, LB_GETTOPINDEX, None, None).0;
        SendMessageW(list, LB_RESETCONTENT, None, None);
        for row in rows {
            let text = wide(row);
            SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        SendMessageW(list, LB_SETTOPINDEX, Some(WPARAM(top.max(0) as usize)), None);
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(select.unwrap_or(usize::MAX))), None);
    }
}

thread_local! {
    /// The window was made ahead of time and waits hidden: the settings
    /// (as JSON) and the palette (as text) it was filled with.
    static PREPARED: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    /// The palette the window is coloured with (`palette_key`).
    static PAINTED_WITH: RefCell<String> = const { RefCell::new(String::new()) };
}

fn settings_key(settings: &Settings) -> String {
    serde_json::to_string(settings).unwrap_or_default()
}

/// What tells two palettes apart.
pub(crate) fn palette_key(palette: &Palette) -> String {
    format!("{palette:?}")
}

/// Makes the window ahead of time, hidden, so that opening it later only
/// shows it: its sixty controls take more than 150 ms to make, which a
/// click must not wait for. The app calls this while the person is away,
/// again and again: each call is one step of a few milliseconds (refer to
/// `STEP_BUDGET_MS`), so a key or a click that comes meanwhile waits for one
/// step at most. True when nothing is left to do.
pub fn prepare(owner: HWND, current: &Settings, palette: &Palette, on_change: fn(Settings)) -> bool {
    let open = STATE.with(|s| s.try_borrow().map_or(true, |g| g.is_some()));
    if prepared() || (open && !building()) {
        return true;
    }
    HANDLER.with(|h| h.set(Some(on_change)));
    let _doing = crate::diag::doing(&"settings:prepare", 0);
    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
    let stale = BUILD.with(|b| b.borrow().as_ref().is_some_and(|b| (dpi as f32 / 96.0 - b.scale).abs() >= 0.01));
    if stale {
        // The screen's scale changed on the way: begin again at the new one.
        drop_build();
    }
    if !building() {
        let Ok(build) = begin(owner) else { return true };
        BUILD.with(|b| *b.borrow_mut() = Some(build));
        return false;
    }
    let Some(mut build) = BUILD.with(|b| b.borrow_mut().take()) else { return true };
    if !build.step(current, palette, true) {
        BUILD.with(|b| *b.borrow_mut() = Some(build));
        return false;
    }
    finish_prepared(current, palette);
    true
}

/// Whether a window made ahead is still being made.
pub fn building() -> bool {
    BUILD.with(|b| b.borrow().is_some())
}

/// Whether a window made ahead waits hidden.
pub fn prepared() -> bool {
    PREPARED.with(|p| p.borrow().is_some())
}

/// Lets go of a window made ahead that can no longer be used as it is
/// (the screen's scale changed).
fn discard_prepared() {
    if building() {
        drop_build();
        return;
    }
    PREPARED.with(|p| p.borrow_mut().take());
    let Some(state) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    unsafe {
        let _ = DestroyWindow(state.hwnd);
        let _ = DeleteObject(state.font.into());
        let _ = DeleteObject(state.heading_font.into());
    }
}

/// Where the window goes: centred on its owner, a third of the way down.
fn place(owner: HWND, hwnd: HWND) {
    unsafe {
        let (mut owner_rect, mut own) = (RECT::default(), RECT::default());
        let _ = GetWindowRect(owner, &mut owner_rect);
        let _ = GetWindowRect(hwnd, &mut own);
        let (width, height) = (own.right - own.left, own.bottom - own.top);
        let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
        let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 3;
        let _ = SetWindowPos(hwnd, None, x.max(0), y.max(0), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Opens the window, or brings it forward if it is already open.
pub fn open(owner: HWND, current: &Settings, palette: &Palette, on_change: fn(Settings)) {
    finish_build(current, palette);
    let prepared = PREPARED.with(|p| p.borrow_mut().take());
    let existing = STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| (st.hwnd, st.theme, st.scale))));
    if let Some((hwnd, first, scale)) = existing {
        let Some((settings_was, palette_was)) = prepared else {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                if crate::omnibar::owns_foreground() {
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            return;
        };
        let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
        if (dpi as f32 / 96.0 - scale).abs() < 0.01 {
            HANDLER.with(|h| h.set(Some(on_change)));
            if palette_was != palette_key(palette) {
                set_palette(palette);
            }
            if settings_was != settings_key(current) {
                with_state(|st| {
                    st.settings = current.clone();
                    fill_all(st);
                });
            }
            place(owner, hwnd);
            show(hwnd, first);
            return;
        }
        PREPARED.with(|p| *p.borrow_mut() = Some((settings_was, palette_was)));
        discard_prepared();
    }
    HANDLER.with(|h| h.set(Some(on_change)));
    {
        let _doing = crate::diag::doing(&"settings:create", 0);
        let Ok(mut build) = begin(owner) else { return };
        build.step(current, palette, false);
    }
    PAINTED_WITH.with(|p| *p.borrow_mut() = palette_key(palette));
    let Some((hwnd, first)) = STATE.with(|s| s.borrow().as_ref().map(|st| (st.hwnd, st.theme))) else { return };
    show(hwnd, first);
}

fn show(hwnd: HWND, first: HWND) {
    // A dialog appearing: 100 ms of fade.
    crate::fade::fade_in_window(hwnd, crate::anim::specs::DIALOG);
    unsafe {
        // Opened by a script while another program has the keyboard: shown
        // without taking it (see `omnibar::owns_foreground`).
        if crate::omnibar::owns_foreground() {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetFocus(Some(first));
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

/// The app changed a setting itself ("Keep site awake" from a tab):
/// the window shows it, so its next change doesn't undo it.
pub fn sync(current: &Settings) {
    with_state(|st| {
        st.settings = current.clone();
        fill_all(st);
    });
}

/// Who applies a tab layout picked here.
pub fn set_layout_handler(handler: fn(bool, bool)) {
    LAYOUT_HANDLER.with(|h| h.set(Some(handler)));
}

/// The app's tab layout changed (or is told for the first time): the
/// controls follow it.
pub fn set_tab_layout(vertical: bool, collapsed: bool) {
    TAB_LAYOUT.with(|l| l.set((vertical, collapsed)));
    with_state(|st| fill_tab_layout(st));
}

fn fill_tab_layout(st: &State) {
    let (vertical, collapsed) = TAB_LAYOUT.with(|l| l.get());
    let index = TAB_LAYOUTS.iter().position(|(v, _)| *v == vertical).unwrap_or(0);
    let check = if collapsed { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(st.tab_layout, CB_SETCURSEL, Some(WPARAM(index)), None);
        SendMessageW(st.collapsed, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
    }
}

/// Hands a layout picked here to the app (after the state is released: the
/// app calls back into `set_tab_layout`).
fn emit_layout(vertical: bool, collapsed: bool) {
    TAB_LAYOUT.with(|l| l.set((vertical, collapsed)));
    if let Some(handler) = LAYOUT_HANDLER.with(|h| h.get()) {
        handler(vertical, collapsed);
    }
}

/// The chords among `chords` that another program holds system-wide: asked
/// by trying to register each one for an instant. Only chords with Ctrl or
/// Alt are asked (a bare F-key may be reserved by the system).
fn taken_chords(hwnd: HWND, chords: &[shortcuts::Chord]) -> Vec<shortcuts::Chord> {
    const PROBE_ID: i32 = 0x4C56;
    let mut taken = Vec::new();
    for chord in chords.iter().filter(|c| c.ctrl || c.alt) {
        let mut mods = MOD_NOREPEAT;
        for (on, m) in [(chord.ctrl, MOD_CONTROL), (chord.shift, MOD_SHIFT), (chord.alt, MOD_ALT)] {
            if on {
                mods |= m;
            }
        }
        let mods: HOT_KEY_MODIFIERS = mods;
        unsafe {
            if RegisterHotKey(Some(hwnd), PROBE_ID, mods, chord.vk).is_ok() {
                let _ = UnregisterHotKey(Some(hwnd), PROBE_ID);
            } else if !taken.contains(chord) {
                taken.push(*chord);
            }
        }
    }
    taken
}

/// Who handles the Extensiones section's buttons (the app, which owns the list).
pub fn set_extension_handler(handler: fn(ExtensionCommand, Option<usize>)) {
    EXTENSION_HANDLER.with(|h| h.set(Some(handler)));
}

/// Shows the installed extensions and a line about what they cost.
pub fn set_extensions(rows: &[String], note: &str) {
    with_state(|st| {
        let selected = list_selection(st.ext_list).filter(|i| *i < rows.len());
        list_fill(st.ext_list, rows, selected.or(if rows.is_empty() { None } else { Some(0) }));
        set_text(st.ext_note, note);
    });
}

/// Keyboard handling of a dialog (Tab between controls, Enter, Esc) for the
/// window's messages. True when the message was for it and is handled.
pub fn pre_translate(msg: &MSG) -> bool {
    let hwnd = STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| st.hwnd)));
    let Some(hwnd) = hwnd else { return false };
    unsafe {
        if msg.hwnd != hwnd && !IsChild(hwnd, msg.hwnd).as_bool() {
            return false;
        }
        IsDialogMessageW(hwnd, msg).as_bool()
    }
}

/// A size or a coordinate at the scale `from`, at the scale `to`.
fn rescaled(v: i32, from: f32, to: f32) -> i32 {
    (v as f32 * to / from).round() as i32
}

/// The window has a new DPI: Windows moved it to a screen with another
/// scale, or the scale of its screen changed. Each control keeps its place
/// and size in DIPs and gets fonts at the new scale. The window is not made
/// again: a drag across two screens continues, and the focus and the text
/// typed stay.
fn rescale(dpi: u32, suggested: Option<RECT>) {
    let to = crate::design::dpi_scale(dpi.max(96));
    let Some((hwnd, children, from, old_font, old_heading)) = STATE.with(|s| {
        s.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|st| (st.hwnd, st.children.clone(), st.scale, st.font, st.heading_font)))
    }) else {
        return;
    };
    let font = make_font(to, 400);
    let heading_font = make_font(to, 500);
    let scale_rect = |r: RECT| RECT {
        left: rescaled(r.left, from, to),
        top: rescaled(r.top, from, to),
        right: rescaled(r.right, from, to),
        bottom: rescaled(r.bottom, from, to),
    };
    unsafe {
        for (child, kind) in &children {
            let mut rect = RECT::default();
            let _ = GetWindowRect(*child, &mut rect);
            if matches!(kind, Kind::Combo) {
                // Its height on screen is the closed field; the height it
                // was given is that of the open list.
                SendMessageW(*child, CB_GETDROPPEDCONTROLRECT, None, Some(LPARAM(&mut rect as *mut RECT as isize)));
            }
            let mut corners = [POINT { x: rect.left, y: rect.top }, POINT { x: rect.right, y: rect.bottom }];
            MapWindowPoints(None, Some(hwnd), &mut corners);
            let at =
                scale_rect(RECT { left: corners[0].x, top: corners[0].y, right: corners[1].x, bottom: corners[1].y });
            let heading = SendMessageW(*child, WM_GETFONT, None, None).0 == old_heading.0 as isize;
            let new = if heading { heading_font } else { font };
            SendMessageW(*child, WM_SETFONT, Some(WPARAM(new.0 as usize)), Some(LPARAM(0)));
            let _ = SetWindowPos(
                *child,
                None,
                at.left,
                at.top,
                at.right - at.left,
                at.bottom - at.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if let Some(r) = suggested {
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    WELLS.with(|w| w.borrow_mut().iter_mut().for_each(|well| *well = scale_rect(*well)));
    with_state(|st| {
        st.font = font;
        st.heading_font = heading_font;
        st.scale = to;
        unsafe {
            let row = (28.0 * to).round() as isize;
            SendMessageW(st.keys_list, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(row)));
        }
    });
    LOOK.with(|l| l.set(l.get().map(|(palette, _)| (palette, to))));
    unsafe {
        let _ = DeleteObject(old_font.into());
        let _ = DeleteObject(old_heading.into());
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

/// Re-colours the window (also the first time): the same pairs as the rest.
pub fn set_palette(palette: &Palette) {
    let Some((hwnd, children)) =
        STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| (st.hwnd, st.children.clone()))))
    else {
        return;
    };
    PAINTED_WITH.with(|p| *p.borrow_mut() = palette_key(palette));
    set_colors(palette);
    theme_children(&children, palette.dark);
    unsafe {
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

/// The brushes and the colours of the window and its caption.
fn set_colors(palette: &Palette) {
    let Some((hwnd, scale)) =
        STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| (st.hwnd, st.scale))))
    else {
        return;
    };
    let old = COLORS.with(|c| c.get());
    unsafe {
        for brush in [old.background, old.field, old.selection] {
            if brush != 0 {
                let _ = DeleteObject(HBRUSH(brush as _).into());
            }
        }
        COLORS.with(|c| {
            c.set(Colors {
                background: CreateSolidBrush(palette.field.colorref()).0 as isize,
                field: CreateSolidBrush(palette.selection.colorref()).0 as isize,
                selection: CreateSolidBrush(palette.selection.colorref()).0 as isize,
                background_color: palette.field.colorref().0,
                field_color: palette.selection.colorref().0,
                ink: palette.ink.colorref().0,
            })
        });
        LOOK.with(|l| l.set(Some((*palette, scale))));
        dwm::set(hwnd, dwm::DARK_MODE, palette.dark as u32);
        dwm::set(hwnd, dwm::CORNERS, dwm::ROUND);
        dwm::set(hwnd, dwm::CAPTION_COLOR, palette.field.colorref().0);
        dwm::set(hwnd, dwm::TEXT_COLOR, palette.ink_muted.colorref().0);
        dwm::set(hwnd, dwm::BORDER_COLOR, palette.line.colorref().0);
    }
}

/// The system theme of each control, light or dark.
fn theme_children(children: &[(HWND, Kind)], dark: bool) {
    unsafe {
        for &(child, kind) in children {
            let _ = match (dark, kind) {
                (_, Kind::Label) => Ok(()),
                (true, Kind::Edit | Kind::Combo) => SetWindowTheme(child, w!("DarkMode_CFD"), PCWSTR::null()),
                (true, _) => SetWindowTheme(child, w!("DarkMode_Explorer"), PCWSTR::null()),
                (false, Kind::Edit | Kind::Combo) => SetWindowTheme(child, PCWSTR::null(), PCWSTR::null()),
                (false, _) => SetWindowTheme(child, w!("Explorer"), PCWSTR::null()),
            };
        }
    }
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// One control of the window, made by a later step.
struct Spec {
    class: PCWSTR,
    text: String,
    style: u32,
    ex: WINDOW_EX_STYLE,
    rect: (i32, i32, i32, i32),
    id: i32,
    kind: Kind,
    heading: bool,
}

/// The controls of the window, in DIPs, in the order they are made (the
/// order of the Tab key).
fn controls() -> Vec<Spec> {
    let mut specs = Vec::new();
    let mut add = |class: PCWSTR,
                   text: &str,
                   style: u32,
                   ex: WINDOW_EX_STYLE,
                   rect: (i32, i32, i32, i32),
                   id: i32,
                   kind: Kind,
                   heading: bool| {
        specs.push(Spec { class, text: text.to_string(), style, ex, rect, id, kind, heading });
    };
    let edge = WS_EX_CLIENTEDGE;
    let none = WINDOW_EX_STYLE::default();
    let tab = WS_TABSTOP.0;
    let label = |add: &mut AddChild<'_>, text: &str, rect: (i32, i32, i32, i32)| {
        add(w!("STATIC"), text, 0, none, rect, -1, Kind::Label, false)
    };
    let heading = |add: &mut AddChild<'_>, text: &str, rect: (i32, i32, i32, i32)| {
        add(w!("STATIC"), text, 0, none, rect, -1, Kind::Label, true)
    };
    let edit_style = tab | ES_AUTOHSCROLL as u32;
    let number_style = edit_style | ES_NUMBER as u32;
    let button_style = tab | BS_OWNERDRAW as u32;

    // Left column: Tabs first (where they go is the first thing
    // asked for), then General.
    let l = 24;
    heading(&mut add, "Tabs", (l, 18, 340, 22));
    label(&mut add, "Position", (l, 52, 140, 22));
    add(
        w!("COMBOBOX"),
        "",
        tab | CBS_DROPDOWNLIST as u32,
        none,
        (l + 150, 48, 190, 200),
        ID_TAB_LAYOUT,
        Kind::Combo,
        false,
    );
    // The checkboxes have no text of their own: a themed checkbox ignores
    // the dark palette's ink, a label doesn't.
    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l, 86, 18, 18), ID_COLLAPSED, Kind::Button, false);
    add(w!("STATIC"), "Hide tabs", SS_NOTIFY, none, (l + 22, 84, 150, 22), ID_COLLAPSED_LABEL, Kind::Label, false);
    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l + 180, 86, 18, 18), ID_SPLIT, Kind::Button, false);
    add(w!("STATIC"), "Split view", SS_NOTIFY, none, (l + 202, 84, 138, 22), ID_SPLIT_LABEL, Kind::Label, false);

    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l, 112, 18, 18), ID_PREVIEW, Kind::Button, false);
    add(
        w!("STATIC"),
        "Show a page preview on hover",
        SS_NOTIFY,
        none,
        (l + 22, 110, 318, 22),
        ID_PREVIEW_LABEL,
        Kind::Label,
        false,
    );

    heading(&mut add, "General", (l, 146, 340, 22));
    label(&mut add, "Theme", (l, 180, 140, 22));
    add(
        w!("COMBOBOX"),
        "",
        tab | CBS_DROPDOWNLIST as u32,
        none,
        (l + 150, 176, 190, 200),
        ID_THEME,
        Kind::Combo,
        false,
    );
    label(&mut add, "Default search engine (address with {q})", (l, 216, 340, 22));
    add(w!("EDIT"), "", edit_style, edge, (l, 238, 340, 26), ID_SEARCH, Kind::Edit, false);

    label(&mut add, "Put tabs to sleep after (seconds)", (l, 278, 260, 22));
    add(w!("EDIT"), "", number_style, edge, (l + 264, 274, 76, 26), ID_SUSPEND, Kind::Edit, false);
    label(&mut add, "Discard them after (minutes)", (l, 312, 260, 22));
    add(w!("EDIT"), "", number_style, edge, (l + 264, 308, 76, 26), ID_DISCARD, Kind::Edit, false);
    label(&mut add, "Discard them at memory use (%)", (l, 346, 260, 22));
    add(w!("EDIT"), "", number_style, edge, (l + 264, 342, 76, 26), ID_PRESSURE, Kind::Edit, false);
    label(&mut add, "Sites that never sleep (use more memory)", (l, 384, 340, 22));
    let list_style = tab | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32;
    add(w!("LISTBOX"), "", list_style, edge, (l, 406, 340, 104), ID_AWAKE_LIST, Kind::List, false);
    add(w!("EDIT"), "", edit_style, edge, (l, 518, 196, 26), ID_AWAKE_EDIT, Kind::Edit, false);
    add(w!("BUTTON"), "Add", button_style, none, (l + 204, 518, 66, 26), ID_AWAKE_ADD, Kind::Button, false);
    add(w!("BUTTON"), "Remove", button_style, none, (l + 276, 518, 64, 26), ID_AWAKE_REMOVE, Kind::Button, false);

    heading(&mut add, "Passwords", (l, 566, 340, 22));
    label(&mut add, "Offer to save", (l, 600, 140, 22));
    add(
        w!("COMBOBOX"),
        "",
        tab | CBS_DROPDOWNLIST as u32,
        none,
        (l + 150, 596, 190, 200),
        ID_PASSWORDS_OFFER,
        Kind::Combo,
        false,
    );
    add(w!("BUTTON"), "Import…", button_style, none, (l, 632, 110, 26), ID_PASSWORDS_IMPORT, Kind::Button, false);
    add(
        w!("BUTTON"),
        "Show saved…",
        button_style,
        none,
        (l + 118, 632, 150, 26),
        ID_PASSWORDS_LIST,
        Kind::Button,
        false,
    );
    heading(&mut add, "Extensions", (l, 688, 140, 22));
    add(w!("STATIC"), "", 0, none, (l + 150, 688, 582, 22), ID_EXT_NOTE, Kind::Label, false);
    add(w!("LISTBOX"), "", list_style, edge, (l, 714, 732, 88), ID_EXT_LIST, Kind::List, false);
    add(w!("BUTTON"), "Enable / disable", button_style, none, (l, 810, 170, 26), ID_EXT_TOGGLE, Kind::Button, false);
    add(w!("BUTTON"), "Pin / unpin", button_style, none, (l + 176, 810, 120, 26), ID_EXT_PIN, Kind::Button, false);
    add(w!("BUTTON"), "Reload", button_style, none, (l + 302, 810, 84, 26), ID_EXT_RELOAD, Kind::Button, false);
    add(w!("BUTTON"), "Remove…", button_style, none, (l + 392, 810, 80, 26), ID_EXT_REMOVE, Kind::Button, false);
    add(w!("BUTTON"), "Load unpacked…", button_style, none, (l + 478, 810, 170, 26), ID_EXT_LOAD, Kind::Button, false);

    // Right column: Shortcuts.
    let r = 400;
    heading(&mut add, "Shortcuts", (r, 18, 356, 22));
    let keys_style = list_style | (LBS_OWNERDRAWFIXED | LBS_HASSTRINGS) as u32;
    add(w!("LISTBOX"), "", keys_style, edge, (r, 48, 356, 298), ID_KEYS_LIST, Kind::List, false);
    add(w!("EDIT"), "", edit_style, edge, (r, 356, 176, 26), ID_KEY_EDIT, Kind::Edit, false);
    add(w!("BUTTON"), "Assign", button_style, none, (r + 184, 356, 80, 26), ID_KEY_ASSIGN, Kind::Button, false);
    add(w!("BUTTON"), "Reset", button_style, none, (r + 270, 356, 86, 26), ID_KEY_RESET, Kind::Button, false);
    label(&mut add, "Example: Ctrl+Shift+O or F6, F7. Empty: none.", (r, 388, 356, 22));
    add(
        w!("STATIC"),
        "Nea applies and saves changes immediately.",
        0,
        none,
        (r, 418, 356, 22),
        ID_STATUS,
        Kind::Label,
        false,
    );

    // The field: where it floats, laid out as the Position of Tabs.
    heading(&mut add, "Field", (r, 452, 140, 22));
    label(&mut add, "Position", (r, 486, 140, 22));
    add(
        w!("COMBOBOX"),
        "",
        tab | CBS_DROPDOWNLIST as u32,
        none,
        (r + 150, 482, 190, 200),
        ID_FIELD_POSITION,
        Kind::Combo,
        false,
    );

    // The box carries no text of its own: a themed checkbox ignores the
    // text colour, so in dark mode its label would vanish. The label
    // beside it toggles it too.
    heading(&mut add, "Updates", (r, 520, 140, 22));
    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r + 150, 521, 20, 20), ID_UPDATES, Kind::Button, false);
    add(
        w!("STATIC"),
        "Check once a day",
        SS_NOTIFY,
        none,
        (r + 174, 520, 182, 22),
        ID_UPDATES_LABEL,
        Kind::Label,
        false,
    );
    heading(&mut add, "Privacy", (r, 552, 140, 22));
    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r, 579, 20, 20), ID_TRACKERS, Kind::Button, false);
    add(
        w!("STATIC"),
        "Block third-party cookies and trackers",
        SS_NOTIFY,
        none,
        (r + 24, 578, 332, 22),
        ID_TRACKERS_LABEL,
        Kind::Label,
        false,
    );
    heading(&mut add, "Testing", (r, 610, 140, 22));
    add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r + 150, 611, 20, 20), ID_BENCH, Kind::Button, false);
    add(
        w!("STATIC"),
        "Script control (local pipe)",
        SS_NOTIFY,
        none,
        (r + 174, 610, 182, 22),
        ID_BENCH_LABEL,
        Kind::Label,
        false,
    );
    add(w!("STATIC"), "", SS_ENDELLIPSIS, none, (r, 638, 356, 22), ID_VERSION, Kind::Label, false);
    add(
        w!("BUTTON"),
        "Close",
        button_style,
        none,
        (CLIENT_WIDTH - 24 - 96, CLIENT_HEIGHT - 24 - 28, 96, 28),
        ID_CLOSE,
        Kind::Button,
        false,
    );

    specs
}

/// How long one step of the window made ahead may hold the UI thread.
const STEP_BUDGET_MS: f64 = 6.0;
/// What one more piece of work costs at most (measured: a combo box 4-12
/// ms, an edit or a list 1-6 ms, a label or a button about 1 ms).
fn unit_cost_ms(kind: Kind) -> f64 {
    match kind {
        Kind::Combo => 6.0,
        Kind::Edit | Kind::List => 3.0,
        Kind::Label | Kind::Button => 1.0,
    }
}

/// Whether one more piece of work, which costs about `next_ms`, still fits
/// in the step after `spent_ms`. The first piece of a step (`first`) always
/// runs, so each step makes progress, also with a piece over the budget.
fn fits_in_step(first: bool, spent_ms: f64, next_ms: f64) -> bool {
    first || spent_ms + next_ms <= STEP_BUDGET_MS
}

/// The pieces of work after the controls, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finish {
    /// The texts of the combo box with this index among the combo boxes.
    ComboTexts(usize),
    /// The row height of the shortcuts list, the wells and the hovers.
    Details,
    /// The chords other programs hold, and the state.
    State,
    /// The colours of the window and its caption.
    Colors,
    /// The themes of the controls from this index, `THEME_CHUNK` at a time.
    Themes(usize),
    /// The values of the fields.
    Fields,
    /// The rows of the shortcuts list from this index, `KEYS_CHUNK` at a time.
    Keys(usize),
}

const THEME_CHUNK: usize = 16;
const KEYS_CHUNK: usize = 16;
const COMBO_COUNT: usize = 4;

/// The piece that follows `step`, for `children` controls and `keys` rows
/// of the shortcuts list; none after the last one.
fn next_finish(step: Finish, children: usize, keys: usize) -> Option<Finish> {
    Some(match step {
        Finish::ComboTexts(i) if i + 1 < COMBO_COUNT => Finish::ComboTexts(i + 1),
        Finish::ComboTexts(_) => Finish::Details,
        Finish::Details => Finish::State,
        Finish::State => Finish::Colors,
        Finish::Colors => Finish::Themes(0),
        Finish::Themes(i) if i + THEME_CHUNK < children => Finish::Themes(i + THEME_CHUNK),
        Finish::Themes(_) => Finish::Fields,
        Finish::Fields => Finish::Keys(0),
        Finish::Keys(i) if i + KEYS_CHUNK < keys => Finish::Keys(i + KEYS_CHUNK),
        Finish::Keys(_) => return None,
    })
}

/// The window made ahead in steps: each step is a few milliseconds of the
/// UI thread, so the person's input is never held for the whole window.
struct Build {
    hwnd: HWND,
    instance: windows::Win32::Foundation::HINSTANCE,
    scale: f32,
    font: HFONT,
    heading_font: HFONT,
    specs: Vec<Spec>,
    children: Vec<(HWND, Kind)>,
    wells: Vec<RECT>,
    /// The next piece of the controls (two for each: made, then its font);
    /// past the last one, `finish` runs.
    next: usize,
    finish: Option<Finish>,
}

thread_local! {
    static BUILD: RefCell<Option<Build>> = const { RefCell::new(None) };
}

/// Loads the system theme of the control classes once, on a thread of its
/// own: the first combo box of the process spent about 6 ms more than the
/// next ones in its theme. The handles stay open, so the data stays loaded.
fn warm_themes() {
    static WARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARMED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("themes".into()).stack_size(64 * 1024).spawn(|| unsafe {
            for class in [w!("Combobox"), w!("Edit"), w!("Listbox"), w!("Button"), w!("Scrollbar")] {
                let _ = windows::Win32::UI::Controls::OpenThemeData(None, class);
            }
        });
    });
}

/// Makes the window itself (hidden) and its fonts: the first step.
fn begin(owner: HWND) -> windows::core::Result<Build> {
    warm_themes();
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("NeaSettings"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(crate::app_icon::ICON_ID as usize as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let px = |v: i32| (v as f32 * scale).round() as i32;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let ex_style = WS_EX_CONTROLPARENT;
        let mut outer = RECT { left: 0, top: 0, right: px(CLIENT_WIDTH), bottom: px(CLIENT_HEIGHT) };
        let _ = AdjustWindowRectExForDpi(&mut outer, style, false, ex_style, dpi);
        let (width, height) = (outer.right - outer.left, outer.bottom - outer.top);
        let mut owner_rect = RECT::default();
        let _ = GetWindowRect(owner, &mut owner_rect);
        let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
        let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 3;
        let hwnd = CreateWindowExW(
            ex_style,
            w!("NeaSettings"),
            w!("Settings — Nea"),
            style,
            x.max(0),
            y.max(0),
            width,
            height,
            Some(owner),
            None,
            Some(instance),
            None,
        )?;
        crate::app_icon::apply(hwnd);
        Ok(Build {
            hwnd,
            instance,
            scale,
            font: make_font(scale, 400),
            heading_font: make_font(scale, 500),
            specs: controls(),
            children: Vec::new(),
            wells: Vec::new(),
            next: 0,
            finish: None,
        })
    }
}

impl Build {
    fn px(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }

    /// Makes control `index`.
    fn make(&mut self, index: usize) {
        let spec = &self.specs[index];
        let (mut x, mut y, mut w, mut h) = spec.rect;
        let mut ex = spec.ex;
        if ex == WS_EX_CLIENTEDGE {
            // No sunken edge: the control sits inside a grey well the
            // window draws, a little smaller than the place it was given.
            self.wells.push(RECT { left: self.px(x), top: self.px(y), right: self.px(x + w), bottom: self.px(y + h) });
            let (dx, dy) = if matches!(spec.kind, Kind::Edit) { (8, 4) } else { (4, 4) };
            (x, y, w, h) = (x + dx, y + dy, w - 2 * dx, h - 2 * dy);
            ex = WINDOW_EX_STYLE::default();
        }
        let spec = &self.specs[index];
        unsafe {
            let child = CreateWindowExW(
                ex,
                spec.class,
                &wide(&spec.text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(spec.style),
                self.px(x),
                self.px(y),
                self.px(w),
                self.px(h),
                Some(self.hwnd),
                Some(HMENU(spec.id as isize as _)),
                Some(self.instance),
                None,
            )
            .unwrap_or_default();
            self.children.push((child, spec.kind));
        }
    }

    /// Gives control `index` its font: for a combo box, a few milliseconds
    /// of its own (it measures its rows again).
    fn give_font(&self, index: usize) {
        let font = if self.specs[index].heading { self.heading_font } else { self.font };
        unsafe {
            SendMessageW(self.children[index].0, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(0)));
        }
    }

    fn item(&self, id: i32) -> HWND {
        unsafe { GetDlgItem(Some(self.hwnd), id).unwrap_or_default() }
    }

    /// One piece of work after the controls.
    fn run_finish(&mut self, step: Finish, current: &Settings, palette: &Palette) {
        unsafe {
            match step {
                Finish::ComboTexts(i) => {
                    let (id, names): (i32, Vec<&str>) = match i {
                        0 => (ID_THEME, THEMES.iter().map(|(_, n)| *n).collect()),
                        1 => (ID_TAB_LAYOUT, TAB_LAYOUTS.iter().map(|(_, n)| *n).collect()),
                        2 => (ID_FIELD_POSITION, FIELD_POSITIONS.iter().map(|(_, n)| *n).collect()),
                        _ => (ID_PASSWORDS_OFFER, OFFER_CHOICES.iter().map(|(_, n)| *n).collect()),
                    };
                    let combo = self.item(id);
                    for name in names {
                        let text = wide(name);
                        SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
                    }
                }
                Finish::Details => {
                    let row = self.px(28) as isize;
                    SendMessageW(self.item(ID_KEYS_LIST), LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(row)));
                    WELLS.with(|w| *w.borrow_mut() = std::mem::take(&mut self.wells));
                    for (child, kind) in &self.children {
                        if matches!(kind, Kind::Button)
                            && (GetWindowLongW(*child, GWL_STYLE) as u32 & BS_TYPEMASK as u32) == BS_OWNERDRAW as u32
                        {
                            widgets::track_hover(*child);
                        }
                    }
                }
                Finish::State => {
                    let commands = shortcuts::all_commands();
                    let every_chord: Vec<shortcuts::Chord> =
                        commands.iter().flat_map(|c| shortcuts::chords_of(&current.shortcuts, *c)).collect();
                    let taken = taken_chords(self.hwnd, &every_chord);
                    let state = State {
                        hwnd: self.hwnd,
                        settings: current.clone(),
                        commands,
                        tab_layout: self.item(ID_TAB_LAYOUT),
                        collapsed: self.item(ID_COLLAPSED),
                        field_position: self.item(ID_FIELD_POSITION),
                        taken,
                        children: self.children.clone(),
                        theme: self.item(ID_THEME),
                        search: self.item(ID_SEARCH),
                        suspend: self.item(ID_SUSPEND),
                        discard: self.item(ID_DISCARD),
                        pressure: self.item(ID_PRESSURE),
                        split: self.item(ID_SPLIT),
                        preview: self.item(ID_PREVIEW),
                        awake_list: self.item(ID_AWAKE_LIST),
                        awake_edit: self.item(ID_AWAKE_EDIT),
                        keys_list: self.item(ID_KEYS_LIST),
                        key_edit: self.item(ID_KEY_EDIT),
                        updates: self.item(ID_UPDATES),
                        version: self.item(ID_VERSION),
                        bench: self.item(ID_BENCH),
                        trackers: self.item(ID_TRACKERS),
                        passwords_offer: self.item(ID_PASSWORDS_OFFER),
                        status_after_import: false,
                        status: self.item(ID_STATUS),
                        ext_list: self.item(ID_EXT_LIST),
                        ext_note: self.item(ID_EXT_NOTE),
                        font: self.font,
                        heading_font: self.heading_font,
                        scale: self.scale,
                    };
                    STATE.with(|s| *s.borrow_mut() = Some(state));
                }
                Finish::Colors => set_colors(palette),
                Finish::Themes(from) => {
                    let to = (from + THEME_CHUNK).min(self.children.len());
                    theme_children(&self.children[from..to], palette.dark);
                }
                Finish::Fields => {
                    with_state(|st| {
                        st.settings = current.clone();
                        fill_fields(st);
                    });
                }
                Finish::Keys(from) => {
                    with_state(|st| fill_keys_rows(st, from, KEYS_CHUNK));
                }
            }
        }
    }

    /// One step: what fits in `STEP_BUDGET_MS`. True when the window is
    /// whole.
    fn step(&mut self, current: &Settings, palette: &Palette, budget: bool) -> bool {
        let started = std::time::Instant::now();
        let spent = || started.elapsed().as_secs_f64() * 1000.0;
        let mut first = true;
        loop {
            // Two pieces for each control: make it, then give it its font.
            if self.next < 2 * self.specs.len() {
                let (index, font) = (self.next / 2, self.next % 2 == 1);
                let cost = unit_cost_ms(self.specs[index].kind);
                if budget && !fits_in_step(first, spent(), cost) {
                    return false;
                }
                first = false;
                if font {
                    self.give_font(index)
                } else {
                    self.make(index)
                }
                self.next += 1;
                if self.next == 2 * self.specs.len() {
                    self.finish = Some(Finish::ComboTexts(0));
                }
                continue;
            }
            let Some(step) = self.finish else { return true };
            if budget && !fits_in_step(first, spent(), 3.0) {
                return false;
            }
            first = false;
            self.run_finish(step, current, palette);
            let keys = shortcuts::all_commands().len();
            self.finish = next_finish(step, self.children.len(), keys);
            if self.finish.is_none() {
                return true;
            }
        }
    }
}

/// Makes the rest of a window that is made ahead, at once: the person
/// opens Settings before the steps came to the end.
fn finish_build(current: &Settings, palette: &Palette) {
    let Some(mut build) = BUILD.with(|b| b.borrow_mut().take()) else { return };
    let _doing = crate::diag::doing(&"settings:finish", 0);
    build.step(current, palette, false);
    finish_prepared(current, palette);
}

fn finish_prepared(current: &Settings, palette: &Palette) {
    PAINTED_WITH.with(|p| *p.borrow_mut() = palette_key(palette));
    unsafe {
        if let Some(hwnd) = STATE.with(|s| s.borrow().as_ref().map(|st| st.hwnd)) {
            let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
        }
    }
    PREPARED.with(|p| *p.borrow_mut() = Some((settings_key(current), palette_key(palette))));
}

/// Drops a window that is still being made (and its fonts).
fn drop_build() {
    let Some(build) = BUILD.with(|b| b.borrow_mut().take()) else { return };
    let assembled = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.hwnd == build.hwnd));
    if assembled {
        // The state owns the window and the fonts now.
        PREPARED.with(|p| *p.borrow_mut() = Some((String::new(), String::new())));
        discard_prepared();
        return;
    }
    unsafe {
        let _ = DestroyWindow(build.hwnd);
        let _ = DeleteObject(build.font.into());
        let _ = DeleteObject(build.heading_font.into());
    }
}

fn fill_all(st: &mut State) {
    fill_fields(st);
    fill_keys(st);
}

/// Everything `fill_all` fills but the shortcuts list.
fn fill_fields(st: &mut State) {
    let theme_index = THEMES.iter().position(|(t, _)| *t == st.settings.theme).unwrap_or(0);
    unsafe {
        SendMessageW(st.theme, CB_SETCURSEL, Some(WPARAM(theme_index)), None);
    }
    set_text(st.search, &st.settings.search_url);
    set_text(st.suspend, &st.settings.suspend_after_secs.to_string());
    set_text(st.discard, &(st.settings.discard_after_secs / 60).max(1).to_string());
    set_text(st.pressure, &st.settings.memory_pressure_percent.to_string());
    let check = if st.settings.split_view { BST_CHECKED.0 } else { 0 };
    let preview = if st.settings.hover_preview { BST_CHECKED.0 } else { 0 };
    unsafe {
        SendMessageW(st.split, BM_SETCHECK, Some(WPARAM(check as usize)), None);
        SendMessageW(st.preview, BM_SETCHECK, Some(WPARAM(preview as usize)), None);
    }
    let position_index = FIELD_POSITIONS.iter().position(|(p, _)| *p == st.settings.field_position).unwrap_or(0);
    unsafe {
        SendMessageW(st.field_position, CB_SETCURSEL, Some(WPARAM(position_index)), None);
    }
    let offer_index = OFFER_CHOICES.iter().position(|(on, _)| *on == st.settings.passwords_offer).unwrap_or(0);
    unsafe {
        SendMessageW(st.passwords_offer, CB_SETCURSEL, Some(WPARAM(offer_index)), None);
    }
    fill_awake(st, None);
    fill_tab_layout(st);
    let check = if st.settings.check_updates { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(st.updates, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
    }
    set_text(st.version, &version_line());
    fill_bench(st);
}

/// "Version 0.1.0", and the one waiting for the next launch if there is one.
fn version_line() -> String {
    let mut line = format!("Version {}", updater::CURRENT_VERSION);
    if cfg!(debug_assertions) {
        line.push_str(" (development)");
    }
    if let Some(next) = updater::pending_version() {
        line.push_str(&format!(" · {next} installs when you open Nea again"));
    }
    line
}

fn toggle_updates(st: &mut State, flip: bool) -> Outcome {
    unsafe {
        if flip {
            let checked = SendMessageW(st.updates, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
            let next = if checked { BST_UNCHECKED } else { BST_CHECKED };
            SendMessageW(st.updates, BM_SETCHECK, Some(WPARAM(next.0 as usize)), None);
        }
        st.settings.check_updates = SendMessageW(st.updates, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
    }
    status(
        st,
        if st.settings.check_updates {
            "Nea checks for a new version once a day."
        } else {
            "Nea does not check for new versions."
        },
    );
    Outcome::Changed
}

/// Same shape as the other checkboxes: a click on the label flips it first.
fn toggle_bench(st: &mut State, flip: bool) -> Outcome {
    unsafe {
        if flip {
            let checked = SendMessageW(st.bench, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
            let next = if checked { BST_UNCHECKED } else { BST_CHECKED };
            SendMessageW(st.bench, BM_SETCHECK, Some(WPARAM(next.0 as usize)), None);
        }
        st.settings.bench_enabled = SendMessageW(st.bench, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
    }
    let on = st.settings.bench_enabled;
    status(st, if on { "Script control is on. Use it only for testing." } else { "Script control is off." });
    Outcome::Changed
}

fn fill_bench(st: &State) {
    let check = if st.settings.bench_enabled { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(st.bench, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
    }
    let check = if st.settings.block_trackers { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(st.trackers, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
    }
}

fn toggle_trackers(st: &mut State, flip: bool) -> Outcome {
    unsafe {
        if flip {
            let checked = SendMessageW(st.trackers, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
            let next = if checked { BST_UNCHECKED } else { BST_CHECKED };
            SendMessageW(st.trackers, BM_SETCHECK, Some(WPARAM(next.0 as usize)), None);
        }
        st.settings.block_trackers = SendMessageW(st.trackers, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
    }
    let on = st.settings.block_trackers;
    status(
        st,
        if on { "Trackers and cookie banners are blocked." } else { "Trackers and cookie banners are allowed." },
    );
    Outcome::Changed
}

fn fill_awake(st: &State, select: Option<usize>) {
    let hosts: Vec<String> = st.settings.keep_awake.iter().cloned().collect();
    let select = select.filter(|_| !hosts.is_empty()).map(|i| i.min(hosts.len() - 1));
    list_fill(st.awake_list, &hosts, select);
}

fn fill_keys(st: &State) {
    list_fill(st.keys_list, &key_rows(st), list_selection(st.keys_list));
}

/// Rows `from..from + count` of the shortcuts list, for a window made in
/// steps: the first part empties the list.
fn fill_keys_rows(st: &State, from: usize, count: usize) {
    let rows = key_rows(st);
    let to = (from + count).min(rows.len());
    unsafe {
        if from == 0 {
            SendMessageW(st.keys_list, LB_RESETCONTENT, None, None);
        }
        for row in &rows[from.min(to)..to] {
            let text = wide(row);
            SendMessageW(st.keys_list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
    }
}

fn key_rows(st: &State) -> Vec<String> {
    st.commands
        .iter()
        .map(|c| {
            let chords = shortcuts::chords_of(&st.settings.shortcuts, *c);
            let shown = if chords.is_empty() { "none".to_string() } else { shortcuts::format_chords(&chords) };
            // A third field marks a chord another program holds.
            let taken = if chords.iter().any(|ch| st.taken.contains(ch)) { "\ttaken" } else { "" };
            format!("{}\t{shown}{taken}", c.label())
        })
        .collect()
}

fn status(st: &State, text: &str) {
    set_text(st.status, text);
}

/// Reads one text field back into the settings. A value that doesn't fit is
/// put back as it was, with a line saying why.
fn commit_field(st: &mut State, id: i32) -> Outcome {
    let before = st.settings.clone();
    let s = &mut st.settings;
    let error = match id {
        ID_SEARCH => {
            let text = text_of(st.search);
            if settings::valid_search_url(&text) {
                s.search_url = text.trim().to_string();
                None
            } else {
                Some("The search engine must start with https:// and have {q} where the search text goes.")
            }
        }
        ID_SUSPEND => match settings::parse_number(&text_of(st.suspend), 5..=86_400) {
            Some(n) => {
                s.suspend_after_secs = n;
                None
            }
            None => Some("Sleep: from 5 to 86400 seconds."),
        },
        ID_DISCARD => match settings::parse_number(&text_of(st.discard), 1..=10_080) {
            Some(n) => {
                s.discard_after_secs = n * 60;
                None
            }
            None => Some("Discard: from 1 to 10080 minutes."),
        },
        ID_PRESSURE => match settings::parse_number(&text_of(st.pressure), 10..=100) {
            Some(n) => {
                s.memory_pressure_percent = n as u32;
                None
            }
            None => Some("Memory: a percentage from 10 to 100."),
        },
        _ => return Outcome::Nothing,
    };
    if let Some(error) = error {
        status(st, error);
        let s = &st.settings;
        match id {
            ID_SEARCH => set_text(st.search, &s.search_url),
            ID_SUSPEND => set_text(st.suspend, &s.suspend_after_secs.to_string()),
            ID_DISCARD => set_text(st.discard, &(s.discard_after_secs / 60).max(1).to_string()),
            _ => set_text(st.pressure, &s.memory_pressure_percent.to_string()),
        }
        return Outcome::Nothing;
    }
    if st.settings.search_url == before.search_url
        && st.settings.suspend_after_secs == before.suspend_after_secs
        && st.settings.discard_after_secs == before.discard_after_secs
        && st.settings.memory_pressure_percent == before.memory_pressure_percent
    {
        return Outcome::Nothing;
    }
    status(st, "Saved.");
    Outcome::Changed
}

fn commit_all_fields(st: &mut State) -> Outcome {
    let mut changed = false;
    for id in [ID_SEARCH, ID_SUSPEND, ID_DISCARD, ID_PRESSURE] {
        changed |= matches!(commit_field(st, id), Outcome::Changed);
    }
    if changed { Outcome::Changed } else { Outcome::Nothing }
}

fn add_awake(st: &mut State) -> Outcome {
    let Some(host) = settings::normalize_host(&text_of(st.awake_edit)) else {
        status(st, "Type a site, for example meet.google.com.");
        return Outcome::Nothing;
    };
    set_text(st.awake_edit, "");
    if !st.settings.keep_awake.insert(host.clone()) {
        return Outcome::Nothing;
    }
    let index = st.settings.keep_awake.iter().position(|h| *h == host);
    fill_awake(st, index);
    status(st, &format!("{host} does not sleep now."));
    Outcome::Changed
}

fn remove_awake(st: &mut State) -> Outcome {
    let Some(index) = list_selection(st.awake_list) else { return Outcome::Nothing };
    let Some(host) = st.settings.keep_awake.iter().nth(index).cloned() else { return Outcome::Nothing };
    st.settings.keep_awake.remove(&host);
    fill_awake(st, Some(index));
    status(st, &format!("{host} sleeps like the other sites again."));
    Outcome::Changed
}

/// Reads the checkbox into the settings; a click on its label flips it first.
fn toggle_split(st: &mut State, flip: bool) -> Outcome {
    unsafe {
        if flip {
            let checked = SendMessageW(st.split, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0;
            let next = if checked { 0 } else { BST_CHECKED.0 };
            SendMessageW(st.split, BM_SETCHECK, Some(WPARAM(next as usize)), None);
        }
        st.settings.split_view = SendMessageW(st.split, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0;
    }
    status(
        st,
        if st.settings.split_view {
            "Split view: press Ctrl+Alt+N or drag a tab."
        } else {
            "Split view is off. Nea keeps the saved pairs."
        },
    );
    Outcome::Changed
}

/// The hover card's picture, like `toggle_split`.
fn toggle_preview(st: &mut State, flip: bool) -> Outcome {
    unsafe {
        if flip {
            let checked = SendMessageW(st.preview, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0;
            let next = if checked { 0 } else { BST_CHECKED.0 };
            SendMessageW(st.preview, BM_SETCHECK, Some(WPARAM(next as usize)), None);
        }
        st.settings.hover_preview = SendMessageW(st.preview, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0;
    }
    status(
        st,
        if st.settings.hover_preview {
            "The tab card shows a preview of the page."
        } else {
            "The tab card shows only the title and the status."
        },
    );
    Outcome::Changed
}

fn selected_command(st: &State) -> Option<Command> {
    list_selection(st.keys_list).and_then(|i| st.commands.get(i).copied())
}

fn show_command(st: &State) {
    if let Some(command) = selected_command(st) {
        let chords = shortcuts::chords_of(&st.settings.shortcuts, command);
        set_text(st.key_edit, &shortcuts::format_chords(&chords));
        let taken: Vec<_> =
            chords.iter().filter(|c| st.taken.contains(c)).map(|c| shortcuts::format_chord(*c)).collect();
        if !taken.is_empty() {
            status(st, &format!("Another program uses {}. Assign a different shortcut.", taken.join(", ")));
        }
    }
}

fn describe_displaced(displaced: &[Command]) -> String {
    if displaced.is_empty() {
        return String::new();
    }
    let names: Vec<String> = displaced.iter().map(|c| c.label()).collect();
    format!(" Removed from: {}.", names.join(", "))
}

fn assign_key(st: &mut State) -> Outcome {
    let Some(command) = selected_command(st) else {
        status(st, "Select an action in the list.");
        return Outcome::Nothing;
    };
    let text = text_of(st.key_edit);
    let Some(chords) = shortcuts::parse_chords(&text) else {
        status(st, &format!("Nea does not know '{}'. Example: Ctrl+Shift+O, F6, Alt+Left.", text.trim()));
        return Outcome::Nothing;
    };
    if let Some(bad) = chords.iter().find(|c| !shortcuts::is_usable(**c)) {
        status(st, &format!("{} alone types text in the field. Add Ctrl or Alt.", shortcuts::format_chord(*bad)));
        return Outcome::Nothing;
    }
    let displaced = shortcuts::assign(&mut st.settings.shortcuts, command, &chords);
    fill_keys(st);
    show_command(st);
    let shown = if chords.is_empty() { "none".to_string() } else { shortcuts::format_chords(&chords) };
    status(st, &format!("{}: {shown}.{}", command.label(), describe_displaced(&displaced)));
    Outcome::Changed
}

fn reset_key(st: &mut State) -> Outcome {
    let Some(command) = selected_command(st) else {
        status(st, "Select an action in the list.");
        return Outcome::Nothing;
    };
    let displaced = shortcuts::reset(&mut st.settings.shortcuts, command);
    fill_keys(st);
    show_command(st);
    status(st, &format!("{}: default shortcut.{}", command.label(), describe_displaced(&displaced)));
    Outcome::Changed
}

fn on_command(st: &mut State, id: i32, code: u32) -> Outcome {
    match (id, code) {
        (ID_THEME, CBN_SELCHANGE) => {
            let index = unsafe { SendMessageW(st.theme, CB_GETCURSEL, None, None).0 };
            match THEMES.get(index.max(0) as usize) {
                Some((theme, _)) if *theme != st.settings.theme => {
                    st.settings.theme = *theme;
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            }
        }
        (ID_PASSWORDS_OFFER, CBN_SELCHANGE) => {
            let index = unsafe { SendMessageW(st.passwords_offer, CB_GETCURSEL, None, None).0 };
            match OFFER_CHOICES.get(index.max(0) as usize) {
                Some((on, _)) if *on != st.settings.passwords_offer => {
                    st.settings.passwords_offer = *on;
                    status(
                        st,
                        if *on {
                            "Nea offers to save the password when you sign in."
                        } else {
                            "Nea does not offer to save new passwords."
                        },
                    );
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            }
        }
        (ID_FIELD_POSITION, CBN_SELCHANGE) => {
            let index = unsafe { SendMessageW(st.field_position, CB_GETCURSEL, None, None).0 };
            match FIELD_POSITIONS.get(index.max(0) as usize) {
                Some((position, _)) if *position != st.settings.field_position => {
                    st.settings.field_position = *position;
                    status(st, field_position_note(*position));
                    Outcome::Changed
                }
                _ => Outcome::Nothing,
            }
        }
        (ID_PASSWORDS_IMPORT, BN_CLICKED_CODE) => Outcome::ImportPasswords,
        (ID_PASSWORDS_LIST, BN_CLICKED_CODE) => {
            crate::web::push(crate::web::Event::Command(Command::Passwords));
            Outcome::Nothing
        }
        (ID_SEARCH | ID_SUSPEND | ID_DISCARD | ID_PRESSURE, EN_KILLFOCUS) => commit_field(st, id),
        (ID_TAB_LAYOUT, CBN_SELCHANGE) => {
            let index = unsafe { SendMessageW(st.tab_layout, CB_GETCURSEL, None, None).0 };
            let (_, collapsed) = TAB_LAYOUT.with(|l| l.get());
            match TAB_LAYOUTS.get(index.max(0) as usize) {
                Some((vertical, _)) => Outcome::Layout(*vertical, collapsed),
                None => Outcome::Nothing,
            }
        }
        (ID_COLLAPSED | ID_COLLAPSED_LABEL, _) if code == BN_CLICKED_CODE || code == STN_CLICKED => {
            let (vertical, collapsed) = TAB_LAYOUT.with(|l| l.get());
            if id == ID_COLLAPSED_LABEL {
                let check = if collapsed { BST_UNCHECKED } else { BST_CHECKED };
                unsafe {
                    SendMessageW(st.collapsed, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
                }
            }
            let now = unsafe { SendMessageW(st.collapsed, BM_GETCHECK, None, None).0 as u32 == BST_CHECKED.0 };
            status(
                st,
                if now {
                    "Tabs are hidden. Press Ctrl+K to go to a tab, or show them here."
                } else {
                    "Tabs are visible."
                },
            );
            Outcome::Layout(vertical, now)
        }
        (ID_SPLIT, BN_CLICKED_CODE) => toggle_split(st, false),
        (ID_SPLIT_LABEL, STN_CLICKED) => toggle_split(st, true),
        (ID_PREVIEW, BN_CLICKED_CODE) => toggle_preview(st, false),
        (ID_PREVIEW_LABEL, STN_CLICKED) => toggle_preview(st, true),
        (ID_AWAKE_ADD, BN_CLICKED_CODE) => add_awake(st),
        (ID_AWAKE_REMOVE, BN_CLICKED_CODE) => remove_awake(st),
        (ID_KEYS_LIST, LBN_SELCHANGE) => {
            show_command(st);
            Outcome::Nothing
        }
        (ID_KEY_ASSIGN, BN_CLICKED_CODE) => assign_key(st),
        (ID_KEY_RESET, BN_CLICKED_CODE) => reset_key(st),
        (ID_BENCH, BN_CLICKED_CODE) => toggle_bench(st, false),
        (ID_BENCH_LABEL, STN_CLICKED) => toggle_bench(st, true),
        (ID_CLOSE, BN_CLICKED_CODE) => Outcome::Close,
        (ID_TRACKERS, BN_CLICKED_CODE) => toggle_trackers(st, false),
        (ID_TRACKERS_LABEL, STN_CLICKED) => toggle_trackers(st, true),
        (ID_UPDATES, BN_CLICKED_CODE) => toggle_updates(st, false),
        (ID_UPDATES_LABEL, STN_CLICKED) => toggle_updates(st, true),
        (ID_EXT_TOGGLE | ID_EXT_PIN | ID_EXT_RELOAD | ID_EXT_REMOVE | ID_EXT_LOAD, BN_CLICKED_CODE) => {
            let command = match id {
                ID_EXT_TOGGLE => ExtensionCommand::Toggle,
                ID_EXT_PIN => ExtensionCommand::Pin,
                ID_EXT_RELOAD => ExtensionCommand::Reload,
                ID_EXT_REMOVE => ExtensionCommand::Remove,
                _ => ExtensionCommand::LoadUnpacked,
            };
            Outcome::Extension(command, list_selection(st.ext_list))
        }
        // Enter, sent by the dialog keyboard handling: it does what the
        // focused field is for.
        (1, _) => {
            let focus = unsafe { GetFocus() };
            if focus == st.key_edit {
                assign_key(st)
            } else if focus == st.awake_edit {
                add_awake(st)
            } else {
                commit_all_fields(st)
            }
        }
        // Esc.
        (2, _) => Outcome::Close,
        _ => Outcome::Nothing,
    }
}

/// What the status line says after picking a position, with the one
/// exception the choice has.
fn field_position_note(position: FieldPosition) -> &'static str {
    match position {
        FieldPosition::Top => "The field opens at the top.",
        FieldPosition::Middle => "The field opens in the middle. Find in page stays at the top.",
        FieldPosition::Bottom => "The field opens at the bottom, and its list opens upward.",
    }
}

fn emit(settings: Settings) {
    if let Some(handler) = HANDLER.with(|h| h.get()) {
        handler(settings);
    }
}

/// Saves what is still typed, then hides the window until the next open.
fn close() {
    let pending = with_state(|st| match commit_all_fields(st) {
        Outcome::Changed => Some(st.settings.clone()),
        _ => None,
    })
    .flatten();
    if let Some(settings) = pending {
        emit(settings);
    }
    let Some((hwnd, settings)) =
        STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| (st.hwnd, st.settings.clone()))))
    else {
        return;
    };
    // It leaves as a dialog does (100 ms) and then waits hidden, as the
    // window made ahead does: the next open only shows it again. Making
    // and destroying it on each open also left about six GDI objects behind.
    crate::fade::fade_out(hwnd, crate::anim::specs::DIALOG, 0, None);
    let painted_with = PAINTED_WITH.with(|p| p.borrow().clone());
    PREPARED.with(|p| *p.borrow_mut() = Some((settings_key(&settings), painted_with)));
    if let Some(closed) = CLOSED.with(|c| c.get()) {
        closed();
    }
}

/// Who hears that the window closed (the app makes the next one ahead).
pub fn set_closed_handler(handler: fn()) {
    CLOSED.with(|c| c.set(Some(handler)));
}

/// Owner-drawn row of the shortcuts list: the action on the left, its chords
/// on the right, the picked row on a soft rounded ground.
fn draw_key_row(item: &DRAWITEMSTRUCT, palette: &Palette, scale: f32) {
    if item.itemID == u32::MAX {
        return;
    }
    let text = unsafe {
        let len = SendMessageW(item.hwndItem, LB_GETTEXTLEN, Some(WPARAM(item.itemID as usize)), None).0;
        if len < 0 {
            return;
        }
        let mut buffer = vec![0u16; len as usize + 1];
        SendMessageW(
            item.hwndItem,
            LB_GETTEXT,
            Some(WPARAM(item.itemID as usize)),
            Some(LPARAM(buffer.as_mut_ptr() as isize)),
        );
        String::from_utf16_lossy(&buffer[..len as usize])
    };
    let (label, chords) = text.split_once('\t').unwrap_or((&text, ""));
    let (chords, taken) = match chords.split_once('\t') {
        Some((chords, _)) => (chords, true),
        None => (chords, false),
    };
    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
    SURFACES.with(|s| {
        s.borrow_mut().2.paint(item.hDC, item.rcItem, |g| {
            let rect: RectF = item.rcItem.into();
            g.fill(rect, palette.selection);
            if selected {
                g.round(rect.inset(0.0, 1.0), size::RADIUS * scale * 0.75, palette.tab_active);
            }
            let inner = rect.inset(10.0 * scale, 0.0);
            let small = Font::new(size::TEXT_SMALL * scale, size::REGULAR, Align::Leading);
            let mut chords_rect = RectF::new(inner.right, inner.top, inner.right, inner.bottom);
            if taken {
                // The chord never reaches us: said in the row, in the
                // private violet (the one non-grey ink the interface has).
                let note = "Used by another program";
                let note_w = g.measure(note, small);
                let note_rect = RectF::new(inner.right - note_w - 1.0, inner.top, inner.right, inner.bottom);
                g.text(note, note_rect, small, palette.private_mark);
                chords_rect.right = note_rect.left - 6.0 * scale;
            }
            let chords_w = g.measure(chords, small).min(inner.width() * 0.5);
            chords_rect.left = chords_rect.right - chords_w - 1.0;
            g.text(chords, chords_rect, small, palette.ink_muted);
            let label_rect = RectF::new(inner.left, inner.top, chords_rect.left - 8.0 * scale, inner.bottom);
            g.text(label, label_rect, Font::new(size::TEXT * scale, size::REGULAR, Align::Leading), palette.ink);
        })
    });
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16 as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            let mut extension = None;
            let mut layout = None;
            let outcome = with_state(|st| match on_command(st, id, code) {
                Outcome::Layout(vertical, collapsed) => {
                    layout = Some((vertical, collapsed));
                    Some(None)
                }
                Outcome::Changed => Some(Some(st.settings.clone())),
                Outcome::Close => None,
                Outcome::Nothing => Some(None),
                Outcome::ImportPasswords => {
                    st.status_after_import = true;
                    Some(None)
                }
                Outcome::Extension(command, row) => {
                    extension = Some((command, row));
                    Some(None)
                }
            });
            match outcome {
                Some(Some(Some(settings))) => emit(settings),
                Some(None) => close(),
                _ => {}
            }
            if with_state(|st| std::mem::take(&mut st.status_after_import)) == Some(true) {
                // The menu belongs to this window; the import itself runs
                // in the main window, which shows how far it is.
                if let Some(source) = crate::vault_import::choose(hwnd, None)
                    && let Ok(main) = unsafe { GetWindow(hwnd, GW_OWNER) }
                {
                    crate::app::start_import(main, source);
                }
            }
            if let Some((vertical, collapsed)) = layout {
                emit_layout(vertical, collapsed);
            }
            if let (Some((command, row)), Some(handler)) = (extension, EXTENSION_HANDLER.with(|h| h.get())) {
                handler(command, row);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let suggested = unsafe { crate::msg_param::read::<RECT>(lparam) }.copied();
            rescale((wparam.0 & 0xFFFF) as u32, suggested);
            LRESULT(0)
        }
        WM_NOTIFY => {
            // The check boxes stay native and are drawn as the design system's.
            // Windows does not marshal WM_NOTIFY: from another program,
            // lParam points into that program, or nowhere.
            let Some(header) = (unsafe { crate::msg_param::read::<NMHDR>(lparam) })
                .filter(|header| crate::msg_param::from_child(hwnd, header.hwndFrom))
            else {
                return LRESULT(0);
            };
            let is_check = header.code == NM_CUSTOMDRAW
                && unsafe {
                    (GetWindowLongW(header.hwndFrom, GWL_STYLE) as u32 & BS_TYPEMASK as u32) == BS_AUTOCHECKBOX as u32
                };
            let custom = || unsafe { crate::msg_param::read::<NMCUSTOMDRAW>(lparam) };
            match (LOOK.with(|l| l.get()), is_check.then(custom).flatten()) {
                (Some((palette, scale)), Some(nm)) => {
                    widgets::draw_check(nm, Color::from(palette.field), &palette, scale)
                }
                _ => default(),
            }
        }
        WM_DRAWITEM => {
            // Only from our own controls: another program can send any lParam.
            let Some(item) = (unsafe { crate::msg_param::read::<DRAWITEMSTRUCT>(lparam) })
                .filter(|item| crate::msg_param::from_child(hwnd, item.hwndItem))
            else {
                return LRESULT(0);
            };
            let Some((palette, scale)) = LOOK.with(|l| l.get()) else { return LRESULT(1) };
            if item.CtlType == ODT_BUTTON {
                let kind = if item.CtlID == ID_CLOSE as u32 { ButtonKind::Primary } else { ButtonKind::Secondary };
                SURFACES.with(|s| {
                    widgets::draw_button_item(
                        &mut s.borrow_mut().1,
                        item,
                        kind,
                        Color::from(palette.field),
                        &palette,
                        scale,
                    )
                });
            } else {
                draw_key_row(item, &palette, scale);
            }
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let colors = COLORS.with(|c| c.get());
            if colors.background == 0 {
                return default();
            }
            let hdc = HDC(wparam.0 as _);
            let on_field = msg == WM_CTLCOLOREDIT || msg == WM_CTLCOLORLISTBOX;
            unsafe {
                SetTextColor(hdc, COLORREF(colors.ink));
                SetBkColor(hdc, COLORREF(if on_field { colors.field_color } else { colors.background_color }));
            }
            LRESULT(if on_field { colors.field } else { colors.background })
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let wells = WELLS.with(|w| w.borrow().clone());
                if let Some((palette, scale)) = LOOK.with(|l| l.get()) {
                    SURFACES.with(|s| {
                        s.borrow_mut().0.paint(hdc, rect, |g| {
                            g.fill(rect.into(), palette.field);
                            for well in &wells {
                                widgets::draw_well(g, (*well).into(), &palette, scale);
                            }
                        })
                    });
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => default(),
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/settings_window.rs"]
#[rustfmt::skip]
mod tests;
