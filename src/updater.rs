//! The update check. A thread of its own reads a manifest soon after launch
//! and then every few hours. It downloads a newer package next to the app's
//! data, and keeps it only if its Ed25519 signature verifies against the key
//! built into this binary. Nothing is installed while the app runs. The
//! next launch swaps the executable before the window exists
//! (`apply_pending`) and continues with the new one. When a package waits,
//! the chrome shows a "Restart to update" chip: a click saves the session,
//! starts the executable again and closes this process (`request_restart`,
//! `relaunch`).
//!
//! Every failure is silent: a manifest that does not answer or does not
//! parse leaves the app as it was, and the next try comes at the next wake.
//!
//! A build that runs from a cargo target folder never downloads and never
//! swaps the executable: a development build stays under the developer.

use std::{
    ffi::OsString,
    fs,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
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

/// The time between two checks while the app runs. People keep the app
/// open for days, so a check only at launch is not sufficient.
const CHECK_EVERY: u64 = 4 * 60 * 60;
/// A launch checks at once, unless a check ran less than this time before:
/// a quick restart (or a crash loop) does not ask the server each time.
const LAUNCH_GAP: u64 = 10 * 60;
/// The thread waits this long after launch, so the check never competes with
/// the first page for the network.
const FIRST_DELAY: Duration = Duration::from_secs(20);
/// How often the thread wakes to see whether a check is due (and whether it
/// was turned off meanwhile).
const WAKE_EVERY: Duration = Duration::from_secs(30 * 60);
const MANIFEST_LIMIT: u64 = 64 * 1024;
const PACKAGE_LIMIT: u64 = 128 * 1024 * 1024;

/// The variable that a restarted process gets: the process ID of the
/// instance that started it. The new instance waits until that one is gone
/// (refer to `wait_for_previous`).
const WAIT_VAR: &str = "NEA_RESTART_WAIT";
/// The longest time that a restarted instance waits for the previous one.
const WAIT_LIMIT_MS: u32 = 20_000;

static ENABLED: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static MANIFEST_URL: Mutex<String> = Mutex::new(String::new());
/// The version of the verified package that waits, for the chip. The
/// updater thread sets it; the UI thread only reads it, from memory.
static READY: Mutex<Option<String>> = Mutex::new(None);
/// The window that gets `MESSAGE` when `READY` changes.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static MESSAGE: AtomicU32 = AtomicU32::new(0);
static RESTART: AtomicBool = AtomicBool::new(false);

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
    /// The `ETag` of the last manifest that was read completely. The next
    /// check sends it, and a 304 answer costs no body.
    etag: String,
    /// The version that read that manifest. A tag from another version
    /// is not sent: the same manifest can mean a different result.
    etag_version: String,
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

/// The address a check goes to, if any. A debug build only checks when
/// settings.json names an address explicitly, so it never replaces itself
/// with a published release.
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

/// Whether a check is due. The first wake after launch (`launch`) checks
/// unless a check ran in the last `LAUNCH_GAP`; each later wake checks
/// every `CHECK_EVERY`. A clock that went backwards counts as due, or a
/// wrong date set once would stop the updater for good.
fn due(last_check: u64, now: u64, launch: bool) -> bool {
    let gap = if launch { LAUNCH_GAP } else { CHECK_EVERY };
    now < last_check || now - last_check >= gap
}

/// The cargo target folder that `exe` runs from, if it runs from one: a
/// `target\release` or `target\debug` folder with a Cargo.toml above it,
/// or a profile folder of a custom `--target-dir` (cargo puts `deps` and
/// `.fingerprint` next to the executable).
pub fn dev_build(exe: &Path) -> Option<PathBuf> {
    let is = |dir: &Path, name: &str| dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name));
    for dir in exe.ancestors().skip(1) {
        if !(is(dir, "release") || is(dir, "debug")) {
            continue;
        }
        let Some(target) = dir.parent() else { continue };
        if is(target, "target") && target.ancestors().skip(1).any(|up| up.join("Cargo.toml").is_file()) {
            return Some(target.to_path_buf());
        }
    }
    let dir = exe.parent()?;
    (dir.join("deps").is_dir() && dir.join(".fingerprint").is_dir()).then(|| dir.to_path_buf())
}

