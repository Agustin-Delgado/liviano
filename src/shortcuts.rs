//! Keyboard shortcuts: one table, with overrides read from settings
//! ("Ctrl+Shift+T" style), so every chord can be reassigned — also while the
//! app runs: `Shortcuts` reloads in place, so every view holding it sees the
//! new table without being recreated.

use std::{cell::RefCell, collections::HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    FocusAddress,
    SwitchTab,
    NewTab,
    NewPrivateTab,
    CloseTab,
    ReopenTab,
    Back,
    Forward,
    PrevTab,
    NextTab,
    GoToTab(u8),
    ToggleTabLayout,
    ToggleSidebar,
    Find,
    DuplicateTab,
    CopyAddress,
    PasteAndGo,
    Downloads,
    Reload,
    HardReload,
    PinTab,
    KeepAwake,
    ToggleBookmark,
    Bookmarks,
    History,
    Settings,
    ReaderMode,
    FloatingVideo,
    HideElement,
    ShowHidden,
    ToggleShield,
    TogglePopups,
    SplitPage,
    Unsplit,
    FocusLeftPage,
    FocusRightPage,
    Passwords,
    GroupTab,
    UngroupTab,
    ToggleGroup,
}

impl Command {
    /// Stable names used by the settings file.
    pub fn name(self) -> String {
        match self {
            Command::GoToTab(n) => format!("go_to_tab_{n}"),
            other => {
                let name = match other {
                    Command::FocusAddress => "focus_address",
                    Command::SwitchTab => "switch_tab",
                    Command::NewTab => "new_tab",
                    Command::NewPrivateTab => "new_private_tab",
                    Command::CloseTab => "close_tab",
                    Command::ReopenTab => "reopen_tab",
                    Command::Back => "back",
                    Command::Forward => "forward",
                    Command::PrevTab => "prev_tab",
                    Command::NextTab => "next_tab",
                    Command::ToggleTabLayout => "toggle_tab_layout",
                    Command::ToggleSidebar => "toggle_sidebar",
                    Command::Find => "find",
                    Command::DuplicateTab => "duplicate_tab",
                    Command::CopyAddress => "copy_address",
                    Command::PasteAndGo => "paste_and_go",
                    Command::Downloads => "downloads",
                    Command::Reload => "reload",
                    Command::HardReload => "hard_reload",
                    Command::PinTab => "pin_tab",
                    Command::KeepAwake => "keep_awake",
                    Command::ToggleBookmark => "toggle_bookmark",
                    Command::Bookmarks => "bookmarks",
                    Command::History => "history",
                    Command::Settings => "settings",
                    Command::ReaderMode => "reader_mode",
                    Command::FloatingVideo => "floating_video",
                    Command::HideElement => "hide_element",
                    Command::ShowHidden => "show_hidden",
                    Command::ToggleShield => "toggle_shield",
                    Command::TogglePopups => "toggle_popups",
                    Command::SplitPage => "split_page",
                    Command::Unsplit => "unsplit",
                    Command::FocusLeftPage => "focus_left_page",
                    Command::FocusRightPage => "focus_right_page",
                    Command::Passwords => "passwords",
                    Command::GroupTab => "group_tab",
                    Command::UngroupTab => "ungroup_tab",
                    Command::ToggleGroup => "toggle_group",
                    Command::GoToTab(_) => unreachable!(),
                };
                name.to_string()
            }
        }
    }

