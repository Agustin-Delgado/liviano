//! Plain JSON files under %APPDATA%\Liviano, read on demand and written
//! atomically (temp file + rename) so a crash never leaves half a file.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
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

