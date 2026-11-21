//! Ajustes (Ctrl+,): one small native window with plain controls — no web
//! view for the interface. The controls are the system's, framed in the
//! app's look: the page's ground, edits and lists sitting in soft grey wells
//! instead of a sunken edge, and buttons drawn by `widgets`. It edits a copy of the settings and hands every
//! committed change to the app, which applies it at once and writes
//! settings.json. Everything it holds (window, fonts, brushes) is freed when
//! it closes.

use std::cell::{Cell, RefCell};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HDC, HFONT, PAINTSTRUCT, RDW_ALLCHILDREN,
            RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, SetBkColor, SetTextColor,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{
                BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_SELECTED, ODT_BUTTON,
                SetWindowTheme,
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
    widgets::{self, ButtonKind},
    settings::{self, Settings, Theme},
    shortcuts::{self, Command},
    updater,
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

const BN_CLICKED_CODE: u32 = 0;
// Static control styles (their constants live behind another `windows` feature).
const SS_NOTIFY: u32 = 0x0100;
const SS_ENDELLIPSIS: u32 = 0x4000;
const CLIENT_WIDTH: i32 = 780;
const CLIENT_HEIGHT: i32 = 898;
/// Where the tabs go: the first thing in Ajustes, since the shortcut alone
/// went unnoticed (and another program may hold it).
const TAB_LAYOUTS: [(bool, &str); 2] = [(false, "Arriba"), (true, "Al costado")];
const THEMES: [(Theme, &str); 3] = [(Theme::System, "Del sistema"), (Theme::Light, "Claro"), (Theme::Dark, "Oscuro")];
const OFFER_CHOICES: [(bool, &str); 2] = [(true, "Al iniciar sesión"), (false, "Nunca")];

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
    /// Shortcuts another program registered system-wide: they never reach us.
    taken: Vec<shortcuts::Chord>,
    /// "Importar…" was pressed: its menu opens after the state is released.
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

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static HANDLER: Cell<Option<fn(Settings)>> = const { Cell::new(None) };
    static EXTENSION_HANDLER: Cell<Option<fn(ExtensionCommand, Option<usize>)>> = const { Cell::new(None) };
    /// Who applies the tab layout (vertical, collapsed): the app, which keeps
    /// it in the session rather than in the settings.
    static LAYOUT_HANDLER: Cell<Option<fn(bool, bool)>> = const { Cell::new(None) };
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

/// Opens the window, or brings it forward if it is already open.
pub fn open(owner: HWND, current: &Settings, palette: &Palette, on_change: fn(Settings)) {
    let existing = STATE.with(|s| s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| st.hwnd)));
    if let Some(hwnd) = existing {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            if crate::omnibar::owns_foreground() {
                let _ = SetForegroundWindow(hwnd);
            }
        }
        return;
    }
    HANDLER.with(|h| h.set(Some(on_change)));
    let Ok(state) = create(owner, current) else { return };
    let hwnd = state.hwnd;
    let first = state.theme;
    STATE.with(|s| *s.borrow_mut() = Some(state));
    set_palette(palette);
    with_state(|st| fill_all(st));
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

/// The app changed a setting itself (Mantener sitio despierto from a tab):
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

