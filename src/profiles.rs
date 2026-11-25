//! Profiles: each one is a separate person's browser, with its own folders,
//! its own corner of Credential Manager and its own window (one process per
//! profile, `nea.exe --profile <id>`). This module is the registry and the
//! names and places derived from an id; it touches no window and no file.
//!
//! The registry is `profiles.json` in the default roaming folder, never in a
//! profile's own. The data that was there before profiles is profile 0: it
//! keeps its folders, so nothing moves. A registry with no entry for 0 still
//! lists it, as "Personal" in grey.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::tab_groups::GroupColor;

/// The file of the registry, in the default roaming folder.
pub const REGISTRY_FILE: &str = "profiles.json";
/// The profile of the data that was there before profiles.
pub const DEFAULT_ID: u32 = 0;
/// What profile 0 is called until it is renamed.
pub const DEFAULT_NAME: &str = "Personal";
/// What profile 0 wears until it has an entry.
pub const DEFAULT_COLOR: GroupColor = GroupColor::Grey;
/// The folder, in each default root, that holds the other profiles' folders.
pub const PROFILES_DIR: &str = "Profiles";
/// The command line argument that names the profile.
pub const ARG: &str = "--profile";
/// A longer name does not fit the menu or the title.
pub const MAX_NAME: usize = 32;

/// One profile as the registry keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u32,
    pub name: String,
    /// A `GroupColor` name, kept as text as for the spaces.
    pub color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub profiles: Vec<Entry>,
    /// The profile whose window opened last.
    pub last: u32,
    /// The lowest id never given. Ids are never reused, so a profile made
    /// later never finds the folders of an older one.
    pub next: u32,
}

/// A profile as the menu and Settings show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub id: u32,
    pub name: String,
    pub color: GroupColor,
}

impl Registry {
    /// Every profile, profile 0 first, then by id.
    pub fn list(&self) -> Vec<Profile> {
        let mut list: Vec<Profile> = self
            .profiles
            .iter()
            .map(|e| Profile {
                id: e.id,
                name: e.name.clone(),
                color: GroupColor::from_name(&e.color).unwrap_or(DEFAULT_COLOR),
            })
            .collect();
        if !list.iter().any(|p| p.id == DEFAULT_ID) {
            list.push(Profile { id: DEFAULT_ID, name: DEFAULT_NAME.to_string(), color: DEFAULT_COLOR });
        }
        list.sort_by_key(|p| p.id);
        list.dedup_by_key(|p| p.id);
        list
    }

    /// Profiles show anywhere but Settings only when there are two or more.
    pub fn shown(&self) -> bool {
        self.list().len() >= 2
    }

    pub fn has(&self, id: u32) -> bool {
        id == DEFAULT_ID || self.profiles.iter().any(|e| e.id == id)
    }

    /// The profile `id`; one the registry lost is still named.
    pub fn get(&self, id: u32) -> Profile {
        self.list().into_iter().find(|p| p.id == id).unwrap_or_else(|| Profile {
            id,
            name: format!("Profile {id}"),
            color: DEFAULT_COLOR,
        })
    }

    /// The id the next new profile gets.
    pub fn next_id(&self) -> u32 {
        let past = self.profiles.iter().map(|e| e.id.saturating_add(1)).max().unwrap_or(1);
        self.next.max(past).max(1)
    }

    /// The first colour no profile wears, or the next in turn.
    pub fn next_color(&self) -> GroupColor {
        let list = self.list();
        GroupColor::ALL
            .into_iter()
            .find(|c| !list.iter().any(|p| p.color == *c))
            .unwrap_or(GroupColor::ALL[list.len() % GroupColor::ALL.len()])
    }

    /// Adds a profile named `name` (already cleaned) and returns its id.
    pub fn add(&mut self, name: &str) -> u32 {
        let id = self.next_id();
        let color = self.next_color();
        self.profiles.push(Entry { id, name: name.to_string(), color: color.name().to_string() });
        self.next = id + 1;
        id
    }

