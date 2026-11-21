//! The heartbeat of the frame clock: a thread that waits for the
//! compositor's next frame and tells the UI thread, so each frame of a
//! motion starts right after a vertical blank instead of on a timer that
//! drifts against the screen.
//!
//! The thread waits on the compositor clock
//! (`DCompositionWaitForCompositorClock`, Windows 11) and, where that is
//! missing, on `DwmFlush`; when neither waits, on a high-resolution timer at
//! 60 Hz. It posts one message at a time to a message-only window of the UI
//! thread: a frame the UI thread has not taken yet is not posted again, so a
//! slow frame never queues more. While nothing moves the thread sleeps on a
//! condition variable and costs nothing.
//!
//! A posted message comes before input, paint and timer messages in the
//! queue, and menus and other modal loops dispatch it too, so a stream of
//! mouse moves does not hold back a frame.

use std::{
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Dwm::DwmFlush,
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
                TIMER_ALL_ACCESS, WaitForSingleObject,
            },
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, HWND_MESSAGE, PostMessageW, RegisterClassW, WINDOW_EX_STYLE, WM_APP,
            WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
        },
    },
    core::w,
};

/// Posted to the clock's window for each compositor frame.
const WM_APP_FRAME: u32 = WM_APP + 0x50;

/// What the UI thread gets: a frame (after a vertical blank) or a timer.
pub enum Beat {
    Frame,
    Timer,
}

struct Shared {
    /// Something moves: the thread waits for frames and posts them.
    run: Mutex<bool>,
    wake: Condvar,
    /// A frame is posted and the UI thread has not taken it yet.
    posted: AtomicBool,
    window: AtomicIsize,
    /// When the last compositor frame came (microseconds from start) and
    /// the time between two frames, as measured.
    last_frame_us: AtomicU64,
    period_us: AtomicU64,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

thread_local! {
    static HANDLER: std::cell::Cell<Option<fn(Beat)>> = const { std::cell::Cell::new(None) };
}

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| Shared {
        run: Mutex::new(false),
        wake: Condvar::new(),
        posted: AtomicBool::new(false),
        window: AtomicIsize::new(0),
        last_frame_us: AtomicU64::new(0),
        period_us: AtomicU64::new(crate::anim::DEFAULT_PERIOD_US),
    })
}

/// The clock's window, made on the UI thread the first time it is asked
/// for; `handler` gets every beat. None when it cannot be made.
pub fn window(handler: fn(Beat)) -> Option<HWND> {
    let s = shared();
    let existing = s.window.load(Ordering::Relaxed);
    if existing != 0 {
        return Some(HWND(existing as _));
    }
    HANDLER.with(|h| h.set(Some(handler)));
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).ok()?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(clock_proc),
            hInstance: instance,
            lpszClassName: w!("NeaFrameClock"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("NeaFrameClock"),
            None,
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
        .ok()?
    };
    s.window.store(hwnd.0 as isize, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("frame-pacer".into()).stack_size(64 * 1024).spawn(pace);
    Some(hwnd)
}

/// Starts or stops the frames.
pub fn run(on: bool) {
    let s = shared();
    let mut run = s.run.lock().unwrap_or_else(|e| e.into_inner());
    if *run != on {
        *run = on;
        s.wake.notify_all();
    }
}

/// When the last compositor frame came, in microseconds from start.
pub fn last_frame_us() -> u64 {
    shared().last_frame_us.load(Ordering::Relaxed)
}

/// The measured time between two compositor frames.
pub fn period_us() -> u64 {
    shared().period_us.load(Ordering::Relaxed)
}

