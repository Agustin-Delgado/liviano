//! Installed extensions (extensions.json) and what installing one means on
//! disk: a Web Store package verified and unpacked under
//! %APPDATA%\Nea\extensions\<id>, or a developer's folder used where it
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
        if let Some(id) = &extension.engine_id
            && !self.removed.contains(id)
        {
            self.removed.push(id.clone());
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
const LOCALES: &[&str] = &["en", "en_US"];

/// Reads what the app shows of a manifest. `messages` maps a locale folder
/// to its messages.json, for names like `__MSG_extName__`.
pub fn manifest_info(manifest: &str, messages: &dyn Fn(&str) -> Option<String>) -> Result<ManifestInfo, String> {
    let value: Value = serde_json::from_str(manifest).map_err(|e| format!("manifest.json is not valid: {e}"))?;
    let name = text_at(&value, &["name"]).ok_or("manifest.json has no name")?;
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
    let manifest = read_text(&folder.join("manifest.json")).ok_or("the folder has no manifest.json")?;
    manifest_info(&manifest, &|locale| read_text(&folder.join("_locales").join(locale).join("messages.json")))
}

/// Writes the developer's public key into the manifest as `key`, which is
/// what makes the engine give an unpacked store package its store id.
pub fn with_key(manifest: &str, public_key: &[u8]) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(manifest).map_err(|e| format!("manifest.json is not valid: {e}"))?;
    let object = value.as_object_mut().ok_or("manifest.json is not an object")?;
    object.insert("key".to_string(), Value::String(crx::base64(public_key)));
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

// ---------- what the person is asked ----------

/// Host patterns that give access to all sites.
fn all_sites(pattern: &str) -> bool {
    matches!(pattern, "<all_urls>" | "*://*/*" | "http://*/*" | "https://*/*" | "*://*/" | "file:///*")
}

/// The host of a match pattern ("https://*.example.com/*" -> "example.com").
fn pattern_host(pattern: &str) -> Option<String> {
    let rest = pattern.split_once("://")?.1;
    let host = rest.split('/').next()?.trim_start_matches("*.");
    (!host.is_empty() && host != "*").then(|| host.to_string())
}

/// What the person must know before an install: the access the manifest
/// asks for, in the words Chrome uses (shortened). Unknown permissions give
/// no line, as in Chrome. The manifest is untrusted: anything that is not
/// the expected shape is ignored.
pub fn permission_warnings(manifest: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(manifest) else { return Vec::new() };
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).take(500).map(str::to_string).collect())
            .unwrap_or_default()
    };
    let permissions = strings(value.get("permissions"));
    let mut hosts: Vec<String> =
        permissions.iter().filter(|p| p.contains("://") || *p == "<all_urls>").cloned().collect();
    hosts.extend(strings(value.get("host_permissions")));
    if let Some(scripts) = value.get("content_scripts").and_then(Value::as_array) {
        for script in scripts.iter().take(100) {
            hosts.extend(strings(script.get("matches")));
        }
    }
    let mut lines = Vec::new();
    if hosts.iter().any(|h| all_sites(h)) {
        lines.push("Read and change all your data on all websites".to_string());
    } else {
        let mut names: Vec<String> = Vec::new();
        for host in hosts.iter().filter_map(|h| pattern_host(h)) {
            if !names.contains(&host) {
                names.push(host);
            }
        }
        match names.len() {
            0 => {}
            1..=3 => lines.push(format!("Read and change your data on {}", names.join(", "))),
            n => lines.push(format!("Read and change your data on {n} websites")),
        }
    }
    const KNOWN: &[(&str, &str)] = &[
        ("debugger", "Access the page debugger backend"),
        ("history", "Read and change your browsing history"),
        ("tabs", "Read your browsing history"),
        ("webNavigation", "Read your browsing history"),
        ("bookmarks", "Read and change your bookmarks"),
        ("downloads", "Manage your downloads"),
        ("clipboardRead", "Read data that you copy and paste"),
        ("clipboardWrite", "Change data that you copy and paste"),
        ("nativeMessaging", "Communicate with programs on this computer"),
        ("geolocation", "Detect your physical location"),
        ("management", "Manage your apps, extensions and themes"),
        ("privacy", "Change your settings that control privacy"),
        ("proxy", "Read and change all your data on all websites"),
        ("desktopCapture", "Capture the content of your screen"),
        ("tabCapture", "Capture the content of your tabs"),
        ("notifications", "Show notifications"),
        ("declarativeNetRequest", "Block content on any page"),
        ("topSites", "Read a list of your most frequently visited websites"),
        ("pageCapture", "Read and change all your data on all websites"),
        ("contentSettings", "Change your settings that control websites' access to features"),
    ];
    for (name, text) in KNOWN {
        if permissions.iter().any(|p| p == name) && !lines.iter().any(|l| l == text) {
            lines.push(text.to_string());
        }
    }
    lines
}

/// The question before an install, in the app's words.
pub fn consent_text(extension: &Extension, warnings: &[String]) -> String {
    let mut text = format!("Add the extension '{}'?", extension.name);
    if warnings.is_empty() {
        text.push_str("\n\nIt asks for no special access.");
    } else {
        text.push_str("\n\nIt can:");
        for line in warnings {
            text.push_str("\n•  ");
            text.push_str(line);
        }
    }
    let version = if extension.version.is_empty() { String::new() } else { format!("Version {}. ", extension.version) };
    text.push_str(&format!(
        "\n\n{version}From the Chrome Web Store. Some extensions do not work fully in Nea: there is no toolbar and no side panel."
    ));
    text
}

// ---------- installing ----------

