//! Control by script: a local named pipe, off unless Settings or
//! `NEA_BENCH=1` turn it on. Its own thread reads one JSON request per
//! line; what needs the window is queued for the UI thread (one posted
//! message, as engine events are), which answers through a channel. Memory
//! is measured right on the pipe thread: it needs nothing from the window.
//!
//! Only the current user can open the pipe (its DACL names nobody else),
//! remote clients are refused and the first instance is ours, so no other
//! process can create the name first and pose as the app.

pub mod memory;
pub mod protocol;

use std::{
    collections::VecDeque,
    fs::File,
    io::{BufRead, BufReader, Write},
    mem::ManuallyDrop,
    os::windows::io::{AsRawHandle, FromRawHandle},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
    time::Duration,
};

use serde_json::{Value, json};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, HWND, LPARAM, LocalFree, WPARAM},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX},
        System::{
            IO::CancelSynchronousIo,
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
    core::{HSTRING, PWSTR},
};

use protocol::{Action, Request};

/// Far from the app's own WM_APP + n, which other features keep adding to.
pub const WM_APP_BENCH: u32 = WM_APP + 0x40;

/// Where an answer goes: back to the pipe thread waiting for it. Consumed
/// by answering, so a request is answered once.
pub struct Reply {
    id: Value,
    tx: Sender<String>,
}

impl Reply {
    pub fn ok(self, result: Value) {
        let _ = self.tx.send(protocol::ok(&self.id, result));
    }

    pub fn error(self, message: &str) {
        let _ = self.tx.send(protocol::error(&self.id, message));
    }
}

pub struct Job {
    pub action: Action,
    pub reply: Reply,
}

static INBOX: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// `NEA_BENCH=1`: a run driven by a script.
pub fn from_env() -> bool {
    std::env::var("NEA_BENCH").is_ok_and(|v| v.trim() == "1")
}

/// `NEA_BENCH_FRONT=1` with `NEA_BENCH=1`: the app acts as if its window
/// were in front, so a script measures the focus work of a window in front
/// without taking the foreground from the person's program.
pub fn acts_in_front() -> bool {
    static FRONT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FRONT.get_or_init(|| from_env() && std::env::var("NEA_BENCH_FRONT").is_ok_and(|v| v.trim() == "1"))
}

/// The setting, or the environment for a test run.
pub fn wanted(setting: bool) -> bool {
    setting || from_env()
}

/// The pipe this process listens on. `NEA_BENCH_PIPE` adds a suffix, so
/// a measuring run doesn't collide with the person's own browser.
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
    let suffix = std::env::var("NEA_BENCH_PIPE").ok();
    protocol::pipe_name(&user, suffix.as_deref())
}

/// Starts or stops the pipe; doing what is already done is harmless.
pub fn set_enabled(hwnd: HWND, on: bool) {
    WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
    crate::anim::keep_marks(on);
    if on { start(pipe_name()) } else { stop() }
}

struct Server {
    name: String,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

fn start(name: String) {
    let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    if server.is_some() {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let pipe = name.clone();
    if let Ok(thread) = std::thread::Builder::new().name("bench-pipe".into()).spawn(move || serve(&pipe, &flag)) {
        *server = Some(Server { name, stop, thread });
    }
}

fn stop() {
    let Some(old) = SERVER.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    old.stop.store(true, Ordering::SeqCst);
    // Wakes the thread out of a blocking connect or read; and, should it not
    // be blocked yet, a connection of our own lets its next connect return.
    unsafe {
        let _ = CancelSynchronousIo(HANDLE(old.thread.as_raw_handle()));
    }
    let _ = std::fs::OpenOptions::new().read(true).write(true).open(&old.name);
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Next request for the window.
pub fn pop() -> Option<Job> {
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

/// A request the window was too busy for goes back first in line.
pub fn push_front(job: Job) {
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).push_front(job);
}

/// The window the requests are posted to.
pub fn window() -> HWND {
    HWND(WINDOW.load(Ordering::SeqCst) as *mut _)
}

pub fn post() {
    let hwnd = window();
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_BENCH, WPARAM(0), LPARAM(0));
    }
}

/// "D:P(A;;GA;;;<sid>)": full access for the current user, nobody else.
fn user_only_descriptor() -> Option<PSECURITY_DESCRIPTOR> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buffer = vec![0u64; (len as usize).div_ceil(8)];
        let read = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        read.ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_text).ok()?;
        let sid = sid_text.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(sid_text.0.cast())));
        if sid.is_empty() {
            return None;
        }
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(format!("D:P(A;;GA;;;{sid})")),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .ok()?;
        Some(descriptor)
    }
}

fn create_pipe(name: &str, descriptor: PSECURITY_DESCRIPTOR) -> Option<HANDLE> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    // A pipe just closed by a server being turned off and on again may
    // still hold the name for a moment.
    for _ in 0..20 {
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                Some(&attributes),
            )
        };
        if !pipe.is_invalid() {
            return Some(pipe);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn serve(name: &str, stop: &AtomicBool) {
    // Without a descriptor that keeps other users out there is no pipe.
    let Some(descriptor) = user_only_descriptor() else { return };
    let pipe = create_pipe(name, descriptor);
    if let Some(pipe) = pipe {
        while !stop.load(Ordering::SeqCst) {
            let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
                Ok(()) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if connected {
                serve_client(pipe, stop);
                unsafe {
                    let _ = FlushFileBuffers(pipe);
                }
            }
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
            }
        }
        unsafe {
            let _ = CloseHandle(pipe);
        }
    }
    unsafe {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
}

/// Requests in order, each answered before the next is read.
fn serve_client(pipe: HANDLE, stop: &AtomicBool) {
    // The handle belongs to `serve`; the File only borrows it.
    let file = ManuallyDrop::new(unsafe { File::from_raw_handle(pipe.0) });
    let mut reader = BufReader::new(&*file);
    let mut writer = &*file;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        let answer = answer(&line);
        if writer
            .write_all(answer.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

fn answer(line: &str) -> String {
    let request = match Request::parse(line) {
        Ok(request) => request,
        Err(message) => return protocol::error(&Value::Null, &message),
    };
    match request.action {
        Action::Ping => protocol::ok(
            &request.id,
            json!({ "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"), "pipe": pipe_name() }),
        ),
        Action::Memory => protocol::ok(&request.id, memory_report()),
        _ => {
            let timeout = Duration::from_millis(request.reply_timeout_ms());
            let id = request.id.clone();
            let (tx, rx) = mpsc::channel();
            INBOX
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(Job { action: request.action, reply: Reply { id: request.id, tx } });
            post();
            rx.recv_timeout(timeout).unwrap_or_else(|_| protocol::error(&id, "the window did not answer in time"))
        }
    }
}

/// The app (nea.exe) and the engine (everything below it), apart and summed.
pub fn memory_report() -> Value {
    let tree = memory::measure_tree(std::process::id());
    let engine: u64 = tree.children.iter().map(|c| c.private_ws).sum();
    json!({
        "unit": "bytes, private working set",
        "app": tree.root.private_ws,
        "engine": engine,
        "engine_processes": tree.children.len(),
        "total": tree.total,
        "processes": std::iter::once(&tree.root).chain(tree.children.iter()).collect::<Vec<_>>(),
    })
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/bench.rs"]
#[rustfmt::skip]
mod tests;
