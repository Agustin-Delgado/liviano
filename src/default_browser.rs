//! Nea as the default browser: the entries that make Windows list Nea in
//! Settings → Apps → Default apps, and the link that another app opens.
//!
//! Windows lists a browser only when it finds it under RegisteredApplications,
//! with its Capabilities (the link types and file types it can open) and a
//! ProgID for each. All of it is in HKCU, so no administrator rights.
//! Windows does not let a program make itself the default: the person picks
//! Nea in Settings. The button in Nea's Settings opens that page.
//!
//! A link from another app comes as the first argument (`nea.exe "https://…"`).
//! When this profile's window is already open, the new process hands the link
//! to it (WM_COPYDATA) and exits, so the link opens in a new tab there.

use std::{path::Path, sync::Mutex};

use windows::{
    Win32::{
        Foundation::{ERROR_SUCCESS, HWND, LPARAM, WPARAM},
        System::{
            DataExchange::COPYDATASTRUCT,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
                RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
            },
        },
        UI::{
            Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW},
            WindowsAndMessaging::{
                AllowSetForegroundWindow, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SW_SHOWNORMAL,
                SendMessageTimeoutW, WM_COPYDATA,
            },
        },
    },
    core::{HSTRING, PCWSTR},
};

/// The name under RegisteredApplications and StartMenuInternet.
const APP_KEY: &str = "Nea";
const CLIENT_KEY: &str = r"Software\Clients\StartMenuInternet\Nea";
const CAPABILITIES: &str = r"Software\Clients\StartMenuInternet\Nea\Capabilities";
const REGISTERED: &str = r"Software\RegisteredApplications";
/// The ProgID of web links (http, https).
const URL_PROGID: &str = "NeaURL";
/// The ProgID of web files (.html and others).
const FILE_PROGID: &str = "NeaHTML";
const FILE_TYPES: [&str; 5] = [".htm", ".html", ".shtml", ".xhtml", ".pdf"];
/// Where Windows' default apps page opens, on Nea's entry.
const SETTINGS_URI: &str = "ms-settings:defaultapps?registeredAppUser=Nea";
/// Tags the WM_COPYDATA of a link ("NEAU").
const COPYDATA_LINK: usize = 0x4E45_4155;

/// One registry value under HKCU: key, value name ("" for the default) and
/// text.
pub type Entry = (String, &'static str, String);

/// What Windows needs to list `exe` as a browser.
pub fn entries(exe: &Path) -> Vec<Entry> {
    let exe = exe.display().to_string();
    let icon = format!("\"{exe}\",0");
    let open = format!("\"{exe}\" \"%1\"");
    let mut list: Vec<Entry> = Vec::new();
    for (progid, name) in [(URL_PROGID, "Nea URL"), (FILE_PROGID, "Nea HTML Document")] {
        let key = format!(r"Software\Classes\{progid}");
        list.push((key.clone(), "", name.to_string()));
        list.push((format!(r"{key}\Application"), "ApplicationName", "Nea".to_string()));
        list.push((format!(r"{key}\Application"), "ApplicationIcon", icon.clone()));
        list.push((format!(r"{key}\DefaultIcon"), "", icon.clone()));
        list.push((format!(r"{key}\shell\open\command"), "", open.clone()));
    }
    list.push((CLIENT_KEY.to_string(), "", "Nea".to_string()));
    list.push((format!(r"{CLIENT_KEY}\DefaultIcon"), "", icon.clone()));
    list.push((format!(r"{CLIENT_KEY}\shell\open\command"), "", format!("\"{exe}\"")));
    list.push((CAPABILITIES.to_string(), "ApplicationName", "Nea".to_string()));
    list.push((
        CAPABILITIES.to_string(),
        "ApplicationDescription",
        "A web browser that uses little memory.".to_string(),
    ));
    list.push((CAPABILITIES.to_string(), "ApplicationIcon", icon));
    list.push((format!(r"{CAPABILITIES}\StartMenu"), "StartMenuInternet", APP_KEY.to_string()));
    for scheme in ["http", "https"] {
        list.push((format!(r"{CAPABILITIES}\URLAssociations"), scheme, URL_PROGID.to_string()));
    }
    for ext in FILE_TYPES {
        list.push((format!(r"{CAPABILITIES}\FileAssociations"), ext, FILE_PROGID.to_string()));
    }
    list.push((REGISTERED.to_string(), APP_KEY, CAPABILITIES.to_string()));
    list
}

fn read(key: &str, name: &str) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let name = HSTRING::from(name);
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            if name.is_empty() { PCWSTR::null() } else { PCWSTR(name.as_ptr()) },
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn write(key: &str, name: &str, text: &str) -> bool {
    unsafe {
        let mut handle = HKEY::default();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut handle,
            None,
        );
        if status != ERROR_SUCCESS {
            return false;
        }
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let name = HSTRING::from(name);
        let status = RegSetValueExW(
            handle,
            if name.is_empty() { PCWSTR::null() } else { PCWSTR(name.as_ptr()) },
            None,
            REG_SZ,
            Some(bytes),
        );
        let _ = RegCloseKey(handle);
        status == ERROR_SUCCESS
    }
}

