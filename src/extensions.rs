//! Installed extensions (extensions.json) and what installing one means on
//! disk: a Web Store package verified and unpacked under
//! %APPDATA%\Liviano\extensions\<id>, or a developer's folder used where it
//! is. The engine side (WebView2's profile) is `extension_host`; this file
//! never touches it, so the rules are tested without a window.
//!
//! Extensions cost memory (each one may run its own processes), so the
//! engine only starts with extension support when at least one is enabled.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{crx, storage, unzip, webstore};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extension {
    /// The folder the engine loads.
    pub path: String,
    pub name: String,
    pub version: String,
    /// Set for a Web Store install; an unpacked folder has none.
    pub store_id: Option<String>,
    /// The id the engine gave it once loaded (the store id for store
    /// installs, since the manifest carries the store key).
    pub engine_id: Option<String>,
    pub enabled: bool,
    /// Shown by its initial next to the extensions button.
    pub pinned: bool,
    /// Pages of the extension, relative to its root.
    pub popup: Option<String>,
    pub options: Option<String>,
    pub side_panel: Option<String>,
}

impl Extension {
    pub fn unpacked(&self) -> bool {
        self.store_id.is_none()
    }

    pub fn initial(&self) -> char {
        self.name.chars().find(|c| c.is_alphanumeric()).unwrap_or('·').to_uppercase().next().unwrap_or('·')
    }

    /// `chrome-extension://<id>/<page>`, once the engine knows the id.
    pub fn page_url(&self, page: &str) -> Option<String> {
        let id = self.engine_id.as_deref()?;
        Some(format!("chrome-extension://{id}/{}", page.trim_start_matches('/')))
    }

    fn apply(&mut self, info: ManifestInfo) {
        self.name = info.name;
        self.version = info.version;
        self.popup = info.popup;
        self.options = info.options;
        self.side_panel = info.side_panel;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub extensions: Vec<Extension>,
    /// Engine ids to remove from the profile the next time it is reachable
    /// (removed while the engine ran without extension support).
    pub removed: Vec<String>,
}

pub fn registry_path() -> PathBuf {
    storage::data_dir().join("extensions.json")
}

/// Where store packages are unpacked.
pub fn extensions_dir() -> PathBuf {
    storage::data_dir().join("extensions")
}

impl Registry {
    pub fn load() -> Self {
        storage::read_json(&registry_path())
    }

    pub fn save(&self) {
        let _ = storage::write_json(&registry_path(), self);
    }

    /// Whether the engine should start with extension support.
    pub fn any_enabled(&self) -> bool {
        self.extensions.iter().any(|e| e.enabled)
    }

    pub fn find_store(&self, id: &str) -> Option<usize> {
        self.extensions.iter().position(|e| e.store_id.as_deref() == Some(id))
    }

    pub fn find_path(&self, path: &str) -> Option<usize> {
        self.extensions.iter().position(|e| same_path(&e.path, path))
    }

    pub fn find_engine(&self, id: &str) -> Option<usize> {
        self.extensions.iter().position(|e| e.engine_id.as_deref() == Some(id))
    }

    /// Adds a new install or replaces the one it updates (same store id or
    /// folder), keeping what the person chose (enabled, pinned).
    pub fn upsert(&mut self, mut extension: Extension) -> usize {
        let existing = match &extension.store_id {
            Some(id) => self.find_store(id),
            None => self.find_path(&extension.path),
        };
        match existing {
            Some(i) => {
                let old = &self.extensions[i];
                extension.pinned = old.pinned;
                extension.engine_id = extension.engine_id.or_else(|| old.engine_id.clone());
                self.extensions[i] = extension;
                i
            }
            None => {
                self.extensions.push(extension);
                self.extensions.len() - 1
            }
        }
    }

    /// Takes the extension out; its engine id is kept for the profile.
    pub fn remove(&mut self, index: usize) -> Option<Extension> {
        if index >= self.extensions.len() {
            return None;
        }
        let extension = self.extensions.remove(index);
        if let Some(id) = &extension.engine_id {
            if !self.removed.contains(id) {
                self.removed.push(id.clone());
            }
        }
        Some(extension)
    }

