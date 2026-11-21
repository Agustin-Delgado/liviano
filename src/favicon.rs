//! Site icons for the tabs. The engine says when a page's icon changes
//! (`FaviconChanged`) and hands it over as a PNG; it is decoded once to the
//! pixels a tab draws it at and kept per site, in memory (a few hundred at
//! most) and on disk (`favicons/<host>.png`, a few megabytes at most), so a
//! tab asleep or restored from the last session shows its icon without
//! waking its page. Private tabs are not watched: they keep their violet
//! initial, which is how a private tab reads, and leave nothing on disk.
//! With no icon a tab keeps its initial on a small square.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use webview2_com::{
    FaviconChangedEventHandler, GetFaviconCompletedHandler,
    Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, ICoreWebView2, ICoreWebView2_15},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
    core::{Interface, PWSTR},
};

use crate::image::{self, Pixels};

/// Posted to the window when icons arrived (see `take_arrived`).
pub const WM_APP_FAVICON: u32 = WM_APP + 50;
/// Icons kept decoded in memory; the least recently stored goes first.
const MAX_ICONS: usize = 300;
/// Bytes the icon folder may take; the oldest files go first.
const MAX_DISK: u64 = 4 * 1024 * 1024;
/// One icon file larger than this is not kept (a page serving a poster as its icon).
const MAX_FILE: usize = 64 * 1024;
/// The size a tab draws an icon at, before the DPI scale.
pub const SIZE: f32 = 16.0;

/// An icon as it arrived from a page, waiting to be decoded by the window.
struct Arrived {
    host: String,
    png: Vec<u8>,
}

thread_local! {
    static ARRIVED: RefCell<Vec<Arrived>> = const { RefCell::new(Vec::new()) };
    static CACHE: RefCell<Cache> = RefCell::new(Cache::new(storage_dir()));
}

fn storage_dir() -> Option<PathBuf> {
    Some(crate::storage::data_dir().join("favicons"))
}

/// The key an address's icon is kept under: its host, lowercased, for web
/// pages only (`about:`, `file:` and the like have none).
pub fn key(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.strip_prefix("www.").unwrap_or(host).to_ascii_lowercase();
    let clean: String =
        host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    (!clean.is_empty() && !clean.starts_with('.')).then_some(clean)
}

/// Files to delete so the folder fits in `cap` bytes: the oldest first.
pub fn to_evict(mut files: Vec<(PathBuf, u64, SystemTime)>, cap: u64) -> Vec<PathBuf> {
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.2);
    let mut gone = Vec::new();
    for (path, size, _) in files {
        if total <= cap {
            break;
        }
        total -= size;
        gone.push(path);
    }
    gone
}

pub struct Icon {
    pub pixels: Pixels,
}

/// Icons by host, decoded at one size. `None` remembers a host with no
/// icon on disk, so the disk is asked once per host and run.
pub struct Cache {
    dir: Option<PathBuf>,
    size: u32,
    icons: HashMap<String, Option<Rc<Icon>>>,
    order: VecDeque<String>,
}

impl Cache {
    fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, size: 16, icons: HashMap::new(), order: VecDeque::new() }
    }

    fn set_size(&mut self, size: u32) {
        if size != self.size {
            self.size = size;
            // Decoded again, at the new size, as they are drawn.
            self.icons.clear();
            self.order.clear();
        }
    }

    fn remember(&mut self, host: String, icon: Option<Rc<Icon>>) {
        if !self.icons.contains_key(&host) {
            self.order.push_back(host.clone());
        }
        self.icons.insert(host, icon);
        while self.order.len() > MAX_ICONS {
            if let Some(old) = self.order.pop_front() {
                self.icons.remove(&old);
            }
        }
    }

    fn get(&mut self, host: &str) -> Option<Rc<Icon>> {
        if let Some(found) = self.icons.get(host) {
            return found.clone();
        }
        let icon = self
            .dir
            .as_ref()
            .and_then(|dir| fs::read(dir.join(format!("{host}.png"))).ok())
            .and_then(|png| image::decode_square(&png, self.size))
            .map(|pixels| Rc::new(Icon { pixels }));
        self.remember(host.to_string(), icon.clone());
        icon
    }

    fn store(&mut self, arrived: Arrived) -> bool {
        let Some(pixels) = image::decode_square(&arrived.png, self.size) else { return false };
        if arrived.png.len() <= MAX_FILE
            && let Some(dir) = &self.dir
        {
            write_file(dir, &arrived.host, &arrived.png);
        }
        self.remember(arrived.host, Some(Rc::new(Icon { pixels })));
        true
    }
}

fn write_file(dir: &Path, host: &str, png: &[u8]) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(format!("{host}.png"));
    if fs::read(&path).is_ok_and(|old| old == png) {
        return;
    }
    let _ = fs::write(&path, png);
    let files: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((e.path(), meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect();
    for old in to_evict(files, MAX_DISK) {
        let _ = fs::remove_file(old);
    }
}

/// The icon of an address, if one is known (in memory or on disk).
pub fn for_url(url: &str) -> Option<Rc<Icon>> {
    let host = key(url)?;
    CACHE.with(|c| c.try_borrow_mut().ok().and_then(|mut c| c.get(&host)))
}

/// The pixel size icons are drawn at (the window's DPI changed).
pub fn set_scale(scale: f32) {
    let size = (SIZE * scale).round().max(1.0) as u32;
    CACHE.with(|c| c.borrow_mut().set_size(size));
}

/// Decodes what the pages sent since the last call. True when any icon
/// changed (the strip is worth repainting).
pub fn take_arrived() -> bool {
    let arrived = ARRIVED.with(|a| std::mem::take(&mut *a.borrow_mut()));
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        // Every icon is stored, so this can't stop at the first change.
        let mut changed = false;
        for icon in arrived {
            changed |= cache.store(icon);
        }
        changed
    })
}

/// Listens for the page's icon in this view (never a private one).
pub fn watch(webview: &ICoreWebView2, window: HWND) {
    let Ok(view) = webview.cast::<ICoreWebView2_15>() else { return };
    let window = window.0 as isize;
    let handler = FaviconChangedEventHandler::create(Box::new(move |sender, _| {
        if let Some(sender) = sender {
            fetch(&sender, window);
        }
        Ok(())
    }));
    let mut token = 0;
    unsafe {
        let _ = view.add_FaviconChanged(&handler, &mut token);
    }
}

fn source_of(view: &ICoreWebView2) -> String {
    let mut raw = PWSTR::null();
    unsafe {
        if view.Source(&mut raw).is_err() || raw.is_null() {
            return String::new();
        }
        let text = raw.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(raw.0 as *const _));
        text
    }
}

fn fetch(view: &ICoreWebView2, window: isize) {
    let Some(host) = key(&source_of(view)) else { return };
    let Ok(view) = view.cast::<ICoreWebView2_15>() else { return };
    let handler = GetFaviconCompletedHandler::create(Box::new(move |result, stream| {
        let png = result.ok().and(stream).and_then(|s| image::stream_bytes(&s)).unwrap_or_default();
        if png.is_empty() {
            return Ok(());
        }
        let first = ARRIVED.with(|a| {
            let mut a = a.borrow_mut();
            a.push(Arrived { host: host.clone(), png });
            a.len() == 1
        });
        if first {
            unsafe {
                let _ = PostMessageW(Some(HWND(window as *mut _)), WM_APP_FAVICON, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }));
    unsafe {
        let _ = view.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &handler);
    }
}