    /// What the settings screen calls the command.
    pub fn label(self) -> String {
        let label = match self {
            Command::GoToTab(9) => return "Ir a la última pestaña".to_string(),
            Command::GoToTab(n) => return format!("Ir a la pestaña {n}"),
            Command::FocusAddress => "Barra de direcciones",
            Command::SwitchTab => "Cambiar de pestaña por nombre",
            Command::NewTab => "Nueva pestaña",
            Command::NewPrivateTab => "Nueva pestaña privada",
            Command::CloseTab => "Cerrar pestaña",
            Command::ReopenTab => "Reabrir pestaña cerrada",
            Command::Back => "Atrás",
            Command::Forward => "Adelante",
            Command::PrevTab => "Pestaña anterior",
            Command::NextTab => "Pestaña siguiente",
            Command::ToggleTabLayout => "Pestañas arriba / al costado",
            Command::ToggleSidebar => "Plegar barra lateral",
            Command::Find => "Buscar en la página",
            Command::DuplicateTab => "Duplicar pestaña",
            Command::CopyAddress => "Copiar dirección",
            Command::PasteAndGo => "Pegar e ir",
            Command::Downloads => "Descargas",
            Command::Reload => "Recargar",
            Command::HardReload => "Recargar sin caché",
            Command::PinTab => "Fijar pestaña",
            Command::KeepAwake => "Mantener sitio despierto",
            Command::ToggleBookmark => "Marcar página",
            Command::Bookmarks => "Marcadores",
            Command::History => "Historial",
            Command::Settings => "Ajustes",
            Command::ReaderMode => "Modo lectura",
            Command::FloatingVideo => "Video flotante",
            Command::HideElement => "Ocultar elemento",
            Command::ShowHidden => "Ver elementos ocultos",
            Command::ToggleShield => "Bloqueo de anuncios en este sitio",
            Command::TogglePopups => "Permitir ventanas emergentes en este sitio",
            Command::SplitPage => "Dividir página actual",
            Command::Unsplit => "Deshacer vista dividida",
            Command::FocusLeftPage => "Ir a la página dividida de la izquierda",
            Command::FocusRightPage => "Ir a la página dividida de la derecha",
            Command::Passwords => "Contraseñas",
            Command::GroupTab => "Agregar pestaña a un grupo nuevo",
            Command::UngroupTab => "Quitar pestaña del grupo",
            Command::ToggleGroup => "Contraer o expandir el grupo",
        };
        label.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub vk: u32,
}

const fn chord(ctrl: bool, shift: bool, alt: bool, vk: u32) -> Chord {
    Chord { ctrl, shift, alt, vk }
}

const VK_TAB: u32 = 0x09;
const VK_SPACE: u32 = 0x20;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_INSERT: u32 = 0x2D;
const VK_DELETE: u32 = 0x2E;
const VK_F1: u32 = 0x70;
const VK_F5: u32 = 0x74;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_COMMA: u32 = 0xBC;
const VK_OEM_MINUS: u32 = 0xBD;
const VK_OEM_PERIOD: u32 = 0xBE;

/// Named keys: the name a chord is written with (the Spanish keyboard's
/// label), then the other spellings accepted when reading one.
const NAMED_KEYS: &[(u32, &str, &[&str])] = &[
    (VK_TAB, "Tab", &["tabulador"]),
    (VK_SPACE, "Espacio", &["space"]),
    (VK_PRIOR, "RePág", &["pageup", "pgup", "repag"]),
    (VK_NEXT, "AvPág", &["pagedown", "pgdn", "avpag"]),
    (VK_END, "Fin", &["end"]),
    (VK_HOME, "Inicio", &["home"]),
    (VK_LEFT, "Izquierda", &["left"]),
    (VK_UP, "Arriba", &["up"]),
    (VK_RIGHT, "Derecha", &["right"]),
    (VK_DOWN, "Abajo", &["down"]),
    (VK_INSERT, "Insert", &["ins"]),
    (VK_DELETE, "Supr", &["delete", "del"]),
    (VK_OEM_PLUS, "=", &[]),
    (VK_OEM_COMMA, ",", &[]),
    (VK_OEM_MINUS, "-", &[]),
    (VK_OEM_PERIOD, ".", &[]),
];

const fn key(c: u8) -> u32 {
    c as u32
}

fn defaults() -> Vec<(Chord, Command)> {
    let mut table = vec![
        (chord(true, false, false, key(b'L')), Command::FocusAddress),
        (chord(true, false, false, key(b'K')), Command::SwitchTab),
        (chord(true, false, false, key(b'T')), Command::NewTab),
        (chord(true, true, false, key(b'N')), Command::NewPrivateTab),
        (chord(true, false, false, key(b'W')), Command::CloseTab),
        (chord(true, true, false, key(b'T')), Command::ReopenTab),
        (chord(false, false, true, VK_LEFT), Command::Back),
        (chord(false, false, true, VK_RIGHT), Command::Forward),
        (chord(true, true, false, VK_TAB), Command::PrevTab),
        (chord(true, false, false, VK_PRIOR), Command::PrevTab),
        (chord(true, false, false, VK_TAB), Command::NextTab),
        (chord(true, false, false, VK_NEXT), Command::NextTab),
        (chord(true, true, false, key(b'S')), Command::ToggleTabLayout),
        (chord(true, false, false, key(b'S')), Command::ToggleSidebar),
        (chord(true, false, false, key(b'F')), Command::Find),
        (chord(true, false, false, key(b'D')), Command::DuplicateTab),
        (chord(true, true, false, key(b'C')), Command::CopyAddress),
        (chord(true, true, false, key(b'V')), Command::PasteAndGo),
        (chord(true, true, false, key(b'J')), Command::Downloads),
        (chord(false, false, false, VK_F5), Command::Reload),
        (chord(true, false, false, key(b'R')), Command::Reload),
        (chord(true, false, false, VK_F5), Command::HardReload),
        (chord(true, true, false, key(b'B')), Command::ToggleBookmark),
        // Nea's own chord: a free chord beside the other panels'.
        (chord(true, true, false, key(b'O')), Command::Bookmarks),
        (chord(true, false, false, key(b'Y')), Command::History),
        (chord(true, false, false, VK_OEM_COMMA), Command::Settings),
        (chord(true, true, false, key(b'R')), Command::ReaderMode),
        (chord(true, true, false, key(b'P')), Command::FloatingVideo),
        (chord(true, true, false, key(b'H')), Command::HideElement),
        (chord(true, true, false, key(b'U')), Command::ShowHidden),
        (chord(true, false, true, key(b'N')), Command::SplitPage),
        // Nea's own chord: the split chord with Shift undoes it.
        (chord(true, true, true, key(b'N')), Command::Unsplit),
        (chord(false, true, true, VK_LEFT), Command::FocusLeftPage),
        (chord(false, true, true, VK_RIGHT), Command::FocusRightPage),
        (chord(true, false, true, key(b'L')), Command::Passwords),
        // Nea's own chord: G for group, free in the rest of it.
        (chord(true, true, false, key(b'G')), Command::GroupTab),
    ];
    for n in 1..=9u8 {
        table.push((chord(true, false, false, key(b'0' + n)), Command::GoToTab(n)));
    }
    table
}

pub struct Shortcuts {
    map: RefCell<HashMap<Chord, Command>>,
}

impl Shortcuts {
    pub fn new(overrides: &HashMap<String, String>) -> Self {
        Self { map: RefCell::new(table(overrides).into_iter().collect()) }
    }