    /// The pinned ones, in list order: (index, initial).
    pub fn pinned(&self) -> Vec<(usize, char)> {
        self.extensions.iter().enumerate().filter(|(_, e)| e.pinned).map(|(i, e)| (i, e.initial())).collect()
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |p: &str| p.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

// ---------- manifest ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManifestInfo {
    pub name: String,
    pub version: String,
    pub popup: Option<String>,
    pub options: Option<String>,
    pub side_panel: Option<String>,
}

fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut v = value;
    for key in path {
        v = v.get(key)?;
    }
    v.as_str().filter(|s| !s.trim().is_empty()).map(str::to_string)
}

/// The UI languages to try for `__MSG_name__`, best first.
const LOCALES: &[&str] = &["es_419", "es", "en", "en_US"];

/// Reads what the app shows of a manifest. `messages` maps a locale folder
/// to its messages.json, for names like `__MSG_extName__`.
pub fn manifest_info(manifest: &str, messages: &dyn Fn(&str) -> Option<String>) -> Result<ManifestInfo, String> {
    let value: Value = serde_json::from_str(manifest).map_err(|e| format!("manifest.json no es válido: {e}"))?;
    let name = text_at(&value, &["name"]).ok_or("al manifest.json le falta el nombre")?;
    let default_locale = text_at(&value, &["default_locale"]);
    let name = localize(&name, default_locale.as_deref(), messages);
    Ok(ManifestInfo {
        name,
        version: text_at(&value, &["version"]).unwrap_or_default(),
        popup: text_at(&value, &["action", "default_popup"])
            .or_else(|| text_at(&value, &["browser_action", "default_popup"]))
            .or_else(|| text_at(&value, &["page_action", "default_popup"])),
        options: text_at(&value, &["options_ui", "page"]).or_else(|| text_at(&value, &["options_page"])),
        side_panel: text_at(&value, &["side_panel", "default_path"]),
    })
}

/// `__MSG_key__` replaced from the first locale that has it (keys are
/// case-insensitive, as in Chromium).
fn localize(text: &str, default_locale: Option<&str>, messages: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(key) = text.strip_prefix("__MSG_").and_then(|k| k.strip_suffix("__")) else {
        return text.to_string();
    };
    let key = key.to_lowercase();
    for locale in LOCALES.iter().copied().chain(default_locale) {
        let Some(json) = messages(locale) else { continue };
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&json) else { continue };
        let found = map.iter().find(|(k, _)| k.to_lowercase() == key).and_then(|(_, v)| text_at(v, &["message"]));
        if let Some(found) = found {
            return found;
        }
    }
    text.to_string()
}

/// Strips a UTF-8 byte order mark (some manifests have one).
fn read_text(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
}

pub fn read_manifest(folder: &Path) -> Result<ManifestInfo, String> {
    let manifest = read_text(&folder.join("manifest.json")).ok_or("la carpeta no tiene manifest.json")?;
    manifest_info(&manifest, &|locale| read_text(&folder.join("_locales").join(locale).join("messages.json")))
}

/// Writes the developer's public key into the manifest as `key`, which is
/// what makes the engine give an unpacked store package its store id.
pub fn with_key(manifest: &str, public_key: &[u8]) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(manifest).map_err(|e| format!("manifest.json no es válido: {e}"))?;
    let object = value.as_object_mut().ok_or("manifest.json no es un objeto")?;
    object.insert("key".to_string(), Value::String(crx::base64(public_key)));
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

// ---------- installing ----------