/// What update.json says about a development build.
fn dev_build_result(target: &Path) -> String {
    format!("development build in {}: no download, no swap", target.display())
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

/// The window that is told (with a posted `message`) when a package is
/// ready or is not ready any more.
pub fn notify(window: isize, message: u32) {
    MESSAGE.store(message, Ordering::Relaxed);
    WINDOW.store(window, Ordering::Release);
}

/// The version of the verified package that waits for a restart. It comes
/// from memory: the UI thread can ask at each layout.
pub fn ready_version() -> Option<String> {
    READY.lock().ok().and_then(|r| r.clone())
}

fn set_ready(version: Option<String>) {
    let changed = match READY.lock() {
        Ok(mut slot) if *slot != version => {
            *slot = version;
            true
        }
        _ => false,
    };
    let window = WINDOW.load(Ordering::Acquire);
    if changed && window != 0 {
        use windows::Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::PostMessageW,
        };
        unsafe {
            let _ = PostMessageW(Some(HWND(window as *mut _)), MESSAGE.load(Ordering::Relaxed), WPARAM(0), LPARAM(0));
        }
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
        let mut launch = true;
        loop {
            tick(unix_now(), launch);
            launch = false;
            std::thread::sleep(WAKE_EVERY);
        }
    });
}

fn tick(now: u64, launch: bool) {
    match std::env::current_exe() {
        Ok(exe) => tick_for(&exe, now, launch),
        Err(_) => set_ready(None),
    }
}

/// One wake of the thread, for the executable at `exe`.
fn tick_for(exe: &Path, now: u64, launch: bool) {
    let path = state_path();
    let mut state: State = storage::read_json(&path);
    if let Some(target) = dev_build(exe) {
        set_ready(None);
        let result = dev_build_result(&target);
        if ENABLED.load(Ordering::Relaxed) && state.last_result != result {
            state.last_check = now;
            state.last_result = result;
            let _ = storage::write_json(&path, &state);
        }
        return;
    }
    // A package that is ready stays ready, also with the check off.
    set_ready(ready_in(&state, &update_dir()));
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let url = MANIFEST_URL.lock().map(|u| u.clone()).unwrap_or_default();
    if url.is_empty() || !due(state.last_check, now, launch) {
        return;
    }
    // Recorded before going out, so a check that hangs or crashes still
    // counts as this one.
    state.last_check = now;
    let _ = storage::write_json(&path, &state);
    let Some(key) = public_key() else { return };
    let dir = update_dir();
    let etag = (state.etag_version == CURRENT_VERSION).then_some(state.etag.as_str()).filter(|t| !t.is_empty());
    let result = check(&url, state.pending.as_ref(), &dir, &key, etag);
    state.last_result = match result {
        Ok(Checked { pending, etag }) => {
            state.etag = etag.unwrap_or_default();
            state.etag_version = CURRENT_VERSION.to_string();
            match pending {
                Some(pending) => {
                    let line = format!("{} is ready to install", pending.version);
                    state.pending = Some(pending);
                    line
                }
                None => "no new version".to_string(),
            }
        }
        Err(error) => {
            // The next check reads the whole manifest again.
            state.etag.clear();
            error
        }
    };
    let _ = storage::write_json(&path, &state);
    set_ready(ready_in(&state, &dir));
}

/// The version of the package that `state` names, if it is newer than this
/// build and its file is in `dir`.
fn ready_in(state: &State, dir: &Path) -> Option<String> {
    let pending = state.pending.as_ref()?;
    (is_newer(&pending.version, CURRENT_VERSION) && safe_file_name(&pending.file) && dir.join(&pending.file).is_file())
        .then(|| pending.version.clone())
}

/// What one check found.
#[derive(Debug)]
struct Checked {
    /// A new package that verifies, if one came.
    pending: Option<Pending>,
    /// The tag to send next time: the new one, or the old one after a 304.
    etag: Option<String>,
}

