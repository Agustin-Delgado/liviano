//! Plain JSON files under %APPDATA%\Liviano, read on demand and written
//! atomically (temp file + rename) so a crash never leaves half a file.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};

pub const APP_NAME: &str = "Liviano";
/// What the window, the shortcut and the installed entry are called. A debug
/// build says so, so it is never mistaken for the published one.
pub const DISPLAY_NAME: &str = if cfg!(debug_assertions) { "Liviano (desarrollo)" } else { "Liviano" };

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LIVIANO_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join(APP_NAME)))
        .unwrap_or_else(|| PathBuf::from(".").join(APP_NAME));
    let _ = fs::create_dir_all(&base);
    base
}

/// Engine profile data (cookies, cache) is machine-local, not roaming.
pub fn engine_dir() -> PathBuf {
    let base = std::env::var_os("LIVIANO_DATA_DIR")
        .map(|p| PathBuf::from(p).join("engine"))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(APP_NAME).join("engine")))
        .unwrap_or_else(|| data_dir().join("engine"));
    let _ = fs::create_dir_all(&base);
    base
}

pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