/// Verifies a downloaded store package and unpacks it as the extension `id`.
/// The previous version (an update) is replaced only once the new one is
/// fully on disk.
pub fn unpack_store_package(id: &str, file: &[u8], into: &Path) -> Result<Extension, String> {
    let verified = crx::verify(file, Some(id), true).map_err(|e| e.describe())?;
    let staging = into.join(format!("{id}.new"));
    let _ = fs::remove_dir_all(&staging);
    let result = (|| {
        unzip::extract(verified.archive, &staging)?;
        let manifest_path = staging.join("manifest.json");
        let manifest = read_text(&manifest_path).ok_or("el paquete no trae manifest.json")?;
        fs::write(&manifest_path, with_key(&manifest, &verified.public_key)?).map_err(|e| e.to_string())?;
        read_manifest(&staging)
    })();
    let info = match result {
        Ok(info) => info,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let target = into.join(id);
    let _ = fs::remove_dir_all(&target);
    fs::rename(&staging, &target).map_err(|e| format!("no se pudo instalar en {}: {e}", target.display()))?;
    let mut extension = Extension {
        path: target.to_string_lossy().into_owned(),
        store_id: Some(id.to_string()),
        engine_id: Some(id.to_string()),
        enabled: true,
        ..Default::default()
    };
    extension.apply(info);
    Ok(extension)
}

/// A developer's folder, loaded as it is (nothing is copied).
pub fn from_unpacked(folder: &Path) -> Result<Extension, String> {
    let info = read_manifest(folder)?;
    let mut extension =
        Extension { path: folder.to_string_lossy().into_owned(), enabled: true, ..Default::default() };
    extension.apply(info);
    Ok(extension)
}

/// Re-reads the manifest of an installed extension (after "Recargar").
pub fn refresh(extension: &mut Extension) -> Result<(), String> {
    let info = read_manifest(Path::new(&extension.path))?;
    extension.apply(info);
    Ok(())
}

/// Store folders no extension points at any more (a removal whose folder
/// was still in use, a failed install): deleted at startup.
pub fn sweep(registry: &Registry) {
    let Ok(entries) = fs::read_dir(extensions_dir()) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let path_text = path.to_string_lossy();
        if path.is_dir() && registry.find_path(&path_text).is_none() {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Deletes the files of a removed extension, only if the app put them there.
pub fn delete_files(extension: &Extension) {
    let dir = extensions_dir();
    let path = Path::new(&extension.path);
    if extension.store_id.is_some() && path.parent().is_some_and(|p| same_path(&p.to_string_lossy(), &dir.to_string_lossy())) {
        let _ = fs::remove_dir_all(path);
    }
}

// ---------- the worker thread ----------

/// Posted to the window when a store install finishes; the result waits in
/// `take_finished`.
pub const WM_APP_EXTENSION_INSTALLED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;

pub type Finished = (String, Result<Extension, String>);

static FINISHED: Mutex<Vec<Finished>> = Mutex::new(Vec::new());

/// Downloads, verifies and unpacks a store extension off the UI thread,
/// then tells `hwnd`.
pub fn install_in_background(hwnd: isize, id: String, browser_version: String) {
    std::thread::spawn(move || {
        let result = webstore::download(&webstore::download_url(&id, &browser_version)).and_then(|file| {
            let dir = extensions_dir();
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            unpack_store_package(&id, &file, &dir)
        });
        if let Ok(mut list) = FINISHED.lock() {
            list.push((id, result));
        }
        unsafe {
            use windows::Win32::{
                Foundation::{HWND, LPARAM, WPARAM},
                UI::WindowsAndMessaging::PostMessageW,
            };
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_EXTENSION_INSTALLED, WPARAM(0), LPARAM(0));
        }
    });
}

pub fn take_finished() -> Vec<Finished> {
    FINISHED.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

/// What the engine reported about its profile: engine id -> (name, enabled).
pub type EngineList = HashMap<String, (String, bool)>;

/// The engine-side work that brings the profile in line with the registry.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Folders to load (index in the registry).
    pub add: Vec<usize>,
    pub enable: Vec<(String, bool)>,
    pub remove: Vec<String>,
}

/// Compares the registry with the engine's list. Only ids this app added are
/// ever removed: the engine ships its own (PDF viewer, clipboard).
pub fn plan(registry: &Registry, engine: &EngineList) -> Plan {
    let mut plan = Plan::default();
    for id in &registry.removed {
        if engine.contains_key(id) && registry.find_engine(id).is_none() {
            plan.remove.push(id.clone());
        }
    }
    for (index, extension) in registry.extensions.iter().enumerate() {
        match extension.engine_id.as_ref().and_then(|id| engine.get(id).map(|state| (id, state))) {
            Some((id, (_, on))) if *on != extension.enabled => plan.enable.push((id.clone(), extension.enabled)),
            Some(_) => {}
            None if extension.enabled => plan.add.push(index),
            None => {}
        }
    }
    plan
}

