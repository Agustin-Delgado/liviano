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
//!
//! Cookie banners: a small script registered once per view asks, as each
//! document is created, for the selectors of the cookie list that apply to
//! its host (`cosmetic`), with a synchronous request this handler answers
//! itself. A registration made when the navigation starts arrives too late
//! for it (measured: ~500 ms, after the first frame), and one per view with
//! every selector in it would keep ~300 KB per tab in the engine and parse
//! them in every frame. Later requests to the same path carry the ids and
//! classes the page has (`?t=`) and get the generic selectors they pick.

pub mod cosmetic;
pub mod filters;
mod lists;
pub mod popups;

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashSet},
    rc::Rc,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{Foundation::E_POINTER, System::Com::IStream, UI::Shell::SHCreateMemStream},
    core::{HSTRING, Interface, PWSTR, Result, w},
};

use crate::{inject, omnibox, storage};
use cosmetic::Cosmetic;
use filters::{Filters, Kind};

const COOKIE_HIDE: &str = include_str!("../../assets/cookie-hide.js");
const COOKIE_BOOT: &str = include_str!("../../assets/cookie-boot.js");

static INCOMING: Mutex<Option<Filters>> = Mutex::new(None);
static HAS_INCOMING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static INCOMING_COSMETIC: Mutex<Option<CookieList>> = Mutex::new(None);
static HAS_INCOMING_COSMETIC: AtomicBool = AtomicBool::new(false);
/// Whether cookie banners are hidden (`block_trackers`). Read by the list
/// thread too: with it off, the cookie list is not compiled at all.
static COSMETIC_WANTED: AtomicBool = AtomicBool::new(true);
/// Requests seen and blocked since start, for the script control pipe: what
/// lets a measurement tell "blocking on" from "blocking off" by its effect.
static SEEN: AtomicU64 = AtomicU64::new(0);
static BLOCKED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static ENGINE: RefCell<Option<Filters>> = const { RefCell::new(None) };
    /// Hosts (as `display_host` writes them) where blocking is off.
    static DISABLED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static COSMETIC: RefCell<Option<CookieList>> = const { RefCell::new(None) };
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

/// The cookie list: its element hiding, and its network rules (scripts of
/// consent managers the list knows are safe to stop, site by site). Kept
/// apart from the ad lists so its switch needs no recompiling of those.
pub struct CookieList {
    hiding: Cosmetic,
    network: Filters,
}

impl CookieList {
    pub fn compile<'a>(lists: impl IntoIterator<Item = &'a str> + Clone) -> Self {
        CookieList { hiding: Cosmetic::compile(lists.clone()), network: Filters::compile(lists) }
    }

    pub fn len(&self) -> usize {
        self.hiding.len() + self.network.len()
    }

    pub fn heap_bytes(&self) -> usize {
        self.hiding.heap_bytes() + self.network.heap_bytes()
    }
}

fn deliver_cosmetic(cosmetic: CookieList) {
    if let Ok(mut slot) = INCOMING_COSMETIC.lock() {
        *slot = Some(cosmetic);
        HAS_INCOMING_COSMETIC.store(true, Ordering::Release);
    }
}

/// Cookie banners on or off (`block_trackers`). Off drops the compiled
/// list; on again compiles it from the cache, off the UI thread.
pub fn set_cosmetic(on: bool) {
    let was = COSMETIC_WANTED.swap(on, Ordering::Relaxed);
    if !on {
        COSMETIC.with(|c| *c.borrow_mut() = None);
    } else if !was && STARTED.load(Ordering::Relaxed) {
        lists::spawn_cosmetic(storage::data_dir().join("shield"));
    }
}

fn with_cosmetic<R>(f: impl FnOnce(&CookieList) -> R) -> Option<R> {
    COSMETIC.with(|cosmetic| {
        if HAS_INCOMING_COSMETIC.swap(false, Ordering::Acquire) {
            if let Some(new) = INCOMING_COSMETIC.lock().ok().and_then(|mut slot| slot.take()) {
                if COSMETIC_WANTED.load(Ordering::Relaxed) {
                    *cosmetic.borrow_mut() = Some(new);
                }
            }
        }
        cosmetic.borrow().as_ref().map(f)
    })
}

/// The cookie-banner selectors for a page at `url`, as a JSON array; None
/// when there is nothing to hide there (blocking off, or not a web page).
pub fn cookie_selectors(url: &str) -> Option<String> {
    if !COSMETIC_WANTED.load(Ordering::Relaxed) || !is_on(url) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    let host = filters::host_of(&lower)?.1.trim_end_matches('.').to_string();
    // `?t=` carries the ids and classes the page has found since it last
    // asked (ids and classes are case-sensitive: read from `url`, not
    // `lower`).
    let tokens = url.split_once("?t=").map(|(_, t)| t);
    with_cosmetic(|c| {
        let selectors = match tokens {
            Some(t) => c.hiding.selectors_for_tokens(&host, t.split(',')),
            None => c.hiding.selectors_for(&host),
        };
        serde_json::to_string(&selectors).ok()
    })
    .flatten()
}

/// The same, as a script run on a page already created.
fn cookie_script(url: &str) -> Option<String> {
    let selectors = cookie_selectors(url)?;
    Some(format!("(() => {{ if (window.top !== window) return; {COOKIE_HIDE} livianoHideCookies({selectors}, {path:?}); }})();", path = cookie_path()))
}