    /// Replaces the table in place: every holder of this `Shortcuts` sees it.
    pub fn reload(&self, overrides: &HashMap<String, String>) {
        *self.map.borrow_mut() = table(overrides).into_iter().collect();
    }

    pub fn lookup(&self, chord: Chord) -> Option<Command> {
        self.map.borrow().get(&chord).copied()
    }
}

/// Defaults, then each override replaces every chord of its command. The
/// overrides go in name order, so a chord claimed twice always ends the same way.
pub fn table(overrides: &HashMap<String, String>) -> Vec<(Chord, Command)> {
    let mut table = defaults();
    let mut names: Vec<&String> = overrides.keys().collect();
    names.sort();
    let commands = all_commands();
    for name in names {
        let Some(&command) = commands.iter().find(|c| &c.name() == name) else {
            continue;
        };
        table.retain(|(_, c)| *c != command);
        for part in split_chords(&overrides[name]) {
            if let Some(parsed) = parse_chord(part) {
                table.retain(|(c, _)| *c != parsed);
                table.push((parsed, command));
            }
        }
    }
    table
}

/// Every command, in the order the settings screen lists them.
pub fn all_commands() -> Vec<Command> {
    let mut all: Vec<Command> = Vec::new();
    for command in defaults().into_iter().map(|(_, c)| c).chain([
        Command::PinTab,
        Command::KeepAwake,
        Command::ToggleShield,
        Command::TogglePopups,
        Command::UngroupTab,
        Command::ToggleGroup,
    ]) {
        if !all.contains(&command) {
            all.push(command);
        }
    }
    all
}

/// The chords that run `command` with these overrides.
pub fn chords_of(overrides: &HashMap<String, String>, command: Command) -> Vec<Chord> {
    table(overrides).into_iter().filter(|(_, c)| *c == command).map(|(chord, _)| chord).collect()
}

fn default_chords(command: Command) -> Vec<Chord> {
    chords_of(&HashMap::new(), command)
}

fn same_chords(a: &[Chord], b: &[Chord]) -> bool {
    a.len() == b.len() && a.iter().all(|c| b.contains(c))
}

/// Writes the override of `command`, or drops it when it equals the default.
fn put_override(overrides: &mut HashMap<String, String>, command: Command, chords: &[Chord]) {
    if same_chords(chords, &default_chords(command)) {
        overrides.remove(&command.name());
    } else {
        overrides.insert(command.name(), format_chords(chords));
    }
}

/// Gives `chords` to `command` (none: the command has no shortcut). A chord
/// another command held is taken from it; those commands are returned so the
/// screen can say what changed.
pub fn assign(overrides: &mut HashMap<String, String>, command: Command, chords: &[Chord]) -> Vec<Command> {
    let mut displaced = Vec::new();
    for other in all_commands() {
        if other == command {
            continue;
        }
        let held = chords_of(overrides, other);
        if held.iter().any(|c| chords.contains(c)) {
            let kept: Vec<Chord> = held.into_iter().filter(|c| !chords.contains(c)).collect();
            put_override(overrides, other, &kept);
            displaced.push(other);
        }
    }
    put_override(overrides, command, chords);
    displaced
}

/// Back to the default chords, taken from whoever holds them now.
pub fn reset(overrides: &mut HashMap<String, String>, command: Command) -> Vec<Command> {
    assign(overrides, command, &default_chords(command))
}

/// A chord that doesn't steal typing: it has Ctrl or Alt, or is a function
/// key. A bare letter (or Shift+letter) would fire while typing in the field.
pub fn is_usable(chord: Chord) -> bool {
    chord.ctrl || chord.alt || (VK_F1..VK_F1 + 12).contains(&chord.vk)
}

/// "Ctrl+Shift+T": how a chord is shown and stored.
pub fn format_chord(chord: Chord) -> String {
    let mut out = String::new();
    for (on, name) in [(chord.ctrl, "Ctrl+"), (chord.shift, "Shift+"), (chord.alt, "Alt+")] {
        if on {
            out.push_str(name);
        }
    }
    match chord.vk {
        vk if (VK_F1..VK_F1 + 12).contains(&vk) => out.push_str(&format!("F{}", vk - VK_F1 + 1)),
        vk if (key(b'0')..=key(b'9')).contains(&vk) || (key(b'A')..=key(b'Z')).contains(&vk) => {
            out.push(char::from(vk as u8))
        }
        vk => match NAMED_KEYS.iter().find(|(code, _, _)| *code == vk) {
            Some((_, name, _)) => out.push_str(name),
            None => out.push_str(&format!("0x{vk:02X}")),
        },
    }
    out
}

pub fn format_chords(chords: &[Chord]) -> String {
    chords.iter().map(|c| format_chord(*c)).collect::<Vec<_>>().join(", ")
}

/// Splits "Ctrl+L, Ctrl+," into its chords: a comma right after a '+' is
/// the comma key, any other comma separates two chords.
fn split_chords(spec: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut previous = None;
    for (i, c) in spec.char_indices() {
        if c == ',' && previous != Some('+') {
            parts.push(&spec[start..i]);
            start = i + 1;
        }
        if !c.is_whitespace() {
            previous = Some(c);
        }
    }
    parts.push(&spec[start..]);
    parts.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

/// Reads "Ctrl+L, F5" (empty: no chord at all). Fails if any part is unreadable.
pub fn parse_chords(spec: &str) -> Option<Vec<Chord>> {
    let mut chords = Vec::new();
    for part in split_chords(spec) {
        let chord = parse_chord(part)?;
        if !chords.contains(&chord) {
            chords.push(chord);
        }
    }
    Some(chords)
}

/// Parses "Ctrl+Shift+T", "Alt+Left", "F5", "Ctrl+PageDown", "Ctrl+,".
pub fn parse_chord(spec: &str) -> Option<Chord> {
    let spec = spec.trim();
    // A trailing "+" after another "+" is not a key we name; "Ctrl+," keeps its comma.
    let (mods, last) = match spec.rfind('+') {
        Some(i) if i + 1 < spec.len() => (&spec[..i], &spec[i + 1..]),
        Some(_) => return None,
        None => ("", spec),
    };
    let mut result = chord(false, false, false, 0);
    for token in mods.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_lowercase().as_str() {
            "ctrl" | "control" => result.ctrl = true,
            "shift" | "mayús" | "mayus" => result.shift = true,
            "alt" => result.alt = true,
            _ => return None,
        }
    }
    result.vk = parse_key(last.trim())?;
    Some(result)
}

fn parse_key(token: &str) -> Option<u32> {
    let lower = token.to_lowercase();
    if let Some((code, _, _)) =
        NAMED_KEYS.iter().find(|(_, name, aliases)| name.to_lowercase() == lower || aliases.contains(&lower.as_str()))
    {
        return Some(*code);
    }
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=12).contains(&n).then_some(VK_F1 + n - 1);
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as u32),
        _ => None,
    }
}
