//! Diagnostics that stay on in every build and cost next to nothing:
//!
//! - `crash.log`: a panic (release aborts on one, leaving no trace
//!   elsewhere), an exception nobody handled, and the engine's own process
//!   failures (a renderer or the browser process that died).
//! - `hang.log`: the UI thread stuck for more than `HANG_AFTER` — what it was
//!   doing and for how long — and any single handler slower than `SLOW_AFTER`.
//! - `trace.log`, only with `NEA_TRACE=1`: timestamped marks and every
//!   handler slower than a few milliseconds, for measuring.
//!
//! The UI thread only stores atomics and pushes lines to a short queue; a
//! watchdog thread that wakes once a second checks them and does the file
//! writing. All three files live in the data folder and are capped in size.

use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

/// Posted by the watchdog when the UI thread looks stuck: a nested message
/// loop (a menu, a dialog) still dispatches it, which tells "busy in a modal
/// loop" from "not pumping at all".
pub const WM_APP_PING: u32 = WM_APP + 0x41;

/// The UI thread counts as hung after this long without a message.
const HANG_AFTER: Duration = Duration::from_secs(2);
/// Quiet this long and not idle: the watchdog pings.
const PING_AFTER: Duration = Duration::from_millis(1000);
/// A single handler this slow is written down even if it never reaches a hang.
const SLOW_AFTER: Duration = Duration::from_millis(250);
/// With tracing on, handlers slower than this are traced.
const TRACE_SLOW_AFTER: Duration = Duration::from_millis(4);
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// Past this size a log is moved to `<name>.old` and started again.
const MAX_LOG_BYTES: u64 = 256 * 1024;
/// Lines waiting for the watchdog beyond this are dropped.
const MAX_PENDING: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Log {
    Crash,
    Hang,
    Trace,
}

impl Log {
    fn file_name(self) -> &'static str {
        match self {
            Log::Crash => "crash.log",
            Log::Hang => "hang.log",
            Log::Trace => "trace.log",
        }
    }
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
static TRACING: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static PENDING: Mutex<Vec<(Log, String)>> = Mutex::new(Vec::new());

/// When (ms from start) the UI thread last took a message off its queue or
/// had one dispatched to it.
static TICK_AT: AtomicU64 = AtomicU64::new(0);
/// The UI thread is blocked in its top-level `GetMessage`: idle, not hung.
static WAITING: AtomicBool = AtomicBool::new(false);
/// What the UI thread is doing now (see `doing`), with a number that
/// narrows it down (a message id, a tab id) and since when (ms from start).
static NAME: AtomicPtr<&'static str> = AtomicPtr::new(std::ptr::null_mut());
static DETAIL: AtomicU64 = AtomicU64::new(0);
static SINCE: AtomicU64 = AtomicU64::new(0);

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Milliseconds since the process started.
pub fn now_ms() -> u64 {
    start().elapsed().as_millis() as u64
}

/// Microseconds since the process started, for traces.
pub fn now_us() -> u64 {
    start().elapsed().as_micros() as u64
}

/// Installs the panic hook and the unhandled-exception filter and starts
/// the watchdog. `NEA_TRACE=1` turns tracing on.
pub fn install(dir: &Path) {
    start();
    let _ = DIR.set(dir.to_path_buf());
    TRACING.store(std::env::var("NEA_TRACE").is_ok_and(|v| v.trim() == "1"), Ordering::Relaxed);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_now(Log::Crash, &panic_line(info));
        previous(info);
    }));
    seh::install(dir);
    let _ = std::thread::Builder::new().name("watchdog".into()).stack_size(64 * 1024).spawn(watch);
}

/// The window the watchdog pings.
pub fn set_window(hwnd: HWND) {
    WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
}

pub fn tracing() -> bool {
    TRACING.load(Ordering::Relaxed)
}

/// Queues a line for one of the logs (written by the watchdog within a second).
pub fn log(log: Log, line: String) {
    if let Ok(mut pending) = PENDING.lock()
        && pending.len() < MAX_PENDING
    {
        pending.push((log, line));
    }
}

/// A trace mark, only when tracing is on: `trace(|| format!(...))`.
pub fn trace(line: impl FnOnce() -> String) {
    if tracing() {
        log(Log::Trace, format!("{:>12.3} {}", now_us() as f64 / 1000.0, line()));
    }
}

