//! Plain JSON files under %APPDATA%\Nea, read on demand and written
//! atomically (temp file + rename) so a crash never leaves half a file.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, WM_ENDSESSION,
            WM_QUERYENDSESSION, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
        },
    },
    core::w,
};

pub const APP_NAME: &str = "Nea";
/// What the window, the shortcut and the installed entry are called. A debug
/// build says so, so it is never mistaken for the published one.
pub const DISPLAY_NAME: &str = if cfg!(debug_assertions) { "Nea (development)" } else { "Nea" };

/// Set when the folders of the former name could not be moved this launch
/// (`legacy`): the session reads and writes them where they are.
static ROAMING_INSTEAD: OnceLock<PathBuf> = OnceLock::new();
static LOCAL_INSTEAD: OnceLock<PathBuf> = OnceLock::new();

/// Where the roaming files live (settings, history, bookmarks, extensions)
/// when nothing else was chosen. NEA_DATA_DIR puts everything in one folder.
pub fn default_roaming_root() -> PathBuf {
    std::env::var_os("NEA_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join(APP_NAME)))
        .unwrap_or_else(|| PathBuf::from(".").join(APP_NAME))
}

/// Where the machine-local files live (engine profile, downloaded updates).
pub fn default_local_root() -> PathBuf {
    std::env::var_os("NEA_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(APP_NAME)))
        .unwrap_or_else(default_roaming_root)
}

/// Makes this session use other folders. Only the first call counts.
pub fn use_instead(roaming: Option<PathBuf>, local: Option<PathBuf>) {
    if let Some(dir) = roaming {
        let _ = ROAMING_INSTEAD.set(dir);
    }
    if let Some(dir) = local {
        let _ = LOCAL_INSTEAD.set(dir);
    }
}

pub fn local_root() -> PathBuf {
    LOCAL_INSTEAD.get().cloned().unwrap_or_else(default_local_root)
}

pub fn data_dir() -> PathBuf {
    let base = ROAMING_INSTEAD.get().cloned().unwrap_or_else(default_roaming_root);
    let _ = fs::create_dir_all(&base);
    base
}

/// Engine profile data (cookies, cache) is machine-local, not roaming.
pub fn engine_dir() -> PathBuf {
    let base = local_root().join("engine");
    let _ = fs::create_dir_all(&base);
    base
}

/// The file's value, or the default when it is missing. A file that does
/// not parse (damaged, cut short, edited by hand) is set aside first, so the
/// default written over it later does not destroy what it held.
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(bytes) = fs::read(path) else { return T::default() };
    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            let kept = set_aside(path);
            crate::diag::log(
                crate::diag::Log::Crash,
                format!(
                    "{} unreadable {}: {error}; kept as {}",
                    crate::diag::stamp(),
                    path.display(),
                    kept.as_deref().map_or("nothing (the move failed)".into(), |p| p.display().to_string())
                ),
            );
            T::default()
        }
    }
}

/// How many set-aside copies of one file are kept.
const KEEP_CORRUPT: usize = 3;

/// Moves a file that does not parse to `<name>.corrupt-<unix seconds>` and
/// removes the oldest copies past `KEEP_CORRUPT`. Returns the new path.
pub fn set_aside(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let dir = path.parent()?;
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let prefix = format!("{name}.corrupt-");
    let mut target = dir.join(format!("{prefix}{secs}"));
    // Two in the same second (several files share a start) get a suffix.
    for n in 1..100 {
        if !target.exists() {
            break;
        }
        target = dir.join(format!("{prefix}{secs}-{n}"));
    }
    fs::rename(path, &target).ok()?;
    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path()))
        .collect();
    copies.sort();
    let extra = copies.len().saturating_sub(KEEP_CORRUPT);
    for (_, old) in copies.into_iter().take(extra) {
        let _ = fs::remove_file(old);
    }
    Some(target)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    write_bytes(path, &serde_json::to_vec(value)?)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// A file to write, serialized on the writer thread.