/// Re-colours the window (also the first time): the same pairs as the rest.
pub fn set_palette(palette: &Palette) {
    let Some((hwnd, children, _font, scale)) = STATE.with(|s| {
        s.try_borrow().ok().and_then(|g| g.as_ref().map(|st| (st.hwnd, st.children.clone(), st.font, st.scale)))
    })
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
        for (child, kind) in children {
            let _ = match (palette.dark, kind) {
                (_, Kind::Label) => Ok(()),
                (true, Kind::Edit | Kind::Combo) => SetWindowTheme(child, w!("DarkMode_CFD"), PCWSTR::null()),
                (true, _) => SetWindowTheme(child, w!("DarkMode_Explorer"), PCWSTR::null()),
                (false, Kind::Edit | Kind::Combo) => SetWindowTheme(child, PCWSTR::null(), PCWSTR::null()),
                (false, _) => SetWindowTheme(child, w!("Explorer"), PCWSTR::null()),
            };
        }
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn create(owner: HWND, current: &Settings) -> windows::core::Result<State> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("LivianoSettings"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(1 as _)).unwrap_or_default(),
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
            w!("LivianoSettings"),
            w!("Ajustes — Liviano"),
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

        let font = make_font(scale, 400);
        let heading_font = make_font(scale, 500);
        let mut children: Vec<(HWND, Kind)> = Vec::new();
        let mut wells: Vec<RECT> = Vec::new();
        let mut add = |class: PCWSTR, text: &str, style: u32, ex: WINDOW_EX_STYLE, rect: (i32, i32, i32, i32), id: i32, kind: Kind, heading: bool| {
            let (mut x, mut y, mut w, mut h) = rect;
            let mut ex = ex;
            if ex == WS_EX_CLIENTEDGE {
                // No sunken edge: the control sits inside a grey well the
                // window draws, a little smaller than the place it was given.
                wells.push(RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) });
                let (dx, dy) = if matches!(kind, Kind::Edit) { (8, 4) } else { (4, 4) };
                (x, y, w, h) = (x + dx, y + dy, w - 2 * dx, h - 2 * dy);
                ex = WINDOW_EX_STYLE::default();
            }
            let child = CreateWindowExW(
                ex,
                class,
                &wide(text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                px(x),
                px(y),
                px(w),
                px(h),
                Some(hwnd),
                Some(HMENU(id as isize as _)),
                Some(instance),
                None,
            )
            .unwrap_or_default();
            SendMessageW(child, WM_SETFONT, Some(WPARAM(if heading { heading_font.0 } else { font.0 } as usize)), Some(LPARAM(1)));
            children.push((child, kind));
            child
        };
        let edge = WS_EX_CLIENTEDGE;
        let none = WINDOW_EX_STYLE::default();
        let tab = WS_TABSTOP.0;
        let label = |add: &mut dyn FnMut(PCWSTR, &str, u32, WINDOW_EX_STYLE, (i32, i32, i32, i32), i32, Kind, bool) -> HWND,
                     text: &str,
                     rect: (i32, i32, i32, i32)| {
            add(w!("STATIC"), text, 0, none, rect, -1, Kind::Label, false)
        };
        let heading = |add: &mut dyn FnMut(PCWSTR, &str, u32, WINDOW_EX_STYLE, (i32, i32, i32, i32), i32, Kind, bool) -> HWND,
                       text: &str,
                       rect: (i32, i32, i32, i32)| {
            add(w!("STATIC"), text, 0, none, rect, -1, Kind::Label, true)
        };
        let edit_style = tab | ES_AUTOHSCROLL as u32;
        let number_style = edit_style | ES_NUMBER as u32;
        let button_style = tab | BS_OWNERDRAW as u32;

        // Left column: Pestañas first (where they go is the first thing
        // asked for), then General.
        let l = 24;
        heading(&mut add, "Pestañas", (l, 18, 340, 22));
        label(&mut add, "Ubicación", (l, 52, 140, 22));
        let tab_layout = add(w!("COMBOBOX"), "", tab | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, none, (l + 150, 48, 190, 200), ID_TAB_LAYOUT, Kind::Combo, false);
        // The checkboxes have no text of their own: a themed checkbox ignores
        // the dark palette's ink, a label doesn't.
        let collapsed = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l, 86, 18, 18), ID_COLLAPSED, Kind::Button, false);
        add(w!("STATIC"), "Ocultar las pestañas", SS_NOTIFY, none, (l + 22, 84, 150, 22), ID_COLLAPSED_LABEL, Kind::Label, false);
        let split = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l + 180, 86, 18, 18), ID_SPLIT, Kind::Button, false);
        add(w!("STATIC"), "Vista dividida", SS_NOTIFY, none, (l + 202, 84, 138, 22), ID_SPLIT_LABEL, Kind::Label, false);

        let preview = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (l, 112, 18, 18), ID_PREVIEW, Kind::Button, false);
        add(w!("STATIC"), "Vista previa de la página al pasar el mouse", SS_NOTIFY, none, (l + 22, 110, 318, 22), ID_PREVIEW_LABEL, Kind::Label, false);

        heading(&mut add, "General", (l, 146, 340, 22));
        label(&mut add, "Tema", (l, 180, 140, 22));
        let theme = add(w!("COMBOBOX"), "", tab | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, none, (l + 150, 176, 190, 200), ID_THEME, Kind::Combo, false);
        label(&mut add, "Buscador por defecto (dirección con {q})", (l, 216, 340, 22));
        let search = add(w!("EDIT"), "", edit_style, edge, (l, 238, 340, 26), ID_SEARCH, Kind::Edit, false);

        label(&mut add, "Dormir las de fondo tras (segundos)", (l, 278, 260, 22));
        let suspend = add(w!("EDIT"), "", number_style, edge, (l + 264, 274, 76, 26), ID_SUSPEND, Kind::Edit, false);
        label(&mut add, "Descartarlas tras (minutos)", (l, 312, 260, 22));
        let discard = add(w!("EDIT"), "", number_style, edge, (l + 264, 308, 76, 26), ID_DISCARD, Kind::Edit, false);
        label(&mut add, "Descartar ya con la memoria al (%)", (l, 346, 260, 22));
        let pressure = add(w!("EDIT"), "", number_style, edge, (l + 264, 342, 76, 26), ID_PRESSURE, Kind::Edit, false);
        label(&mut add, "Sitios que nunca se duermen (usan más memoria)", (l, 384, 340, 22));
        let list_style = tab | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32;
        let awake_list = add(w!("LISTBOX"), "", list_style, edge, (l, 406, 340, 104), ID_AWAKE_LIST, Kind::List, false);
        let awake_edit = add(w!("EDIT"), "", edit_style, edge, (l, 518, 196, 26), ID_AWAKE_EDIT, Kind::Edit, false);
        add(w!("BUTTON"), "Agregar", button_style, none, (l + 204, 518, 66, 26), ID_AWAKE_ADD, Kind::Button, false);
        add(w!("BUTTON"), "Quitar", button_style, none, (l + 276, 518, 64, 26), ID_AWAKE_REMOVE, Kind::Button, false);

        heading(&mut add, "Contraseñas", (l, 566, 340, 22));
        label(&mut add, "Ofrecer guardarlas", (l, 600, 140, 22));
        let passwords_offer = add(w!("COMBOBOX"), "", tab | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, none, (l + 150, 596, 190, 200), ID_PASSWORDS_OFFER, Kind::Combo, false);
        add(w!("BUTTON"), "Importar…", button_style, none, (l, 632, 110, 26), ID_PASSWORDS_IMPORT, Kind::Button, false);
        add(w!("BUTTON"), "Ver las guardadas…", button_style, none, (l + 118, 632, 150, 26), ID_PASSWORDS_LIST, Kind::Button, false);
        heading(&mut add, "Extensiones", (l, 688, 140, 22));
        let ext_note = label(&mut add, "", (l + 150, 688, 582, 22));
        let ext_list = add(w!("LISTBOX"), "", list_style, edge, (l, 714, 732, 88), ID_EXT_LIST, Kind::List, false);
        add(w!("BUTTON"), "Habilitar / deshabilitar", button_style, none, (l, 810, 170, 26), ID_EXT_TOGGLE, Kind::Button, false);
        add(w!("BUTTON"), "Fijar / desfijar", button_style, none, (l + 176, 810, 120, 26), ID_EXT_PIN, Kind::Button, false);
        add(w!("BUTTON"), "Recargar", button_style, none, (l + 302, 810, 84, 26), ID_EXT_RELOAD, Kind::Button, false);
        add(w!("BUTTON"), "Quitar…", button_style, none, (l + 392, 810, 80, 26), ID_EXT_REMOVE, Kind::Button, false);
        add(w!("BUTTON"), "Cargar sin empaquetar…", button_style, none, (l + 478, 810, 170, 26), ID_EXT_LOAD, Kind::Button, false);

        // Right column: Atajos.
        let r = 400;
        heading(&mut add, "Atajos", (r, 18, 356, 22));
        let keys_style = list_style | (LBS_OWNERDRAWFIXED | LBS_HASSTRINGS) as u32;
        let keys_list = add(w!("LISTBOX"), "", keys_style, edge, (r, 48, 356, 298), ID_KEYS_LIST, Kind::List, false);
        let key_edit = add(w!("EDIT"), "", edit_style, edge, (r, 356, 176, 26), ID_KEY_EDIT, Kind::Edit, false);
        add(w!("BUTTON"), "Asignar", button_style, none, (r + 184, 356, 80, 26), ID_KEY_ASSIGN, Kind::Button, false);
        add(w!("BUTTON"), "Restablecer", button_style, none, (r + 270, 356, 86, 26), ID_KEY_RESET, Kind::Button, false);
        label(&mut add, "Ej.: Ctrl+Shift+O o F6; varios con coma; vacío: sin atajo.", (r, 388, 356, 22));
        let status = label(&mut add, "Los cambios se aplican y se guardan al momento.", (r, 418, 356, 22));

        // The box carries no text of its own: a themed checkbox ignores the
        // text colour, so in dark mode its label would vanish. The label
        // beside it toggles it too.
        heading(&mut add, "Actualizaciones", (r, 452, 140, 22));
        let updates = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r + 150, 453, 20, 20), ID_UPDATES, Kind::Button, false);
        add(w!("STATIC"), "Buscar una vez por día", SS_NOTIFY, none, (r + 174, 452, 182, 22), ID_UPDATES_LABEL, Kind::Label, false);
        heading(&mut add, "Privacidad", (r, 484, 140, 22));
        let trackers = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r, 511, 20, 20), ID_TRACKERS, Kind::Button, false);
        add(w!("STATIC"), "Bloquear cookies de terceros y rastreadores", SS_NOTIFY, none, (r + 24, 510, 332, 22), ID_TRACKERS_LABEL, Kind::Label, false);
        heading(&mut add, "Pruebas", (r, 542, 140, 22));
        let bench = add(w!("BUTTON"), "", tab | BS_AUTOCHECKBOX as u32, none, (r + 150, 543, 20, 20), ID_BENCH, Kind::Button, false);
        add(w!("STATIC"), "Control por script (canal local)", SS_NOTIFY, none, (r + 174, 542, 182, 22), ID_BENCH_LABEL, Kind::Label, false);
        let version = add(w!("STATIC"), "", SS_ENDELLIPSIS, none, (r, 570, 356, 22), -1, Kind::Label, false);
        add(w!("BUTTON"), "Cerrar", button_style, none, (CLIENT_WIDTH - 24 - 96, CLIENT_HEIGHT - 24 - 28, 96, 28), ID_CLOSE, Kind::Button, false);

        for (theme_value, name) in THEMES {
            let _ = theme_value;
            let text = wide(name);
            SendMessageW(theme, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        for (_, name) in TAB_LAYOUTS {
            let text = wide(name);
            SendMessageW(tab_layout, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        for (_, name) in OFFER_CHOICES {
            let text = wide(name);
            SendMessageW(passwords_offer, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
        }
        SendMessageW(keys_list, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(px(28) as isize)));
        WELLS.with(|w| *w.borrow_mut() = wells);
        for (child, kind) in &children {
            if matches!(kind, Kind::Button) && (GetWindowLongW(*child, GWL_STYLE) as u32 & BS_TYPEMASK as u32) == BS_OWNERDRAW as u32 {
                widgets::track_hover(*child);
            }
        }

        let commands = shortcuts::all_commands();
        let every_chord: Vec<shortcuts::Chord> =
            commands.iter().flat_map(|c| shortcuts::chords_of(&current.shortcuts, *c)).collect();
        let taken = taken_chords(hwnd, &every_chord);
        Ok(State {
            hwnd,
            settings: current.clone(),
            commands,
            tab_layout,
            collapsed,
            taken,
            children,
            theme,
            search,
            suspend,
            discard,
            pressure,
            split,
            preview,
            awake_list,
            awake_edit,
            keys_list,
            key_edit,
            updates,
            version,
            bench,
            trackers,
            passwords_offer,
            status_after_import: false,
            status,
            ext_list,
            ext_note,
            font,
            heading_font,
            scale,
        })
    }
}

fn fill_all(st: &mut State) {
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
    let offer_index = OFFER_CHOICES.iter().position(|(on, _)| *on == st.settings.passwords_offer).unwrap_or(0);
    unsafe {
        SendMessageW(st.passwords_offer, CB_SETCURSEL, Some(WPARAM(offer_index)), None);
    }
    fill_awake(st, None);
    fill_keys(st);
    fill_tab_layout(st);
    let check = if st.settings.check_updates { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(st.updates, BM_SETCHECK, Some(WPARAM(check.0 as usize)), None);
    }
    set_text(st.version, &version_line());
    fill_bench(st);
}

/// "Versión 0.1.0", and the one waiting for the next launch if there is one.
fn version_line() -> String {
    let mut line = format!("Versión {}", updater::CURRENT_VERSION);
    if cfg!(debug_assertions) {
        line.push_str(" (desarrollo)");
    }
    if let Some(next) = updater::pending_version() {
        line.push_str(&format!(" · la {next} se instala al volver a abrir"));
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
    status(st, if st.settings.check_updates { "Busca una versión nueva una vez por día." } else { "No busca versiones nuevas." });
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
    status(st, if on { "Control por script activo: sólo para pruebas." } else { "Control por script apagado." });
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
        if on { "Rastreadores y carteles de cookies bloqueados." } else { "Rastreadores y carteles de cookies permitidos." },
    );
    Outcome::Changed
}

fn fill_awake(st: &State, select: Option<usize>) {
    let hosts: Vec<String> = st.settings.keep_awake.iter().cloned().collect();
    let select = select.filter(|_| !hosts.is_empty()).map(|i| i.min(hosts.len() - 1));
    list_fill(st.awake_list, &hosts, select);
}

fn fill_keys(st: &State) {
    let selected = list_selection(st.keys_list);
    let rows: Vec<String> = st
        .commands
        .iter()
        .map(|c| {
            let chords = shortcuts::chords_of(&st.settings.shortcuts, *c);
            let shown = if chords.is_empty() { "sin atajo".to_string() } else { shortcuts::format_chords(&chords) };
            // A third field marks a chord another program holds.
            let taken = if chords.iter().any(|ch| st.taken.contains(ch)) { "\tocupado" } else { "" };
            format!("{}\t{shown}{taken}", c.label())
        })
        .collect();
    list_fill(st.keys_list, &rows, selected);
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
                Some("El buscador tiene que empezar con https:// y llevar {q} donde va la búsqueda.")
            }
        }
        ID_SUSPEND => match settings::parse_number(&text_of(st.suspend), 5..=86_400) {
            Some(n) => {
                s.suspend_after_secs = n;
                None
            }
            None => Some("Dormir: entre 5 y 86400 segundos."),
        },
        ID_DISCARD => match settings::parse_number(&text_of(st.discard), 1..=10_080) {
            Some(n) => {
                s.discard_after_secs = n * 60;
                None
            }
            None => Some("Descartar: entre 1 y 10080 minutos."),
        },
        ID_PRESSURE => match settings::parse_number(&text_of(st.pressure), 10..=100) {
            Some(n) => {
                s.memory_pressure_percent = n as u32;
                None
            }
            None => Some("Memoria: un porcentaje entre 10 y 100."),
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
    status(st, "Guardado.");
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
        status(st, "Escribí un sitio, por ejemplo meet.google.com.");
        return Outcome::Nothing;
    };
    set_text(st.awake_edit, "");
    if !st.settings.keep_awake.insert(host.clone()) {
        return Outcome::Nothing;
    }
    let index = st.settings.keep_awake.iter().position(|h| *h == host);
    fill_awake(st, index);
    status(st, &format!("{host} ya no se duerme."));
    Outcome::Changed
}