/// Called by the message loop around `GetMessage` and by the window
/// procedure for every message: proof that the UI thread is pumping.
pub fn waiting(on: bool) {
    if !on {
        tick();
    }
    WAITING.store(on, Ordering::Relaxed);
}

pub fn tick() {
    TICK_AT.store(now_ms(), Ordering::Relaxed);
}

/// Marks what the UI thread is doing until the guard drops; nests.
/// `name` is a promoted literal: `doing(&"timer:save", 0)`.
pub fn doing(name: &'static &'static str, detail: u64) -> Doing {
    let previous = (NAME.load(Ordering::Relaxed), DETAIL.load(Ordering::Relaxed), SINCE.load(Ordering::Relaxed));
    NAME.store(name as *const &'static str as *mut _, Ordering::Relaxed);
    DETAIL.store(detail, Ordering::Relaxed);
    SINCE.store(now_ms(), Ordering::Relaxed);
    Doing { name, detail, started: Instant::now(), previous }
}

pub struct Doing {
    name: &'static &'static str,
    detail: u64,
    started: Instant,
    previous: (*mut &'static str, u64, u64),
}

impl Drop for Doing {
    fn drop(&mut self) {
        let took = self.started.elapsed();
        let (name, detail, since) = self.previous;
        NAME.store(name, Ordering::Relaxed);
        DETAIL.store(detail, Ordering::Relaxed);
        SINCE.store(since, Ordering::Relaxed);
        if took >= SLOW_AFTER {
            log(Log::Hang, format!("{} lento: {} ({}) tardó {} ms", stamp(), self.name, self.detail, took.as_millis()));
        }
        if took >= TRACE_SLOW_AFTER && tracing() {
            let (name, detail) = (*self.name, self.detail);
            trace(|| format!("slow {name} {detail} {:.1} ms", took.as_secs_f64() * 1000.0));
        }
    }
}

/// What the UI thread is doing right now, as words.
fn current() -> String {
    let name = NAME.load(Ordering::Relaxed);
    if name.is_null() {
        return "nada marcado (entre mensajes o dentro del motor)".to_string();
    }
    // Only promoted 'static literals are ever stored.
    let name = unsafe { *name };
    let since = now_ms().saturating_sub(SINCE.load(Ordering::Relaxed));
    format!("{name} ({}) desde hace {since} ms", DETAIL.load(Ordering::Relaxed))
}

/// Local time with milliseconds, for every line.
pub fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn panic_line(info: &std::panic::PanicHookInfo<'_>) -> String {
    let thread = std::thread::current();
    let mut line = format!("{} panic v{} hilo {}: ", stamp(), version(), thread.name().unwrap_or("?"));
    if let Some(location) = info.location() {
        let _ = write!(line, "{}:{}:{}: ", location.file(), location.line(), location.column());
    }
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(sin mensaje)".to_string());
    let _ = write!(line, "{message} | haciendo: {}", current());
    line.replace(['\r', '\n'], " ")
}

/// Writes straight away, on the calling thread (a panic is about to abort).
fn write_now(log: Log, line: &str) {
    if let Some(dir) = DIR.get() {
        append(dir, log, std::iter::once(line));
    }
}

fn append<'a>(dir: &Path, log: Log, lines: impl Iterator<Item = &'a str>) {
    let path = dir.join(log.file_name());
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push_str("\r\n");
        }
        let _ = file.write_all(text.as_bytes());
    }
}

fn flush() {
    let lines = PENDING.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default();
    let Some(dir) = DIR.get() else { return };
    for log in [Log::Crash, Log::Hang, Log::Trace] {
        let mut of_log = lines.iter().filter(|(l, _)| *l == log).map(|(_, line)| line.as_str()).peekable();
        if of_log.peek().is_some() {
            append(dir, log, of_log);
        }
    }
}

/// Writes whatever is queued; called at exit, before the process ends.
pub fn flush_now() {
    flush();
}

/// The watchdog's view of the UI thread from one check to the next.
#[derive(Debug, Default, PartialEq)]
struct Watch {
    pinged: bool,
    /// The last message before the hang that was reported (ms from start).
    reported: Option<u64>,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Fine,
    /// Quiet for a while: ask the thread whether it still pumps.
    Ping,
    /// Stuck past the limit: write it down (once).
    Hung {
        for_ms: u64,
    },
    /// Moving again after a reported hang, which lasted about this long.
    Recovered {
        after_ms: u64,
    },
}

