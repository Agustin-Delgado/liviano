//! Where the rules come from: EasyList and EasyPrivacy, cached under the
//! data folder and refreshed at most once a day, with a small built-in list
//! so blocking works offline from the very first start. Everything here runs
//! on one background thread; the UI thread only ever receives a finished
//! engine.

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

use windows::{
    Win32::Networking::WinHttp::*,
    core::{HSTRING, PCWSTR, w},
};

use super::filters::Filters;

/// (cache file, host, path). The only traffic of our own besides the pages.
const LISTS: [(&str, &str, &str); 2] = [
    ("easylist.txt", "easylist.to", "/easylist/easylist.txt"),
    ("easyprivacy.txt", "easylist.to", "/easylist/easyprivacy.txt"),
];

const BUILT_IN: &str = include_str!("minimal.txt");
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the sleeping thread wakes to see whether a day has passed
/// (or to retry a download that failed).
const RECHECK: Duration = Duration::from_secs(60 * 60);
/// Written after a successful refresh; its age is the age of the lists.
const STAMP: &str = "updated";
const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;

pub fn spawn(dir: PathBuf) {
    // Parsing is flat (no recursion): a small stack is plenty.
    let _ = thread::Builder::new().name("shield".into()).stack_size(256 * 1024).spawn(move || run(&dir));
}

fn run(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    super::deliver(compile(dir));
    loop {
        if is_due(dir) && refresh(dir) {
            super::deliver(compile(dir));
        }
        thread::sleep(RECHECK);
    }
}

/// The cached lists plus the built-in one (which also covers a cache that is
/// missing or was cut short).
fn compile(dir: &Path) -> Filters {
    let texts: Vec<String> = LISTS.iter().filter_map(|(file, _, _)| fs::read_to_string(dir.join(file)).ok()).collect();
    let filters = Filters::compile(texts.iter().map(String::as_str).chain(std::iter::once(BUILT_IN)));
    #[cfg(debug_assertions)]
    eprintln!("shield: {} lists, {} rules, {} KB", texts.len(), filters.len(), filters.heap_bytes() / 1024);
    filters
}

fn is_due(dir: &Path) -> bool {
    let modified = fs::metadata(dir.join(STAMP)).and_then(|m| m.modified());
    match modified {
        Ok(at) => SystemTime::now().duration_since(at).map_or(true, |age| age >= MAX_AGE),
        Err(_) => true,
    }
}

/// Downloads every list; true when at least one changed on disk. The stamp
/// is only written on success, so a failed attempt is retried next hour.
fn refresh(dir: &Path) -> bool {
    let mut changed = false;
    let mut all_ok = true;
    for (file, host, path) in LISTS {
        let Some(body) = download(host, path).filter(|b| looks_like_a_list(b)) else {
            all_ok = false;
            continue;
        };
        let target = dir.join(file);
        if fs::read(&target).is_ok_and(|old| old == body) {
            continue;
        }
        let tmp = target.with_extension("tmp");
        if fs::write(&tmp, &body).and_then(|_| fs::rename(&tmp, &target)).is_ok() {
            changed = true;
        }
    }
    if all_ok {
        let _ = fs::write(dir.join(STAMP), b"");
    }
    changed
}

/// A captive portal or an error page must never replace a good list.
fn looks_like_a_list(body: &[u8]) -> bool {
    body.len() > 10_000 && (body.starts_with(b"[Adblock") || body.starts_with(b"! Title"))
}

/// A WinHTTP handle closed on drop.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

const DECOMPRESSION_FLAG_ALL: u32 = 3;

/// GET https://host/path with the system proxy settings; the body on a 200.
fn download(host: &str, path: &str) -> Option<Vec<u8>> {
    unsafe {
        let session =
            Handle(WinHttpOpen(w!("Liviano"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return None;
        }
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 30_000);
        let _ = WinHttpSetOption(
            Some(session.0),
            WINHTTP_OPTION_DECOMPRESSION,
            Some(&DECOMPRESSION_FLAG_ALL.to_ne_bytes()),
        );
        let connection = Handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0));
        if connection.0.is_null() {
            return None;
        }
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return None;
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()?;
        if status != 200 {
            return None;
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut _, chunk.len() as u32, &mut read).ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_LIST_BYTES {
                return None;
            }
        }
        Some(body)
    }
}