/// One look at the manifest; downloads and verifies the package when it is
/// newer than this build and than what is already waiting.
fn check(
    url: &str,
    waiting: Option<&Pending>,
    dir: &Path,
    key: &PublicKey,
    etag: Option<&str>,
) -> Result<Checked, String> {
    // A tag is sent only when a waiting package is still there: else the
    // same manifest must give the package again.
    let etag = etag.filter(|_| waiting.is_none_or(|w| safe_file_name(&w.file) && dir.join(&w.file).is_file()));
    let mut body = Vec::new();
    let new_etag = match http::fetch(url, MANIFEST_LIMIT, &mut body, etag)? {
        http::Fetched::NotModified => {
            return Ok(Checked { pending: None, etag: etag.map(str::to_string) });
        }
        http::Fetched::Body { etag, .. } => (!etag.is_empty()).then_some(etag),
    };
    let manifest: Manifest = serde_json::from_slice(&body).map_err(|e| format!("unreadable manifest: {e}"))?;
    let pending = package(url, &manifest, waiting, dir, key)?;
    Ok(Checked { pending, etag: new_etag })
}

/// Downloads and verifies the package of `manifest`, if it is newer than
/// this build and than the package that waits.
fn package(
    url: &str,
    manifest: &Manifest,
    waiting: Option<&Pending>,
    dir: &Path,
    key: &PublicKey,
) -> Result<Option<Pending>, String> {
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
    Ok(Some(Pending {
        version: manifest.version.trim().to_string(),
        file: name,
        signature: manifest.signature.clone(),
    }))
}

/// The version waiting for the next launch, for Settings to mention.
pub fn pending_version() -> Option<String> {
    let state: State = storage::read_json(&state_path());
    ready_in(&state, &update_dir())
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
/// just exit. A development build leaves the package where it is.
pub fn apply_pending() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let _ = fs::remove_file(old_path(&exe));
    let path = state_path();
    if !path.exists() || dev_build(&exe).is_some() {
        return false;
    }
    let mut state: State = storage::read_json(&path);
    let Some(pending) = state.pending.take() else { return false };
    // Cleared before trying: a package that can't be applied is not retried
    // at every launch. The tag goes too, so the next check downloads it
    // again.
    state.etag.clear();
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
    applied && Command::new(&exe).args(std::env::args_os().skip(1)).env_remove(WAIT_VAR).spawn().is_ok()
}

/// The chip or the menu asked for a restart: when the window has closed
/// (and saved the session), `relaunch` starts the executable again.
pub fn request_restart() {
    RESTART.store(true, Ordering::Relaxed);
}

pub fn restart_requested() -> bool {
    RESTART.load(Ordering::Relaxed)
}

/// The command that starts `exe` again with the same arguments. The new
/// process gets the ID of this one, and waits until this one is gone.
pub fn restart_command(exe: &Path, args: impl IntoIterator<Item = OsString>, pid: u32) -> Command {
    let mut command = Command::new(exe);
    command.args(args).env(WAIT_VAR, pid.to_string());
    command
}

/// After the window closed: starts this executable again. Its
/// `apply_pending` swaps the package in before its window exists.
pub fn relaunch() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    restart_command(&exe, std::env::args_os().skip(1), std::process::id()).spawn().is_ok()
}

/// The process ID in `WAIT_VAR`, if there is a valid one.
fn previous_pid(value: Option<OsString>) -> Option<u32> {
    value?.to_str()?.trim().parse().ok().filter(|&pid| pid != 0 && pid != std::process::id())
}

/// First thing at launch: a restarted instance waits until the instance
/// that started it is gone. That one still holds the profile, the session
/// file and the script pipe while it closes. Only a process that runs the
/// same executable is waited for, and never longer than `WAIT_LIMIT_MS`.
pub fn wait_for_previous() {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                QueryFullProcessImageNameW, WaitForSingleObject,
            },
        },
        core::PWSTR,
    };
    let Some(pid) = previous_pid(std::env::var_os(WAIT_VAR)) else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return;
        };
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let same = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len)
            .is_ok()
            && same_file_name(&String::from_utf16_lossy(&buffer[..len as usize]), &exe);
        if same {
            WaitForSingleObject(process, WAIT_LIMIT_MS);
        }
        let _ = CloseHandle(process);
    }
}

/// Whether the image path of a process names the executable `exe`. Both
/// are made canonical first: one of them can be a short (8.3) path.
fn same_file_name(image: &str, exe: &Path) -> bool {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical(Path::new(image)).as_os_str().eq_ignore_ascii_case(canonical(exe).as_os_str())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/updater.rs"]
#[rustfmt::skip]
mod tests;