impl Watch {
    fn check(&mut self, last_tick: u64, waiting: bool, now: u64) -> Verdict {
        let quiet = if waiting { 0 } else { now.saturating_sub(last_tick) };
        if quiet < PING_AFTER.as_millis() as u64 {
            self.pinged = false;
            return match self.reported.take() {
                Some(before) => Verdict::Recovered { after_ms: last_tick.saturating_sub(before) },
                None => Verdict::Fine,
            };
        }
        if !self.pinged {
            self.pinged = true;
            return Verdict::Ping;
        }
        if self.reported.is_none() && quiet >= HANG_AFTER.as_millis() as u64 {
            self.reported = Some(last_tick);
            return Verdict::Hung { for_ms: quiet };
        }
        Verdict::Fine
    }
}

fn watch() {
    let mut state = Watch::default();
    loop {
        std::thread::sleep(CHECK_EVERY);
        let verdict = state.check(TICK_AT.load(Ordering::Relaxed), WAITING.load(Ordering::Relaxed), now_ms());
        match verdict {
            Verdict::Fine => {}
            Verdict::Ping => {
                let hwnd = WINDOW.load(Ordering::Relaxed);
                if hwnd != 0 {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_PING, WPARAM(0), LPARAM(0));
                    }
                }
            }
            Verdict::Hung { for_ms } => {
                log(Log::Hang, format!("{} colgado hace {for_ms} ms, haciendo: {}", stamp(), current()));
            }
            Verdict::Recovered { after_ms } => {
                log(Log::Hang, format!("{} respondió de nuevo tras ~{after_ms} ms", stamp()));
            }
        }
        flush();
    }
}

/// An exception nobody handled (an access violation in a COM call, a stack
/// overflow): one line with its code and where, relative to the
/// executable, then the default handling (the process ends).
mod seh {
    use std::{path::Path, sync::OnceLock};

    use windows::{
        Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_ALWAYS, WriteFile,
            },
            System::{
                Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter},
                LibraryLoader::GetModuleHandleW,
            },
        },
        core::PCWSTR,
    };

    static PATH: OnceLock<Vec<u16>> = OnceLock::new();
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    pub fn install(dir: &Path) {
        let wide: Vec<u16> = dir.join("crash.log").as_os_str().encode_wide_with_nul();
        let _ = PATH.set(wide);
        unsafe {
            SetUnhandledExceptionFilter(Some(filter));
        }
    }

    trait WideNul {
        fn encode_wide_with_nul(&self) -> Vec<u16>;
    }

    impl WideNul for std::ffi::OsStr {
        fn encode_wide_with_nul(&self) -> Vec<u16> {
            use std::os::windows::ffi::OsStrExt;
            self.encode_wide().chain(std::iter::once(0)).collect()
        }
    }

    /// A fixed buffer: nothing here allocates, the heap may be what broke.
    struct Line {
        bytes: [u8; 256],
        len: usize,
    }

    impl std::fmt::Write for Line {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            let room = self.bytes.len() - self.len;
            let n = s.len().min(room);
            self.bytes[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
            self.len += n;
            Ok(())
        }
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        use std::fmt::Write;
        let Some(path) = PATH.get() else { return EXCEPTION_CONTINUE_SEARCH };
        let (code, address) = unsafe {
            match info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
                Some(record) => (record.ExceptionCode.0 as u32, record.ExceptionAddress as usize),
                None => (0, 0),
            }
        };
        let base = unsafe { GetModuleHandleW(PCWSTR::null()).map_or(0, |m| m.0 as usize) };
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let mut line = Line { bytes: [0; 256], len: 0 };
        let _ = write!(
            line,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} excepcion v{} codigo 0x{code:08X} en 0x{address:X} (nea.exe+0x{:X})\r\n",
            t.wYear,
            t.wMonth,
            t.wDay,
            t.wHour,
            t.wMinute,
            t.wSecond,
            t.wMilliseconds,
            env!("CARGO_PKG_VERSION"),
            address.wrapping_sub(base),
        );
        unsafe {
            if let Ok(file) = CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_APPEND_DATA.0,
                FILE_SHARE_READ,
                None,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) {
                let _ = WriteFile(file, Some(&line.bytes[..line.len]), None, None);
                let _ = windows::Win32::Foundation::CloseHandle(HANDLE(file.0));
            }
        }
        EXCEPTION_CONTINUE_SEARCH
    }
}
