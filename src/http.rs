//! A plain HTTP GET over WinHTTP, the client Windows already ships: no TLS
//! stack of our own, the system's proxy and certificate store for free.
//! Used only by the updater, from its own thread.

use std::io::Write;

use windows::{
    Win32::Networking::WinHttp::*,
    core::{HSTRING, PCWSTR},
};

#[derive(Debug, PartialEq, Eq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with '/'.
    pub path: String,
}

/// http(s)://host[:port][/path]. Anything else (credentials, other schemes)
/// is refused rather than half understood.
pub fn parse(url: &str) -> Option<Url> {
    let url = url.trim();
    let (secure, rest) = if let Some(rest) = strip_prefix_ignore_case(url, "https://") {
        (true, rest)
    } else {
        (false, strip_prefix_ignore_case(url, "http://")?)
    };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.is_empty() || authority.contains(['@', ' ', '\\']) {
        return None;
    }
    // A colon inside "[::1]" is part of the address, not a port.
    let (host, port) = match authority.rfind(':') {
        Some(i) if !authority[i..].contains(']') => (&authority[..i], authority[i + 1..].parse().ok()?),
        _ => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    let path = match path {
        "" => "/".to_string(),
        p if p.starts_with('?') => format!("/{p}"),
        p => p.to_string(),
    };
    Some(Url { secure, host: host.to_ascii_lowercase(), port, path })
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    // `get`: the cut may fall inside a character of the text.
    text.get(..prefix.len()).filter(|head| head.eq_ignore_ascii_case(prefix)).map(|_| &text[prefix.len()..])
}

/// Plain http is only for a server on this machine (tests, a local mirror):
/// the signature already protects the package, https keeps the check private.
pub fn allowed(url: &Url) -> bool {
    url.secure || matches!(url.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// `reference` as seen from the document at `base`: absolute URLs stay,
/// "/x" goes to the base's root, anything else sits beside the base.
pub fn resolve(base: &str, reference: &str) -> String {
    let reference = reference.trim();
    if parse(reference).is_some() {
        return reference.to_string();
    }
    let base = base.trim();
    // The query and fragment go first: a '/' inside "?x=/y" is not a path.
    let base = &base[..base.find(['?', '#']).unwrap_or(base.len())];
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let path_start = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if let Some(absolute) = reference.strip_prefix('/') {
        return format!("{}/{absolute}", &base[..path_start]);
    }
    let without_query = base;
    let dir_end = without_query[path_start..].rfind('/').map_or(without_query.len(), |i| path_start + i);
    format!("{}/{reference}", &without_query[..dir_end])
}

struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn open(handle: *mut core::ffi::c_void, what: &str) -> Result<Handle, String> {
    if handle.is_null() { Err(format!("{what}: {}", windows::core::Error::from_thread())) } else { Ok(Handle(handle)) }
}

/// Streams the body of a 200 response into `sink`, at most `limit` bytes.
/// Redirects are followed (WinHTTP never follows https to http).
pub fn get(url: &str, limit: u64, sink: &mut dyn Write) -> Result<u64, String> {
    match fetch(url, limit, sink, None)? {
        Fetched::Body { written, .. } => Ok(written),
        Fetched::NotModified => Err("HTTP 304".into()),
    }
}

/// The result of a conditional GET.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// The server sent the body (status 200). `etag` is its `ETag` header,
    /// or empty.
    Body { written: u64, etag: String },
    /// The entity tag of the request is still current (status 304). The
    /// server sent no body.
    NotModified,
}

/// An entity tag that can go into a request header: printable ASCII only,
/// so that a changed state file cannot add a header line.
pub fn valid_etag(etag: &str) -> bool {
    !etag.is_empty() && etag.len() <= 256 && etag.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The same as `get`, but with `If-None-Match: <etag>` when `etag` is a
/// valid tag. Then a 304 response is `Fetched::NotModified`.
pub fn fetch(url: &str, limit: u64, sink: &mut dyn Write, etag: Option<&str>) -> Result<Fetched, String> {
    let parsed = parse(url).ok_or_else(|| format!("invalid address: {url}"))?;
    if !allowed(&parsed) {
        return Err(format!("https only: {url}"));
    }
    unsafe {
        let agent = HSTRING::from(format!("Nea/{}", env!("CARGO_PKG_VERSION")));
        let session = open(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "WinHttpOpen",
        )?;
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connect =
            open(WinHttpConnect(session.0, &HSTRING::from(parsed.host.as_str()), parsed.port, 0), "WinHttpConnect")?;
        let flags = if parsed.secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = open(
            WinHttpOpenRequest(
                connect.0,
                &HSTRING::from("GET"),
                &HSTRING::from(parsed.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "WinHttpOpenRequest",
        )?;
        let header: Option<Vec<u16>> =
            etag.filter(|t| valid_etag(t)).map(|t| format!("If-None-Match: {t}").encode_utf16().collect());
        WinHttpSendRequest(request.0, header.as_deref(), None, 0, 0, 0)
            .map_err(|e| format!("no connection ({})", e.code()))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("no response ({})", e.code()))?;
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
        .map_err(|e| e.to_string())?;
        if status == 304 && header.is_some() {
            return Ok(Fetched::NotModified);
        }
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let etag = header_text(request.0, WINHTTP_QUERY_ETAG).filter(|t| valid_etag(t)).unwrap_or_default();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buffer.as_mut_ptr() as *mut _, buffer.len() as u32, &mut read)
                .map_err(|e| e.to_string())?;
            if read == 0 {
                return Ok(Fetched::Body { written: total, etag });
            }
            total += read as u64;
            if total > limit {
                return Err(format!("response larger than {limit} bytes"));
            }
            sink.write_all(&buffer[..read as usize]).map_err(|e| e.to_string())?;
        }
    }
}

/// The text of one response header, if the response has it.
///
/// # Safety
/// `request` is an open WinHTTP request that has its response.
unsafe fn header_text(request: *mut core::ffi::c_void, query: u32) -> Option<String> {
    let mut buffer = [0u16; 512];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    unsafe {
        WinHttpQueryHeaders(
            request,
            query,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr() as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )
        .ok()?;
    }
    let len = (size as usize / 2).min(buffer.len());
    String::from_utf16(&buffer[..len]).ok()
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/http.rs"]
#[rustfmt::skip]
mod tests;