unsafe extern "system" fn clock_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let beat = match msg {
        WM_APP_FRAME => {
            shared().posted.store(false, Ordering::Release);
            Some(Beat::Frame)
        }
        WM_TIMER => Some(Beat::Timer),
        _ => None,
    };
    match (beat, HANDLER.with(|h| h.get())) {
        (Some(beat), Some(handler)) => {
            let _doing = crate::diag::doing(&"frame", 0);
            handler(beat);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// How the thread waits for the next frame.
enum Wait {
    Compositor(Compositor),
    Dwm,
    Timer(HANDLE),
}

type WaitForClock = unsafe extern "system" fn(u32, *const HANDLE, u32) -> u32;

struct Compositor {
    _library: crate::system_dll::Library,
    wait: WaitForClock,
}

/// A wait shorter than this did not wait for anything.
const NO_WAIT: Duration = Duration::from_micros(500);
/// That many waits in a row that did not wait: the next way is tried.
const FAILS_BEFORE_FALLBACK: u32 = 8;

fn first_wait() -> Wait {
    let library = crate::system_dll::Library::load(w!("dcomp.dll"));
    let compositor = library.and_then(|l| {
        let wait = unsafe { l.function::<WaitForClock>(c"DCompositionWaitForCompositorClock") }?;
        Some(Compositor { _library: l, wait })
    });
    match compositor {
        Some(c) => Wait::Compositor(c),
        None => Wait::Dwm,
    }
}

fn timer_wait() -> Wait {
    let timer =
        unsafe { CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0) };
    match timer {
        Ok(timer) => Wait::Timer(timer),
        // Without a timer of its own the thread sleeps.
        Err(_) => Wait::Timer(HANDLE::default()),
    }
}

impl Wait {
    /// Waits for the next frame; false when the wait returned at once.
    fn next(&self) -> bool {
        let started = Instant::now();
        match self {
            Wait::Compositor(c) => {
                const WAIT_FAILED: u32 = u32::MAX;
                const WAIT_TIMEOUT: u32 = 258;
                let result = unsafe { (c.wait)(0, std::ptr::null(), 100) };
                if result == WAIT_FAILED || result == WAIT_TIMEOUT {
                    return false;
                }
            }
            Wait::Dwm => {
                if unsafe { DwmFlush() }.is_err() {
                    return false;
                }
            }
            Wait::Timer(timer) => {
                let period = crate::anim::DEFAULT_PERIOD_US as i64;
                if timer.is_invalid() {
                    std::thread::sleep(Duration::from_micros(period as u64));
                } else {
                    // Relative time, in 100 ns units.
                    let due = -(period * 10);
                    unsafe {
                        if SetWaitableTimer(*timer, &due, 0, None, None, false).is_ok() {
                            WaitForSingleObject(*timer, INFINITE);
                        }
                    }
                }
                return true;
            }
        }
        started.elapsed() >= NO_WAIT
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        if let Wait::Timer(timer) = self
            && !timer.is_invalid()
        {
            unsafe {
                let _ = CloseHandle(*timer);
            }
        }
    }
}

fn pace() {
    let s = shared();
    let mut wait = first_wait();
    let mut fails = 0u32;
    let mut previous: Option<u64> = None;
    loop {
        {
            let mut run = s.run.lock().unwrap_or_else(|e| e.into_inner());
            if !*run {
                previous = None;
            }
            while !*run {
                run = s.wake.wait(run).unwrap_or_else(|e| e.into_inner());
            }
        }
        if !wait.next() {
            fails += 1;
            if fails >= FAILS_BEFORE_FALLBACK {
                fails = 0;
                wait = match wait {
                    Wait::Compositor(_) => Wait::Dwm,
                    _ => timer_wait(),
                };
            }
            continue;
        }
        fails = 0;
        let now = crate::diag::now_us();
        if let Some(before) = previous {
            let period = crate::anim::next_period(s.period_us.load(Ordering::Relaxed), now.saturating_sub(before));
            s.period_us.store(period, Ordering::Relaxed);
        }
        previous = Some(now);
        s.last_frame_us.store(now, Ordering::Relaxed);
        if s.posted.swap(true, Ordering::AcqRel) {
            continue;
        }
        let hwnd = HWND(s.window.load(Ordering::Relaxed) as _);
        if unsafe { PostMessageW(Some(hwnd), WM_APP_FRAME, WPARAM(0), LPARAM(0)) }.is_err() {
            s.posted.store(false, Ordering::Release);
        }
    }
}
