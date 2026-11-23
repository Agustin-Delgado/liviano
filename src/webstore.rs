//! The Chrome Web Store: which extension a pasted link is about, the
//! script that gives the store page's install button to Nea, and the
//! package download (WinHTTP, so no HTTP crate ships in the executable).

use serde::Deserialize;
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2,
};
use windows::{
    Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders, WinHttpCloseHandle,
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::{crx::is_extension_id, http};

/// Packages above this are not extensions anybody should install.
const MAX_DOWNLOAD: usize = 256 << 20;

/// The extension id of a Web Store page link, in either the current form
/// (chromewebstore.google.com/detail/<slug>/<id>) or the old one
/// (chrome.google.com/webstore/detail/<slug>/<id>), with or without slug,
/// scheme, query or fragment.
pub fn extension_id_from_link(text: &str) -> Option<String> {
    let text = text.trim();
    if text.contains(char::is_whitespace) {
        return None;
    }
    let rest = text.split_once("://").map_or(text, |(scheme, rest)| {
        if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") { rest } else { "" }
    });
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = rest.split_once('/')?;
    let path = match host.to_ascii_lowercase().as_str() {
        "chromewebstore.google.com" => path.strip_prefix("detail/")?,
        "chrome.google.com" => path.strip_prefix("webstore/detail/")?,
        _ => return None,
    };
    // The id is the last segment that looks like one (the slug never does).
    path.split('/').rev().find(|segment| is_extension_id(segment)).map(str::to_string)
}

/// The update service that serves store packages. Its cookies are the only
/// ones a download may carry (`cookies_allowed`).
pub const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/crx";

/// Where the store serves the CRX3 for `id` to a browser of `version`.
pub fn download_url(id: &str, version: &str) -> String {
    format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={version}\
         &acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    )
}

/// The page script that gives `chrome.webstorePrivate` installs to Nea.
pub const PAGE_SCRIPT: &str = include_str!("../assets/webstore.js");

/// Runs `PAGE_SCRIPT` in every document of the view. The script returns at
/// once on any page that is not the store. Its messages reach the app with
/// the other page messages (`Event::WebMessage`).
pub fn watch(webview: &ICoreWebView2) {
    unsafe {
        let _ = webview.AddScriptToExecuteOnDocumentCreated(
            &HSTRING::from(PAGE_SCRIPT),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
        );
    }
}

/// The store page origin. Only messages from it can start an install.
const STORE_ORIGIN: &str = "https://chromewebstore.google.com";

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    nea: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    id: String,
}

/// What the store page script asks of Nea.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRequest {
    /// The store extensions that Nea has (for the state of the button).
    List,
    Install(String),
    Remove(String),
}

/// A request from the store page script, or None for any other message.
/// The origin comes from the engine, not from the page.
pub fn store_request(source: &str, json: &str) -> Option<StoreRequest> {
    let raw: RawMessage = serde_json::from_str(json).ok()?;
    if raw.nea != "webstore" {
        return None;
    }
    let origin = source.get(..STORE_ORIGIN.len()).filter(|o| o.eq_ignore_ascii_case(STORE_ORIGIN))?;
    let rest = &source[origin.len()..];
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    match raw.kind.as_str() {
        "list" => Some(StoreRequest::List),
        "install" if is_extension_id(&raw.id) => Some(StoreRequest::Install(raw.id)),
        "remove" if is_extension_id(&raw.id) => Some(StoreRequest::Remove(raw.id)),
        _ => None,
    }
}

/// What the page receives when Nea is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Installed,
    Cancelled,
    Failed,
}

/// The script that gives the page its answer for `id`: the result codes of
/// `beginInstallWithManifest3` ("" is a success), and the name and version
/// that the page shows for an installed extension.
pub fn answer_script(id: &str, answer: Answer, name: &str, version: &str) -> String {
    let code = match answer {
        Answer::Installed => "",
        Answer::Cancelled => "user_cancelled",
        Answer::Failed => "unknown_error",
    };
    let text = crate::inject::js_string;
    format!(
        "window.__neaWebstoreAnswer && window.__neaWebstoreAnswer({}, {}, {}, {})",
        text(id),
        text(code),
        text(name),
        text(version)
    )
}

/// One store extension of Nea, as the page sees it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
}

/// The script that gives the page the store extensions of Nea.
pub fn list_script(items: &[Listed]) -> String {
    let json = serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string());
    format!("window.__neaWebstoreList && window.__neaWebstoreList({json})")
}

/// The script that tells the page whether `id` was removed.
pub fn removed_script(id: &str, removed: bool) -> String {
    format!("window.__neaWebstoreRemoved && window.__neaWebstoreRemoved({}, {removed})", crate::inject::js_string(id))
}