/// Writes the entries for `exe`, when one is missing or points at another
/// file. Returns false when a write failed.
pub fn register(exe: &Path) -> bool {
    let list = entries(exe);
    if list.iter().all(|(key, name, text)| read(key, name).as_deref() == Some(text.as_str())) {
        return true;
    }
    let ok = list.iter().fold(true, |ok, (key, name, text)| write(key, name, text) && ok);
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    ok
}

/// Takes the entries back out (at uninstall).
pub fn unregister() {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(REGISTERED), &HSTRING::from(APP_KEY));
        for key in [
            CLIENT_KEY.to_string(),
            format!(r"Software\Classes\{URL_PROGID}"),
            format!(r"Software\Classes\{FILE_PROGID}"),
        ] {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// The Settings button: registers this executable and opens Windows' page
/// where the person picks Nea. Returns the line for the status note.
pub fn open_windows_settings() -> &'static str {
    let Ok(exe) = std::env::current_exe() else {
        return "Cannot find the program.";
    };
    if !register(&exe) {
        return "Cannot write the browser entry in the registry.";
    }
    unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(SETTINGS_URI),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
    "In Windows Settings, select Nea and then Set default."
}

/// "%20" for a space: the characters a path can hold that a URL cannot.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        match c {
            '\\' => out.push('/'),
            ' ' | '#' | '%' | '?' | '"' | '<' | '>' | '`' | '{' | '}' | '|' | '^' => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A link Nea may open from outside: a web address, a file URL, or the path
/// of a local file (turned into a file URL). Anything else (a `javascript:`
/// or another app's scheme) is not opened.
pub fn link(text: &str) -> Option<String> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if ["http://", "https://", "file://"].iter().any(|p| lower.starts_with(p)) {
        return Some(text.to_string());
    }
    // C:\folder\page.html, or \\server\share\page.html.
    let bytes = text.as_bytes();
    let drive =
        bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/');
    if drive {
        return Some(format!("file:///{}", encode_path(text)));
    }
    if text.starts_with(r"\\") && text.len() > 2 {
        return Some(format!("file:{}", encode_path(text)));
    }
    None
}

/// The link on the command line: the first argument that is not one of
/// Nea's own (`--profile 2`, `--install`…).
pub fn link_from_args<I: IntoIterator<Item = std::ffi::OsString>>(args: I) -> Option<String> {
    let args: Vec<String> = args.into_iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let mut skip_next = false;
    for arg in &args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == crate::profiles::ARG {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--") {
            continue;
        }
        return link(arg);
    }
    None
}

/// The link this process started with, for the window to open once it is up.
static STARTUP_LINK: Mutex<Option<String>> = Mutex::new(None);

pub fn take_startup_link() -> Option<String> {
    STARTUP_LINK.lock().ok()?.take()
}

/// At start: a link for a profile whose window is open goes to that window,
/// and this process ends (`true`). Otherwise it waits for this process's own
/// window.
pub fn hand_over(link: String, profile: u32) -> bool {
    if let Some(hwnd) = crate::profile_launch::find_window(profile)
        && send_link(hwnd, &link)
    {
        return true;
    }
    if let Ok(mut slot) = STARTUP_LINK.lock() {
        *slot = Some(link);
    }
    false
}

fn send_link(hwnd: HWND, link: &str) -> bool {
    let wide: Vec<u16> = link.encode_utf16().collect();
    let data =
        COPYDATASTRUCT { dwData: COPYDATA_LINK, cbData: (wide.len() * 2) as u32, lpData: wide.as_ptr() as *mut _ };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let mut result = 0usize;
        let sent = SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&data as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            5000,
            Some(&mut result),
        );
        sent.0 != 0 && result == 1
    }
}

/// The link in a WM_COPYDATA that another Nea sent, checked again here: the
/// message can come from any program of this user.
pub fn received_link(lparam: LPARAM) -> Option<String> {
    if lparam.0 == 0 {
        return None;
    }
    let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    if data.dwData != COPYDATA_LINK || data.lpData.is_null() || data.cbData % 2 != 0 || data.cbData > 64 * 1024 {
        return None;
    }
    let units = unsafe { std::slice::from_raw_parts(data.lpData as *const u16, data.cbData as usize / 2) };
    link(&String::from_utf16_lossy(units))
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/default_browser.rs"]
#[rustfmt::skip]
mod tests;
