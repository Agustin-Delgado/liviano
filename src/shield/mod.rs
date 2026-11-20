//! RF-13: ad and tracker blocking at the network level.
//!
//! Every request of every view passes through `WebResourceRequested`, and the
//! engine answers there and then, on the UI thread: a request waits for the
//! handler, so this is the one engine callback that decides instead of
//! queueing a `web::Event`. It reads only state owned here (the compiled
//! filters and the hosts with blocking off), never the app's.
//!
//! The filters are compiled on a background thread (`lists`) and handed over
//! through `INCOMING`; the UI thread picks them up on its next request.

pub mod filters;
mod lists;

use std::{
    cell::RefCell,
    collections::{BTreeSet, HashSet},
    rc::Rc,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::System::Com::IStream,
    core::{Interface, PWSTR, Result, w},
};

use crate::{omnibox, storage};
use filters::{Filters, Kind};

static INCOMING: Mutex<Option<Filters>> = Mutex::new(None);
static HAS_INCOMING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Requests seen and blocked since start, for the script control pipe: what
/// lets a measurement tell "blocking on" from "blocking off" by its effect.
static SEEN: AtomicU64 = AtomicU64::new(0);
static BLOCKED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static ENGINE: RefCell<Option<Filters>> = const { RefCell::new(None) };
    /// Hosts (as `display_host` writes them) where blocking is off.
    static DISABLED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Starts compiling the lists, once. Called when the web engine is first
/// needed: with no page open, no filter is held in memory.
pub fn start() {
    if !STARTED.swap(true, Ordering::Relaxed) {
        lists::spawn(storage::data_dir().join("shield"));
    }
}

/// Called from the background thread with a freshly compiled engine.
fn deliver(filters: Filters) {
    if let Ok(mut slot) = INCOMING.lock() {
        *slot = Some(filters);
        HAS_INCOMING.store(true, Ordering::Release);
    }
}

/// The per-site exceptions, from settings.
pub fn set_disabled(hosts: &BTreeSet<String>) {
    DISABLED.with(|d| *d.borrow_mut() = hosts.iter().cloned().collect());
}

/// The settings key of a page: its host without "www.".
pub fn site_key(url: &str) -> &str {
    omnibox::display_host(url)
}

/// What the engine has done so far and what it holds. Read on the UI
/// thread, which owns the engine.
pub struct Stats {
    pub requests: u64,
    pub blocked: u64,
    /// None while the first compile is still running.
    pub rules: Option<usize>,
    pub heap_bytes: Option<usize>,
}

pub fn stats() -> Stats {
    let (rules, heap_bytes) = with_engine(|f| (f.len(), f.heap_bytes())).unzip();
    Stats { requests: SEEN.load(Ordering::Relaxed), blocked: BLOCKED.load(Ordering::Relaxed), rules, heap_bytes }
}

fn with_engine<R>(f: impl FnOnce(&Filters) -> R) -> Option<R> {
    ENGINE.with(|engine| {
        if HAS_INCOMING.swap(false, Ordering::Acquire) {
            if let Some(new) = INCOMING.lock().ok().and_then(|mut slot| slot.take()) {
                // The old engine is dropped here, on the thread that used it.
                *engine.borrow_mut() = Some(new);
            }
        }
        engine.borrow().as_ref().map(f)
    })
}

fn kind_of(context: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> Kind {
    match context {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => Kind::Subdocument,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => Kind::Stylesheet,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => Kind::Image,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_TEXT_TRACK => Kind::Media,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => Kind::Font,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => Kind::Script,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST
        | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH
        | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_EVENT_SOURCE => Kind::Xhr,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => Kind::Websocket,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_CSP_VIOLATION_REPORT => Kind::Ping,
        _ => Kind::Other,
    }
}

/// The decision for one request made by the page `page`.
fn should_block(url: &str, page: &str, context: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> bool {
    // The page itself is never blocked (its frames are).
    if context == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT && url == page {
        return false;
    }
    if DISABLED.with(|d| d.borrow().contains(site_key(page))) {
        return false;
    }
    with_engine(|f| f.should_block(url, page, kind_of(context))).unwrap_or(false)
}

/// Wires a new view: every request goes through the filters.
pub fn attach(webview: &ICoreWebView2) -> Result<()> {
    unsafe {
        let env = webview.cast::<ICoreWebView2_2>()?.Environment()?;
        // Frames and workers too where the runtime can tell them apart.
        match webview.cast::<ICoreWebView2_22>() {
            Ok(w22) => w22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                w!("*"),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
            )?,
            Err(_) => webview.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?,
        }

        // The top-level page the requests are made for, known from the start
        // of its navigation (`Source` only changes once it commits).
        let page = Rc::new(RefCell::new(String::new()));
        let mut token = 0i64;
        let navigating = page.clone();
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    *navigating.borrow_mut() = take_pwstr(uri);
                }
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Request()?.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                args.ResourceContext(&mut context)?;
                SEEN.fetch_add(1, Ordering::Relaxed);
                if should_block(&uri, &page.borrow(), context) {
                    BLOCKED.fetch_add(1, Ordering::Relaxed);
                    #[cfg(debug_assertions)]
                    eprintln!("shield: blocked {uri}");
                    let response = env.CreateWebResourceResponse(None::<&IStream>, 403, w!("Blocked"), w!(""))?;
                    args.SetResponse(&response)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

