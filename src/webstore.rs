//! The Chrome Web Store: which extension a pasted link is about, and the
//! package download (WinHTTP, so no HTTP crate ships in the executable).

use windows::{
    Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
        WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
        WinHttpSetTimeouts,
    },
    core::{HSTRING, PCWSTR, w},
};

use crate::crx::is_extension_id;

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

/// Where the store serves the CRX3 for `id` to a browser of `version`.
pub fn download_url(id: &str, version: &str) -> String {
    format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={version}\
         &acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    )
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

/// GET over https, following redirects (WinHTTP's default), into memory.
/// Blocking: call it from a worker thread.
pub fn download(url: &str) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("https only")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let network = |e: windows::core::Error| format!("the download failed ({e})");
    unsafe {
        let session =
            handle(WinHttpOpen(w!("Nea"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
        let connection = handle(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0))?;
        let request = handle(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_OPEN_REQUEST_FLAGS(WINHTTP_FLAG_SECURE.0),
        ))?;
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
            204 | 404 => {
                return Err(
                    "the Chrome Web Store does not have that extension (or it is not available for this browser)"
                        .to_string(),
                );
            }
            other => return Err(format!("the Chrome Web Store answered {other}")),
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
        Ok(body)
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/webstore.rs"]
#[rustfmt::skip]
mod tests;
