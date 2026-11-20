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
    (text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &text[prefix.len()..])
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
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let path_start = base[scheme_end..].find('/').map_or(base.len(), |i| scheme_end + i);
    if let Some(absolute) = reference.strip_prefix('/') {
        return format!("{}/{absolute}", &base[..path_start]);
    }
    let without_query = &base[..base.find('?').unwrap_or(base.len())];
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
    let parsed = parse(url).ok_or_else(|| format!("dirección inválida: {url}"))?;
    if !allowed(&parsed) {
        return Err(format!("solo https: {url}"));
    }
    unsafe {
        let agent = HSTRING::from(format!("Liviano/{}", env!("CARGO_PKG_VERSION")));
        let session = open(
            WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "WinHttpOpen",
        )?;
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connect = open(WinHttpConnect(session.0, &HSTRING::from(parsed.host.as_str()), parsed.port, 0), "WinHttpConnect")?;
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
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| format!("sin conexión ({})", e.code()))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| format!("sin respuesta ({})", e.code()))?;
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
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buffer.as_mut_ptr() as *mut _, buffer.len() as u32, &mut read)
                .map_err(|e| e.to_string())?;
            if read == 0 {
                return Ok(total);
            }
            total += read as u64;
            if total > limit {
                return Err(format!("respuesta de más de {limit} bytes"));
            }
            sink.write_all(&buffer[..read as usize]).map_err(|e| e.to_string())?;
        }
    }
}