    /// Renames `id`. Profile 0 gets its entry here, in the colour it wore.
    pub fn rename(&mut self, id: u32, name: &str) -> bool {
        if let Some(entry) = self.profiles.iter_mut().find(|e| e.id == id) {
            entry.name = name.to_string();
            return true;
        }
        if id != DEFAULT_ID {
            return false;
        }
        self.profiles.push(Entry { id, name: name.to_string(), color: DEFAULT_COLOR.name().to_string() });
        true
    }
}

/// A typed name made fit for a profile: trimmed, one line, not too long.
/// None when nothing is left.
pub fn clean_name(text: &str) -> Option<String> {
    let one_line: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let words: Vec<&str> = one_line.split_whitespace().collect();
    let name: String = words.join(" ").chars().take(MAX_NAME).collect();
    let name = name.trim_end().to_string();
    (!name.is_empty()).then_some(name)
}

/// The letter of the profile's round mark.
pub fn initial(name: &str) -> char {
    let first = name.chars().find(|c| c.is_alphanumeric()).unwrap_or('?');
    first.to_uppercase().next().unwrap_or(first)
}

/// The profile the command line names (`--profile 2` or `--profile=2`).
/// No argument, or one that is not a number: profile 0.
pub fn from_args<I: IntoIterator<Item = OsString>>(args: I) -> u32 {
    let args: Vec<String> = args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
    for (i, arg) in args.iter().enumerate() {
        let value = if arg == ARG {
            args.get(i + 1).map(String::as_str)
        } else {
            arg.strip_prefix(ARG).and_then(|rest| rest.strip_prefix('='))
        };
        if let Some(value) = value {
            return value.trim().parse().unwrap_or(DEFAULT_ID);
        }
    }
    DEFAULT_ID
}

/// The profile this process opens: the one asked for, unless the registry
/// does not know it and it has no folder (a shortcut left from a lost
/// registry must not make an empty profile).
pub fn resolve(asked: u32, known: bool, folder_exists: bool) -> u32 {
    if asked == DEFAULT_ID || known || folder_exists { asked } else { DEFAULT_ID }
}

/// The registry's file, from the default roaming folder.
pub fn registry_path(default_roaming: &Path) -> PathBuf {
    default_roaming.join(REGISTRY_FILE)
}

/// A profile's own folder under a default root (roaming or local). None for
/// profile 0, which uses the root itself.
pub fn profile_dir(default_root: &Path, id: u32) -> Option<PathBuf> {
    (id != DEFAULT_ID).then(|| default_root.join(PROFILES_DIR).join(id.to_string()))
}

/// A profile's corner of Credential Manager: profile 0 keeps `base`, the
/// others get theirs inside it ("Nea:p1:"). The profile 0 store never reads
/// them: what follows its prefix is not an origin.
pub fn credential_prefix(base: &str, id: u32) -> String {
    if id == DEFAULT_ID { base.to_string() } else { format!("{base}p{id}:") }
}

/// The app's name with the profile's after it, when profiles show: "Nea —
/// Sofía". The window title ends with it.
pub fn app_name(display: &str, profile: Option<&str>) -> String {
    match profile {
        Some(name) => format!("{display} — {name}"),
        None => display.to_string(),
    }
}

/// The desktop shortcut's file name: "Nea (Sofía).lnk", with what a file
/// name cannot hold left out.
pub fn shortcut_file(app: &str, name: &str) -> String {
    let safe: String =
        name.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let safe = safe.trim().trim_end_matches('.');
    if safe.is_empty() { format!("{app}.lnk") } else { format!("{app} ({safe}).lnk") }
}

/// The name of the window property that tags a main window with its
/// profile. It holds a hash of the default roaming folder, so a Nea that
/// runs on other data (a test run) is never taken for this one.
pub fn window_property(default_roaming: &Path) -> String {
    let text = default_roaming.to_string_lossy().to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.trim_end_matches(['\\', '/']).bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("NeaProfile:{hash:016x}")
}

/// What the window property holds for `id`: 0 means no property, so the
/// value is one more than the id.
pub fn property_value(id: u32) -> usize {
    id as usize + 1
}

/// The id a property value stands for; None for no property.
pub fn property_id(value: usize) -> Option<u32> {
    value.checked_sub(1).and_then(|v| u32::try_from(v).ok())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/profiles.rs"]
#[rustfmt::skip]
mod tests;