/// A store package that is verified and unpacked beside its final folder,
/// waiting for the person to say yes.
#[derive(Debug)]
pub struct Staged {
    pub id: String,
    /// Where the files are now (`<id>.new`).
    pub folder: PathBuf,
    /// The extension as it is after the commit (its path is the final folder).
    pub extension: Extension,
    pub warnings: Vec<String>,
}

/// Verifies a downloaded store package and unpacks it as the extension `id`
/// into a staging folder. Nothing that is installed changes.
pub fn stage_store_package(id: &str, file: &[u8], into: &Path) -> Result<Staged, String> {
    let verified = crx::verify(file, Some(id), true).map_err(|e| e.describe())?;
    let staging = into.join(format!("{id}.new"));
    let _ = fs::remove_dir_all(&staging);
    let result = (|| {
        unzip::extract(verified.archive, &staging)?;
        let manifest_path = staging.join("manifest.json");
        let manifest = read_text(&manifest_path).ok_or("the package has no manifest.json")?;
        fs::write(&manifest_path, with_key(&manifest, &verified.public_key)?).map_err(|e| e.to_string())?;
        Ok::<_, String>((read_manifest(&staging)?, permission_warnings(&manifest)))
    })();
    let (info, warnings) = match result {
        Ok(found) => found,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let mut extension = Extension {
        path: into.join(id).to_string_lossy().into_owned(),
        store_id: Some(id.to_string()),
        engine_id: Some(id.to_string()),
        enabled: true,
        ..Default::default()
    };
    extension.apply(info);
    Ok(Staged { id: id.to_string(), folder: staging, extension, warnings })
}

/// Puts a staged package in its final folder. The previous version (an
/// update) is replaced only now, once the new one is fully on disk.
pub fn commit_staged(staged: Staged) -> Result<Extension, String> {
    let target = PathBuf::from(&staged.extension.path);
    let _ = fs::remove_dir_all(&target);
    if let Err(error) = fs::rename(&staged.folder, &target) {
        let _ = fs::remove_dir_all(&staged.folder);
        return Err(format!("cannot install in {}: {error}", target.display()));
    }
    Ok(staged.extension)
}

/// Deletes a staged package that the person did not want.
pub fn discard_staged(staged: &Staged) {
    let _ = fs::remove_dir_all(&staged.folder);
}

/// Verifies a downloaded store package and installs it as the extension
/// `id`, with no question (the stage and the commit in one step). Only the
/// tests use it: the app asks the person between the two steps.
#[cfg(all(test, nea_tests))]
pub fn unpack_store_package(id: &str, file: &[u8], into: &Path) -> Result<Extension, String> {
    commit_staged(stage_store_package(id, file, into)?)
}

/// A developer's folder, loaded as it is (nothing is copied).
pub fn from_unpacked(folder: &Path) -> Result<Extension, String> {
    let info = read_manifest(folder)?;
    let mut extension = Extension { path: folder.to_string_lossy().into_owned(), enabled: true, ..Default::default() };
    extension.apply(info);
    Ok(extension)
}

/// Re-reads the manifest of an installed extension (after "Reload").
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
    if extension.store_id.is_some()
        && path.parent().is_some_and(|p| same_path(&p.to_string_lossy(), &dir.to_string_lossy()))
    {
        let _ = fs::remove_dir_all(path);
    }
}

// ---------- the worker thread ----------

/// Posted to the window when a store install step finishes; the result
/// waits in `take_finished`.
pub const WM_APP_EXTENSION_INSTALLED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 40;

/// The end of one step of a store install.
#[derive(Debug)]
pub enum Outcome {
    /// Downloaded, verified and unpacked: the person must say yes.
    Staged(Staged),
    /// In its final folder.
    Installed(Extension),
    Failed(String),
}

pub type Finished = (String, Outcome);

static FINISHED: Mutex<Vec<Finished>> = Mutex::new(Vec::new());

fn finish(hwnd: isize, id: String, outcome: Outcome) {
    if let Ok(mut list) = FINISHED.lock() {
        list.push((id, outcome));
    }
    unsafe {
        use windows::Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::PostMessageW,
        };
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_EXTENSION_INSTALLED, WPARAM(0), LPARAM(0));
    }
}

/// Downloads, verifies and stages a store extension off the UI thread, then
/// tells `hwnd`. `cookie` is the store session of the engine profile, for
/// an extension that the store gives only to signed-in persons.
pub fn stage_in_background(hwnd: isize, id: String, browser_version: fn() -> String, cookie: Option<String>) {
    std::thread::spawn(move || {
        let result =
            webstore::download(&webstore::download_url(&id, &browser_version()), cookie.as_deref()).and_then(|file| {
                let dir = extensions_dir();
                fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                stage_store_package(&id, &file, &dir)
            });
        let outcome = match result {
            Ok(staged) => Outcome::Staged(staged),
            Err(error) => Outcome::Failed(error),
        };
        finish(hwnd, id, outcome);
    });
}

/// Commits a staged package off the UI thread (it can delete an old
/// version with many files), then tells `hwnd`.
pub fn commit_in_background(hwnd: isize, staged: Staged) {
    std::thread::spawn(move || {
        let id = staged.id.clone();
        let outcome = match commit_staged(staged) {
            Ok(extension) => Outcome::Installed(extension),
            Err(error) => Outcome::Failed(error),
        };
        finish(hwnd, id, outcome);
    });
}

/// Deletes a staged package off the UI thread.
pub fn discard_in_background(staged: Staged) {
    std::thread::spawn(move || discard_staged(&staged));
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

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/extensions.rs"]
#[rustfmt::skip]
mod tests;