/// The extension id of a package download that the engine starts itself
/// (its own store install, which cannot work in WebView2): the query asks
/// for a CRX, has `x=id%3D<id>...`, and the engine tags it as an install
/// (`installsource=ondemand` in `x`, or the removed host that WebView2 puts
/// in place of the update service). A CRX that a person downloads from a
/// link stays a normal download.
pub fn engine_package_download(url: &str) -> Option<String> {
    let (address, query) = url.split_once('?')?;
    let query = query.split('#').next().unwrap_or_default();
    let removed_host = http::parse(address).is_some_and(|u| u.host == "permanently-removed.invalid");
    let mut crx = false;
    let mut install = removed_host;
    let mut id = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "acceptformat" => crx |= value.contains("crx"),
            "x" => {
                let value = value.replace("%3D", "=").replace("%3d", "=").replace("%26", "&");
                install |= value.split('&').any(|p| p == "installsource=ondemand");
                id = value
                    .split('&')
                    .find_map(|p| p.strip_prefix("id="))
                    .filter(|i| is_extension_id(i))
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    id.filter(|_| crx && install)
}

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

fn handle(raw: *mut core::ffi::c_void) -> Result<Handle, String> {
    if raw.is_null() {
        Err(format!("no connection ({})", windows::core::Error::from_thread()))
    } else {
        Ok(Handle(raw))
    }
}

/// A redirect chain longer than this is a loop.
const MAX_REDIRECTS: usize = 6;

/// Whether a request to `host` may carry the store cookies: only Google's
/// update service itself. The package host (googleusercontent.com) and
/// any other host never get them.
pub fn cookies_allowed(host: &str) -> bool {
    host.eq_ignore_ascii_case("clients2.google.com")
}

enum Response {
    Body(Vec<u8>),
    Redirect(String),
    Status(u32),
}

/// One GET over https, with no automatic redirect, into memory.
fn get_once(url: &http::Url, cookie: Option<&str>) -> Result<Response, String> {
    let network = |e: windows::core::Error| format!("the download failed ({e})");
    unsafe {
        let session =
            handle(WinHttpOpen(w!("Nea"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
        let connection = handle(WinHttpConnect(session.0, &HSTRING::from(url.host.as_str()), url.port, 0))?;
        let request = handle(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(url.path.as_str()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_OPEN_REQUEST_FLAGS(WINHTTP_FLAG_SECURE.0),
        ))?;
        // Each hop is checked here: the cookies must not follow a redirect.
        let _ = WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes()),
        );
        if let Some(cookie) = cookie.filter(|c| !c.is_empty() && !c.contains(['\r', '\n'])) {
            let header: Vec<u16> = format!("Cookie: {cookie}").encode_utf16().collect();
            WinHttpAddRequestHeaders(request.0, &header, WINHTTP_ADDREQ_FLAG_ADD).map_err(network)?;
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(network)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(network)?;
        let mut status = 0u32;
        let mut size = 4u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(network)?;
        match status {
            200 => {}
            301 | 302 | 303 | 307 | 308 => {
                let mut buffer = vec![0u16; 4096];
                let mut size = (buffer.len() * 2) as u32;
                WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_LOCATION,
                    PCWSTR::null(),
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                    std::ptr::null_mut(),
                )
                .map_err(|_| "the Chrome Web Store sent a redirect with no address".to_string())?;
                let length = (size as usize / 2).min(buffer.len());
                return Ok(Response::Redirect(String::from_utf16_lossy(&buffer[..length])));
            }
            other => return Ok(Response::Status(other)),
        }
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr().cast(), chunk.len() as u32, &mut read).map_err(network)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > MAX_DOWNLOAD {
                return Err("the package is too large".to_string());
            }
        }
        Ok(Response::Body(body))
    }
}

/// GET over https into memory, following redirects (https only). `cookie`
/// is the store session of the engine profile: as in Chromium, Nea tries
/// without it first and sends it again only when the store refuses (an
/// extension that is only for signed-in persons). Blocking: call it from a
/// worker thread.
pub fn download(url: &str, cookie: Option<&str>) -> Result<Vec<u8>, String> {
    match fetch(url, None)? {
        Ok(body) => Ok(body),
        Err(status) if cookie.is_some_and(|c| !c.is_empty()) && matches!(status, 401 | 403 | 404) => {
            fetch(url, cookie)?.map_err(status_message)
        }
        Err(status) => Err(status_message(status)),
    }
}

/// The body, or the status of the last hop when it is not a success.
fn fetch(url: &str, cookie: Option<&str>) -> Result<Result<Vec<u8>, u32>, String> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let parsed = http::parse(&current).filter(|u| u.secure).ok_or("the download address is not https")?;
        let hop_cookie = cookie.filter(|_| cookies_allowed(&parsed.host));
        match get_once(&parsed, hop_cookie)? {
            Response::Body(body) => return Ok(Ok(body)),
            Response::Status(status) => return Ok(Err(status)),
            Response::Redirect(location) => current = http::resolve(&current, &location),
        }
    }
    Err("the Chrome Web Store sent too many redirects".to_string())
}

/// What the person is told for a refused download.
pub fn status_message(status: u32) -> String {
    match status {
        204 | 404 => "the Chrome Web Store does not give this extension to Nea. It is not available for this \
                      browser, for your country, or for a person who is not signed in to the store"
            .to_string(),
        401 | 403 => {
            "the Chrome Web Store refused the download. Sign in to the store in Nea, then try again".to_string()
        }
        other => format!("the Chrome Web Store answered {other}"),
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/webstore.rs"]
#[rustfmt::skip]
mod tests;