fn remove_awake(st: &mut State) -> Outcome {
    let Some(index) = list_selection(st.awake_list) else { return Outcome::Nothing };
    let Some(host) = st.settings.keep_awake.iter().nth(index).cloned() else { return Outcome::Nothing };
    st.settings.keep_awake.remove(&host);
    fill_awake(st, Some(index));
    status(st, &format!("{host} vuelve a dormirse como las demás."));
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
            "Vista dividida: Ctrl+Alt+N o arrastrá una pestaña."
        } else {
            "Vista dividida apagada; los pares quedan guardados."
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
            "La tarjeta de cada pestaña muestra cómo estaba la página."
        } else {
            "La tarjeta muestra sólo el título y el estado."
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
        let taken: Vec<_> = chords.iter().filter(|c| st.taken.contains(c)).map(|c| shortcuts::format_chord(*c)).collect();
        if !taken.is_empty() {
            status(st, &format!("{} lo usa otro programa: asignale otro.", taken.join(", ")));
        }
    }
}

fn describe_displaced(displaced: &[Command]) -> String {
    if displaced.is_empty() {
        return String::new();
    }
    let names: Vec<String> = displaced.iter().map(|c| c.label()).collect();
    format!(" Se quitó de: {}.", names.join(", "))
}

fn assign_key(st: &mut State) -> Outcome {
    let Some(command) = selected_command(st) else {
        status(st, "Elegí una acción de la lista.");
        return Outcome::Nothing;
    };
    let text = text_of(st.key_edit);
    let Some(chords) = shortcuts::parse_chords(&text) else {
        status(st, &format!("No reconozco «{}». Ej.: Ctrl+Shift+O, F6, Alt+Izquierda.", text.trim()));
        return Outcome::Nothing;
    };
    if let Some(bad) = chords.iter().find(|c| !shortcuts::is_usable(**c)) {
        status(st, &format!("{} sola se escribiría en el campo: sumale Ctrl o Alt.", shortcuts::format_chord(*bad)));
        return Outcome::Nothing;
    }
    let displaced = shortcuts::assign(&mut st.settings.shortcuts, command, &chords);
    fill_keys(st);
    show_command(st);
    let shown = if chords.is_empty() { "sin atajo".to_string() } else { shortcuts::format_chords(&chords) };
    status(st, &format!("{}: {shown}.{}", command.label(), describe_displaced(&displaced)));
    Outcome::Changed
}

