//! The daily update. A thread of its own looks at a manifest once a
//! day, downloads a newer package next to the app's data, and keeps it only
//! if its Ed25519 signature verifies against the key built into this binary.
//! Nothing is ever installed while the app runs and nothing restarts it: the
//! next launch swaps the executable before the window exists
//! (`apply_pending`) and carries on with the new one.
//!
//! Every failure is silent: a manifest that doesn't answer or doesn't parse
//! leaves the app exactly as it was, and the next try is a day later.

use std::{
    fs,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ed25519_compact::{PublicKey, Signature};
use serde::{Deserialize, Serialize};

use crate::{http, settings::Settings, storage, update_sign};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub account that publishes the releases (github.com/<owner>/nea).
/// Empty means no updates: with no owner and no address in settings.json the
/// updater never goes out.
pub const RELEASES_OWNER: &str = "Agustin-Delgado";

const DAY_SECS: u64 = 24 * 60 * 60;
/// The thread waits this long after launch, so the check never competes with
/// the first page for the network.
const FIRST_DELAY: Duration = Duration::from_secs(20);
/// How often the thread wakes to see whether a check is due (and whether it
/// was turned off meanwhile).
const WAKE_EVERY: Duration = Duration::from_secs(60 * 60);
const MANIFEST_LIMIT: u64 = 64 * 1024;
const PACKAGE_LIMIT: u64 = 128 * 1024 * 1024;

static ENABLED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static MANIFEST_URL: Mutex<String> = Mutex::new(String::new());

/// latest.json, as published next to the package.
#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    /// Absolute, or relative to the manifest's own address.
    url: String,
    /// Hex of the 64-byte Ed25519 signature (see `update_sign`).
    signature: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Unix seconds of the last attempt, successful or not.
    last_check: u64,
    /// What the last attempt ended in, for whoever wonders why nothing came.
    last_result: String,
    pending: Option<Pending>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    version: String,
    /// File name inside the update folder.
    file: String,
    signature: String,
}

/// The publisher's key. Replacing keys/update-public.hex is how a
/// production key is put in (README, "Sign a release").
fn public_key() -> Option<PublicKey> {
    let bytes = update_sign::from_hex(include_str!("../keys/update-public.hex"))?;
    PublicKey::from_slice(&bytes).ok()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn state_path() -> PathBuf {
    storage::data_dir().join("update.json")
}

/// Downloads are machine-local, beside the engine profile.
pub fn update_dir() -> PathBuf {
    storage::local_root().join("update")
}

pub fn default_manifest_url() -> Option<String> {
    (!RELEASES_OWNER.is_empty())
        .then(|| format!("https://github.com/{RELEASES_OWNER}/nea/releases/latest/download/latest.json"))
}

/// The address a check goes to, if any. A development build only checks
/// when settings.json names an address explicitly, so it never replaces
/// itself with a published release.
fn manifest_url(settings: &Settings) -> Option<String> {
    let configured = settings.update_url.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    if cfg!(debug_assertions) { None } else { default_manifest_url() }
}

/// "1.2.3" (a leading "v" is fine) as numbers. A pre-release suffix is not
/// a version this updater offers, so it doesn't parse.
pub fn parse_version(text: &str) -> Option<Vec<u64>> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    if text.is_empty() {
        return None;
    }
    let mut parts: Vec<u64> = text.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if parts.len() > 4 {
        return None;
    }
    while parts.last() == Some(&0) {
        parts.pop();
    }
    Some(parts)
}

/// True only when both parse and `candidate` is strictly greater.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Once a day; a clock that went backwards counts as due, or a wrong date
/// set once would silence the updater for good.
fn due(last_check: u64, now: u64) -> bool {
    now < last_check || now - last_check >= DAY_SECS
}

/// Streams the file through the verifier: the package is never held whole
/// in memory.
fn verify_file(path: &Path, version: &str, signature_hex: &str, key: &PublicKey) -> bool {
    let Some(signature) = update_sign::from_hex(signature_hex).and_then(|b| Signature::from_slice(&b).ok()) else {
        return false;
    };
    let Ok(mut state) = key.verify_incremental(&signature) else { return false };
    state.absorb(update_sign::signed_prefix(version));
    let Ok(mut file) = fs::File::open(path) else { return false };
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => state.absorb(&buffer[..n]),
            Err(_) => return false,
        }
    }
    state.verify().is_ok()
}

/// A name the state file may point at: inside the update folder, nothing more.
fn safe_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\', ':']) && !name.contains("..")
}

/// Settings changed: turning the check off takes effect at the next wake.
pub fn configure(settings: &Settings) {
    let url = manifest_url(settings);
    ENABLED.store(settings.check_updates && url.is_some(), Ordering::Relaxed);
    if let Ok(mut slot) = MANIFEST_URL.lock() {
        *slot = url.unwrap_or_default();
    }
}

/// Starts the background thread (once). It holds no window and no COM.
/// With no address to ask there is no thread at all: the address only
/// changes by editing settings.json, which takes a restart anyway.
pub fn start(settings: &Settings) {
    configure(settings);
    if manifest_url(settings).is_none() || STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new().name("updater".into()).stack_size(256 * 1024).spawn(|| {
        std::thread::sleep(FIRST_DELAY);
        loop {
            tick(unix_now());
            std::thread::sleep(WAKE_EVERY);
        }
    });
}