/// The path the boot script asks for the selectors at, on the page's own
/// origin (so no cross-origin rule applies). The token, made once per run,
/// keeps a page from telling this app apart by asking for it.
fn cookie_path() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        format!("/__liviano-{:016x}/cookies.json", hasher.finish())
    })
}

/// Whether `url` is the boot script's request.
fn is_cookie_request(url: &str) -> bool {
    let Some((at, host)) = filters::host_of(url) else { return false };
    url[at + host.len()..].split_once('/').is_some_and(|(_, rest)| {
        let path = &cookie_path()[1..];
        rest.strip_prefix(path).is_some_and(|tail| tail.is_empty() || tail.starts_with('?'))
    })
}

fn boot_script() -> String {
    inject::fill(COOKIE_BOOT, &[("PATH", cookie_path()), ("HIDE", COOKIE_HIDE)])
}

/// The per-site exceptions, from settings.
pub fn set_disabled(hosts: &BTreeSet<String>) {
    DISABLED.with(|d| *d.borrow_mut() = hosts.iter().cloned().collect());
}

/// The settings key of a page: its host without "www.".
pub fn site_key(url: &str) -> &str {
    omnibox::display_host(url)
}

/// Whether ad blocking is on for the page `url` (no per-site exception).
pub fn is_on(url: &str) -> bool {
    !DISABLED.with(|d| d.borrow().contains(site_key(url)))
}

/// The decision for a new window the page `opener` asked for. `allowed`:
/// the person allowed popups on that site.
pub fn popup_verdict(opener: &str, target: &str, user_initiated: bool, allowed: bool) -> popups::Verdict {
    let request = popups::Request { opener, target, user_initiated };
    popups::decide(&request, allowed, is_on(opener), || {
        with_engine(|f| f.should_block(target, opener, Kind::Popup)).unwrap_or(false)
    })
}

/// What the engine has done so far and what it holds. Read on the UI
/// thread, which owns the engine.
pub struct Stats {
    pub requests: u64,
    pub blocked: u64,
    /// None while the first compile is still running.
    pub rules: Option<usize>,
    pub heap_bytes: Option<usize>,
    /// The cookie list (None while compiling, or with it off).
    pub cosmetic_rules: Option<usize>,
    pub cosmetic_heap_bytes: Option<usize>,
}

pub fn stats() -> Stats {
    let (rules, heap_bytes) = with_engine(|f| (f.len(), f.heap_bytes())).unzip();
    let (cosmetic_rules, cosmetic_heap_bytes) = with_cosmetic(|c| (c.len(), c.heap_bytes())).unzip();
    Stats {
        requests: SEEN.load(Ordering::Relaxed),
        blocked: BLOCKED.load(Ordering::Relaxed),
        rules,
        heap_bytes,
        cosmetic_rules,
        cosmetic_heap_bytes,
    }
}

fn with_engine<R>(f: impl FnOnce(&Filters) -> R) -> Option<R> {
    ENGINE.with(|engine| {
        if HAS_INCOMING.swap(false, Ordering::Acquire) {
            if let Some(new) = INCOMING.lock().ok().and_then(|mut slot| slot.take()) {
                // Freeing the old one (hundreds of thousands of rules) is
                // left to another thread: this one is answering a request.
                if let Some(old) = engine.borrow_mut().replace(new) {
                    let _ = std::thread::Builder::new().name("shield-drop".into()).spawn(move || drop(old));
                }
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
    if !is_on(page) {
        return false;
    }
    let kind = kind_of(context);
    with_engine(|f| f.should_block(url, page, kind)).unwrap_or(false)
        || with_cosmetic(|c| c.network.should_block(url, page, kind)).unwrap_or(false)
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
        // Whether this navigation's document asked for its cookie selectors.
        let served = Rc::new(Cell::new(false));
        let mut token = 0i64;
        let navigating = page.clone();
        let restarted = served.clone();
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    *navigating.borrow_mut() = take_pwstr(uri);
                    restarted.set(false);
                }
                Ok(())
            })),
            &mut token,
        )?;
        // The document's policy may forbid the request: then the selectors
        // come now, a little later (the first frame may show the banner).
        let asked = served.clone();
        webview.add_ContentLoading(
            &ContentLoadingEventHandler::create(Box::new(move |sender, _| {
                let Some(sender) = sender else { return Ok(()) };
                if asked.get() {
                    return Ok(());
                }
                let mut source = PWSTR::null();
                sender.Source(&mut source)?;
                if let Some(script) = cookie_script(&take_pwstr(source)) {
                    inject::execute_without_gesture(&sender, &script);
                }
                Ok(())
            })),
            &mut token,
        )?;
        let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(())));
        webview.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(boot_script()), &handler)?;
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let _doing = crate::diag::doing(&"filtro:solicitud", 0);
                let Some(args) = args else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Request()?.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                args.ResourceContext(&mut context)?;
                if is_cookie_request(&uri) {
                    served.set(true);
                    let response = match cookie_selectors(&uri) {
                        Some(json) => env.CreateWebResourceResponse(
                            &SHCreateMemStream(Some(json.as_bytes())).ok_or_else(|| windows::core::Error::from(E_POINTER))?,
                            200,
                            w!("OK"),
                            w!("Content-Type: application/json\r\nCache-Control: no-store"),
                        )?,
                        None => env.CreateWebResourceResponse(None::<&IStream>, 204, w!("No Content"), w!(""))?,
                    };
                    args.SetResponse(&response)?;
                    return Ok(());
                }
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

