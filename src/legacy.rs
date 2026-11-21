//! The browser used to be called Liviano. Whoever ran it then has their
//! tabs, history, settings, bookmarks, extensions and engine profile under
//! %APPDATA%\Liviano and %LOCALAPPDATA%\Liviano, and their passwords in
//! Credential Manager under "Liviano:". At launch, before anything reads a
//! file, this moves all of it to the new name, once:
//!
//! - A folder is renamed (one atomic step). If it is in use and the rename
//!   fails, it is copied to a staging folder that only takes the new name
//!   when the copy is whole; the old one stays. If even that fails, this
//!   session uses the old folder as it is and the next launch tries again.
//! - When the new folder already exists it wins and nothing is touched.
//! - Passwords go through `vault::Store::take_from`: written, read back,
//!   and only then deleted under the old prefix.
//!
//! The installed copy is `installer::move_legacy_install`.
//!
//! A run with NEA_DATA_DIR (tests, scripts) never reaches the real folders
//! or the real passwords: it migrates from LIVIANO_DATA_DIR and
//! LIVIANO_CREDENTIAL_PREFIX only when those are set too.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{storage, vault};

/// The former name, as folders and credentials carry it.
pub const NAME: &str = "Liviano";
const DATA_DIR_VAR: &str = "LIVIANO_DATA_DIR";
const CREDENTIAL_PREFIX_VAR: &str = "LIVIANO_CREDENTIAL_PREFIX";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    /// Nothing to move: no old folder, or the new one already exists.
    Nothing,
    Renamed,
    /// Copied whole; the old folder is still there.
    Copied,
    /// Neither worked; both folders are as they were.
    Failed,
}

/// Moves `old` to `new` when only `old` exists.
pub fn move_dir(old: &Path, new: &Path) -> Moved {
    if new.exists() || !old.is_dir() {
        return Moved::Nothing;
    }
    if let Some(parent) = new.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // A process that is just exiting (the old version that started this
    // one after an update) may hold a handle for a moment.
    if rename_with_retries(old, new) {
        return Moved::Renamed;
    }
    let staging = staging_path(new);
    let _ = fs::remove_dir_all(&staging);
    // A virus scanner can still hold a file that was just copied: retry the
    // last rename the same way.
    if copy_tree(old, &staging).is_ok() && rename_with_retries(&staging, new) {
        return Moved::Copied;
    }
    let _ = fs::remove_dir_all(&staging);
    Moved::Failed
}

/// Five tries, 200 ms apart.
fn rename_with_retries(from: &Path, to: &Path) -> bool {
    for attempt in 0..5 {
        if fs::rename(from, to).is_ok() {
            return true;
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    false
}

fn staging_path(new: &Path) -> PathBuf {
    let mut name = new.file_name().unwrap_or_default().to_os_string();
    name.push(".migrating");
    new.with_file_name(name)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// extensions.json keeps absolute folders, and the Web Store ones were
/// unpacked inside the old data folder: they now point into the new one.
/// The engine profile still names the old folders, so it finds nothing
/// there and the registry loads them again from the new path (same id,
/// since the manifest carries the store key).
pub fn repoint_extensions(new_root: &Path, old_root: &Path) {
    let path = new_root.join("extensions.json");
    let Ok(bytes) = fs::read(&path) else { return };
    let Ok(mut registry) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return };
    let old_prefix = old_root.to_string_lossy().to_lowercase();
    let mut changed = false;
    if let Some(list) = registry.get_mut("extensions").and_then(|e| e.as_array_mut()) {
        for extension in list {
            let Some(folder) = extension.get("path").and_then(|p| p.as_str()) else { continue };
            let Some(rest) = strip_dir_prefix(folder, &old_prefix) else { continue };
            let moved = new_root.join(rest.trim_start_matches(['\\', '/']));
            extension["path"] = serde_json::Value::String(moved.to_string_lossy().into_owned());
            changed = true;
        }
    }
    if changed && let Ok(bytes) = serde_json::to_vec(&registry) {
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, bytes).is_ok() {
            let _ = fs::rename(&tmp, &path);
        }
    }
}

/// The part of `path` after the folder `prefix` (lowercase), if inside it.
fn strip_dir_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let head = path.get(..prefix.len())?;
    let rest = &path[prefix.len()..];
    (head.to_lowercase() == prefix && (rest.is_empty() || rest.starts_with(['\\', '/']))).then_some(rest)
}

/// (old, new) roaming folders, then local ones; one pair when a single
/// folder holds everything.
fn folder_pairs() -> Vec<(PathBuf, PathBuf)> {
    let new_roaming = storage::default_roaming_root();
    let new_local = storage::default_local_root();
    if std::env::var_os("NEA_DATA_DIR").is_some() {
        return std::env::var_os(DATA_DIR_VAR).map(|old| vec![(PathBuf::from(old), new_roaming)]).unwrap_or_default();
    }
    let mut pairs = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        pairs.push((PathBuf::from(appdata).join(NAME), new_roaming.clone()));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA")
        && new_local != new_roaming
    {
        pairs.push((PathBuf::from(local).join(NAME), new_local));
    }
    pairs
}

/// The old corner of Credential Manager, when this run may touch it.
fn old_store() -> Option<vault::Store> {
    if vault::prefix_from_env("NEA_CREDENTIAL_PREFIX").is_some() {
        return vault::prefix_from_env(CREDENTIAL_PREFIX_VAR).map(|p| vault::Store::with_prefix(&p));
    }
    if std::env::var_os("NEA_DATA_DIR").is_some() {
        return None;
    }
    Some(vault::Store::with_prefix(&format!("{NAME}:")))
}

/// First thing at launch, before any file is read.
pub fn migrate() {
    let pairs = folder_pairs();
    let single = pairs.len() == 1 && storage::default_roaming_root() == storage::default_local_root();
    for (index, (old, new)) in pairs.iter().enumerate() {
        let moved = move_dir(old, new);
        if matches!(moved, Moved::Renamed | Moved::Copied) && index == 0 {
            repoint_extensions(new, old);
        }
        if moved == Moved::Failed {
            let instead = Some(old.clone());
            match (index, single) {
                (0, true) => storage::use_instead(instead.clone(), instead),
                (0, false) => storage::use_instead(instead, None),
                _ => storage::use_instead(None, instead),
            }
        }
    }
    if let Some(old) = old_store() {
        vault::Store::default().take_from(&old);
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/legacy.rs"]
#[rustfmt::skip]
mod tests;