fn reset_key(st: &mut State) -> Outcome {
    let Some(command) = selected_command(st) else {
        status(st, "Elegí una acción de la lista.");
        return Outcome::Nothing;
    };
    let displaced = shortcuts::reset(&mut st.settings.shortcuts, command);
    fill_keys(st);
    show_command(st);
    status(st, &format!("{}: atajo original.{}", command.label(), describe_displaced(&displaced)));
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
                    status(st, if *on { "Se ofrece guardar la contraseña tras iniciar sesión." } else { "No se ofrece guardar contraseñas nuevas." });
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
            status(st, if now { "Pestañas ocultas: Ctrl+K para ir a una, o volvé a mostrarlas acá." } else { "Pestañas a la vista." });
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

fn emit(settings: Settings) {
    if let Some(handler) = HANDLER.with(|h| h.get()) {
        handler(settings);
    }
}

/// Saves what is still typed, then destroys the window and what it held.
fn close() {
    let pending = with_state(|st| match commit_all_fields(st) {
        Outcome::Changed => Some(st.settings.clone()),
        _ => None,
    })
    .flatten();
    if let Some(settings) = pending {
        emit(settings);
    }
    let Some(state) = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.take())) else { return };
    // It leaves as a dialog does (100 ms) and is destroyed once out of sight.
    crate::fade::fade_out(state.hwnd, crate::anim::specs::DIALOG, 0, Some(destroy));
    unsafe {
        let _ = DeleteObject(state.font.into());
        let _ = DeleteObject(state.heading_font.into());
        let colors = COLORS.with(|c| c.replace(Colors::default()));
        for brush in [colors.background, colors.field, colors.selection] {
            if brush != 0 {
                let _ = DeleteObject(HBRUSH(brush as _).into());
            }
        }
    }
}