type Job = Box<dyn FnOnce() -> Option<Vec<u8>> + Send>;

#[derive(Default)]
struct Queue {
    /// One job per path: a newer one replaces what was still waiting.
    jobs: Vec<(PathBuf, Job)>,
    /// The writer holds jobs it took and has not finished.
    busy: bool,
    started: bool,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { jobs: Vec::new(), busy: false, started: false });
static CHANGED: Condvar = Condvar::new();

/// Writes `value` (moved here, so the caller hands over a copy) on a
/// background thread: serializing and the flush to disk of a large file
/// (the history: megabytes, plus `sync_all`, which an antivirus scan can
/// stretch to seconds) never block the window. Same atomic replace as
/// `write_json`. Only for files read once at start: a read right after this
/// may still see the old contents.
pub fn write_json_later<T: Serialize + Send + 'static>(path: PathBuf, value: T) {
    let job: Job = Box::new(move || serde_json::to_vec(&value).ok());
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.jobs.retain(|(p, _)| *p != path);
    queue.jobs.push((path, job));
    if !queue.started {
        let spawned = std::thread::Builder::new().name("writer".into()).stack_size(256 * 1024).spawn(write_loop);
        queue.started = spawned.is_ok();
    }
    if !queue.started {
        // No thread: write here rather than lose it.
        let jobs = std::mem::take(&mut queue.jobs);
        drop(queue);
        run_jobs(jobs);
        return;
    }
    CHANGED.notify_all();
}

/// Waits (up to `limit`) until everything queued is on disk. At exit.
pub fn flush_writes(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    while !queue.jobs.is_empty() || queue.busy {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        queue = CHANGED.wait_timeout(queue, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    true
}

fn run_jobs(jobs: Vec<(PathBuf, Job)>) {
    for (path, job) in jobs {
        if let Some(bytes) = job() {
            let _ = write_bytes(&path, &bytes);
        }
    }
}

/// How long the end of the session waits for the queued files on the
/// watcher's thread. Windows gives a program about 5 s before it asks the
/// person.
const SESSION_END_WRITE_LIMIT: Duration = Duration::from_secs(3);

static WATCHER: AtomicIsize = AtomicIsize::new(0);

/// Starts a thread that only waits for the end of the Windows session (sign
/// out, restart, shut down) in a hidden window of its own, and then waits
/// until the queued files are on disk. The window's thread can be in a
/// modal loop or a long handler when Windows ends the session: this thread
/// is free, so what the app queued last (refer to `write_json_later`) is
/// not cut off when the process stops. It does no other work and never
/// writes itself, so a broadcast to all windows never waits for a disk.
/// Only the first call counts.
pub fn watch_session_end() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("session end".into()).stack_size(64 * 1024).spawn(session_end_loop);
    });
}

/// The watcher's window, once it is made.
#[cfg(test)]
pub fn session_end_window() -> Option<HWND> {
    let raw = WATCHER.load(Ordering::Acquire);
    (raw != 0).then_some(HWND(raw as _))
}

fn session_end_loop() {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(session_end_proc),
            hInstance: module.into(),
            lpszClassName: w!("NeaSessionEnd"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // Top-level and never shown: a message-only window does not get the
        // end of the session.
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("NeaSessionEnd"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            return;
        };
        WATCHER.store(hwnd.0 as isize, Ordering::Release);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn session_end_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Nea never stops the end of the session.
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                flush_writes(SESSION_END_WRITE_LIMIT);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn write_loop() {
    loop {
        let jobs = {
            let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            while queue.jobs.is_empty() {
                queue = CHANGED.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
            queue.busy = true;
            std::mem::take(&mut queue.jobs)
        };
        run_jobs(jobs);
        QUEUE.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
        CHANGED.notify_all();
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/storage.rs"]
#[rustfmt::skip]
mod tests;