fn tick(now: u64) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let url = MANIFEST_URL.lock().map(|u| u.clone()).unwrap_or_default();
    if url.is_empty() {
        return;
    }
    let path = state_path();
    let mut state: State = storage::read_json(&path);
    if !due(state.last_check, now) {
        return;
    }
    // Recorded before going out, so a check that hangs or crashes still
    // counts as today's.
    state.last_check = now;
    let _ = storage::write_json(&path, &state);
    let Some(key) = public_key() else { return };
    let result = check(&url, state.pending.as_ref(), &update_dir(), &key);
    state.last_result = match result {
        Ok(Some(pending)) => {
            let line = format!("{} is ready to install", pending.version);
            state.pending = Some(pending);
            line
        }
        Ok(None) => "no new version".to_string(),
        Err(error) => error,
    };
    let _ = storage::write_json(&path, &state);
}

/// One look at the manifest; downloads and verifies the package when it is
/// newer than this build and than what is already waiting.
fn check(url: &str, waiting: Option<&Pending>, dir: &Path, key: &PublicKey) -> Result<Option<Pending>, String> {
    let mut body = Vec::new();
    http::get(url, MANIFEST_LIMIT, &mut body)?;
    let manifest: Manifest = serde_json::from_slice(&body).map_err(|e| format!("unreadable manifest: {e}"))?;
    if parse_version(&manifest.version).is_none() {
        return Err(format!("unreadable version: {}", manifest.version));
    }
    if !is_newer(&manifest.version, CURRENT_VERSION) {
        return Ok(None);
    }
    if let Some(waiting) = waiting
        && !is_newer(&manifest.version, &waiting.version)
        && dir.join(&waiting.file).exists()
    {
        return Ok(None);
    }
    let package_url = http::resolve(url, &manifest.url);
    let name = format!("nea-{}.exe", manifest.version.trim());
    if !safe_file_name(&name) {
        return Err("invalid package name".into());
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let partial = dir.join(format!("{name}.part"));
    let limit = manifest.size.unwrap_or(PACKAGE_LIMIT).min(PACKAGE_LIMIT);
    let downloaded = (|| {
        let mut file = BufWriter::new(fs::File::create(&partial).map_err(|e| e.to_string())?);
        let written = http::get(&package_url, limit, &mut file)?;
        file.flush().map_err(|e| e.to_string())?;
        Ok::<u64, String>(written)
    })();
    let written = match downloaded {
        Ok(written) => written,
        Err(error) => {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
    if manifest.size.is_some_and(|size| size != written)
        || !verify_file(&partial, &manifest.version, &manifest.signature, key)
    {
        let _ = fs::remove_file(&partial);
        return Err(format!("{}: the signature does not verify, discarded", manifest.version));
    }
    // Only the newest package is kept.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path() != partial {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let ready = dir.join(&name);
    fs::rename(&partial, &ready).map_err(|e| e.to_string())?;
    Ok(Some(Pending { version: manifest.version.trim().to_string(), file: name, signature: manifest.signature }))
}

/// The version waiting for the next launch, for Settings to mention.
pub fn pending_version() -> Option<String> {
    let state: State = storage::read_json(&state_path());
    let pending = state.pending?;
    (safe_file_name(&pending.file) && update_dir().join(&pending.file).exists()).then_some(pending.version)
}

/// Where the replaced executable goes until the launch after.
pub fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Puts `new` in place of `target`. A running executable can be renamed but
/// not overwritten, so the current one steps aside first, and comes back if
/// the copy fails.
pub fn replace_exe(target: &Path, new: &Path) -> std::io::Result<()> {
    let old = old_path(target);
    let _ = fs::remove_file(&old);
    if target.exists() {
        fs::rename(target, &old)?;
    }
    if let Err(error) = fs::copy(new, target) {
        let _ = fs::remove_file(target);
        let _ = fs::rename(&old, target);
        return Err(error);
    }
    Ok(())
}

/// First thing at launch. Removes what the previous update left and, when a
/// verified package is waiting, swaps it in and starts it with the same
/// arguments. True means the new executable is running and this one should
/// just exit.
pub fn apply_pending() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let _ = fs::remove_file(old_path(&exe));
    let path = state_path();
    if !path.exists() {
        return false;
    }
    let mut state: State = storage::read_json(&path);
    let Some(pending) = state.pending.take() else { return false };
    // Cleared before trying: a package that can't be applied is not retried
    // at every launch.
    let _ = storage::write_json(&path, &state);
    if !safe_file_name(&pending.file) {
        return false;
    }
    let file = update_dir().join(&pending.file);
    // Verified again: the folder is writable by anything the user runs.
    let verified = is_newer(&pending.version, CURRENT_VERSION)
        && public_key().is_some_and(|key| verify_file(&file, &pending.version, &pending.signature, &key));
    let applied = verified && replace_exe(&exe, &file).is_ok();
    let _ = fs::remove_file(&file);
    applied && std::process::Command::new(&exe).args(std::env::args_os().skip(1)).spawn().is_ok()
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/updater.rs"]
#[rustfmt::skip]
mod tests;