fn destroy(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
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
        SendMessageW(item.hwndItem, LB_GETTEXT, Some(WPARAM(item.itemID as usize)), Some(LPARAM(buffer.as_mut_ptr() as isize)));
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
                let note = "otro programa";
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
                crate::vault_import::run(hwnd, None);
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
        WM_NOTIFY => {
            // The check boxes stay native and are drawn as HKUI's.
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            let is_check = header.code == NM_CUSTOMDRAW
                && unsafe { (GetWindowLongW(header.hwndFrom, GWL_STYLE) as u32 & BS_TYPEMASK as u32) == BS_AUTOCHECKBOX as u32 };
            match LOOK.with(|l| l.get()) {
                Some((palette, scale)) if is_check => {
                    let nm = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                    widgets::draw_check(nm, Color::from(palette.field), &palette, scale)
                }
                _ => default(),
            }
        }
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            let Some((palette, scale)) = LOOK.with(|l| l.get()) else { return LRESULT(1) };
            if item.CtlType == ODT_BUTTON {
                let kind = if item.CtlID == ID_CLOSE as u32 { ButtonKind::Primary } else { ButtonKind::Secondary };
                SURFACES.with(|s| {
                    widgets::draw_button_item(&mut s.borrow_mut().1, item, kind, Color::from(palette.field), &palette, scale)
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
