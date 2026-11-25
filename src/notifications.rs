//! Web notifications as Windows notifications (toasts), as Chrome shows
//! them: the page's title and text, the site under them, a click that
//! brings Nea forward on the tab that sent it.
//!
//! Windows shows a toast only for a known application ID. Nea is not a
//! packaged app, so the ID is registered under
//! `HKCU\Software\Classes\AppUserModelId`, with the name and the icon the
//! toast shows. The process keeps its own taskbar identity: the shortcuts
//! and pins the person made stay what they were.
//!
//! The toast's events (a click, a dismissal) come on a thread of the
//! system: they queue an `Outcome` and post one message to the window,
//! which answers on its own thread (`take_outcomes`).

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicIsize, Ordering},
    },
};

use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification,
        ToastNotificationManager, ToastNotifier,
    },
    Win32::{
        Foundation::{ERROR_SUCCESS, HWND, LPARAM, WPARAM},
        System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW},
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
    core::{HSTRING, IInspectable},
};

use crate::storage;

/// The application ID that Nea's toasts carry. A development build has its
/// own, so its toasts never mix with those of the installed Nea.
pub const AUMID: &str = if cfg!(debug_assertions) { "Nea.Browser.Development" } else { "Nea.Browser" };

/// Posted to the window when a toast was clicked, closed or failed.
pub const WM_APP_TOAST: u32 = WM_APP + 0x70;

/// What happened to a toast, by the key the app gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Clicked(u64),
    /// The person closed it, or it could not be shown.
    Closed(u64),
}

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static OUTCOMES: Mutex<VecDeque<Outcome>> = Mutex::new(VecDeque::new());

pub fn set_window(hwnd: HWND) {
    WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
}

fn report(outcome: Outcome) {
    let was_empty = {
        let Ok(mut queue) = OUTCOMES.lock() else { return };
        let was_empty = queue.is_empty();
        queue.push_back(outcome);
        was_empty
    };
    if was_empty {
        let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_TOAST, WPARAM(0), LPARAM(0));
        }
    }
}

/// What the toasts said since the last call, oldest first.
pub fn take_outcomes() -> Vec<Outcome> {
    OUTCOMES.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default()
}

// ---------- the pure parts ----------

/// The origin a decision is kept under: scheme, host and a port other than
/// the scheme's own, in lower case. "https://App.Slack.com/client/T1" ->
/// "https://app.slack.com". Only secure pages may notify, as in Chrome:
/// https, and http on the computer itself.
pub fn origin_of(uri: &str) -> Option<String> {
    let uri = uri.trim();
    let (scheme, rest) = uri.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next()?;
    // Credentials never go into the key.
    let authority = authority.rsplit('@').next()?.to_ascii_lowercase();
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']')?;
            (format!("[{host}]"), after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (authority.clone(), None),
        },
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let default_port = match scheme.as_str() {
        "https" => "443",
        "http" if is_local(&host) => "80",
        _ => return None,
    };
    match port.filter(|p| !p.is_empty() && *p != default_port) {
        Some(port) if port.chars().all(|c| c.is_ascii_digit()) => Some(format!("{scheme}://{host}:{port}")),
        Some(_) => None,
        None => Some(format!("{scheme}://{host}")),
    }
}

fn is_local(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// The site as the toast and the question name it: the host, without
/// "www.". "https://www.example.com:8443" -> "example.com:8443".
pub fn site_label(origin: &str) -> &str {
    let host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Text for an XML element or attribute.
pub fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no place for the other control characters.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast: the title, the text and the site, as Chrome shows them.
/// A notification without a title shows the site there instead.
pub fn toast_xml(title: &str, body: &str, site: &str, silent: bool) -> String {
    let title = if title.trim().is_empty() { site } else { title };
    let mut texts = format!("<text>{}</text>", escape_xml(title));
    if !body.trim().is_empty() {
        texts.push_str(&format!("<text>{}</text>", escape_xml(body)));
    }
    texts.push_str(&format!("<text placement=\"attribution\">{}</text>", escape_xml(site)));
    let audio = if silent { "<audio silent=\"true\"/>" } else { "" };
    format!("<toast><visual><binding template=\"ToastGeneric\">{texts}</binding></visual>{audio}</toast>")
}

/// The tag of a toast (Windows takes up to 64 characters): a notification
/// with the tag of an earlier one of the same site replaces it.
pub fn toast_tag(origin: &str, tag: &str) -> String {
    // FNV-1a, 64 bits: stable across runs and builds.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in origin.bytes().chain([0]).chain(tag.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ---------- Windows ----------

const AUMID_ROOT: &str = r"Software\Classes\AppUserModelId";

fn aumid_key(aumid: &str) -> String {
    format!(r"{AUMID_ROOT}\{aumid}")
}

fn icon_path() -> PathBuf {
    storage::local_root().join("nea.ico")
}

fn read_value(key: &str, name: &str) -> Option<String> {
    let mut buffer = [0u16; 512];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
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

fn write_value(key: &str, name: &str, value: &str) -> bool {
    let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            REG_SZ.0,
            Some(wide.as_ptr() as *const _),
            (wide.len() * 2) as u32,
        )
    };
    status == ERROR_SUCCESS
}

/// Makes `aumid` known to Windows, with the name and the icon its toasts
/// show. Only what differs is written: at most a file and two values,
/// once.
pub fn register(aumid: &str, name: &str) -> bool {
    let icon = icon_path();
    let bytes: &[u8] = include_bytes!("../assets/logo/nea.ico");
    if std::fs::metadata(&icon).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        if let Some(dir) = icon.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&icon, bytes);
    }
    let key = aumid_key(aumid);
    let icon = icon.display().to_string();
    let mut ok = true;
    for (value_name, value) in [("DisplayName", name), ("IconUri", icon.as_str())] {
        if read_value(&key, value_name).as_deref() != Some(value) {
            ok &= write_value(&key, value_name, value);
        }
    }
    ok
}

/// Takes Nea's application ID out of the registry (the uninstall).
pub fn unregister(aumid: &str) {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(aumid_key(aumid)));
    }
}

/// What shows Nea's toasts.
pub struct Toasts {
    notifier: ToastNotifier,
    aumid: HSTRING,
}

/// The parts of one toast.
pub struct Toast<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub site: &'a str,
    pub silent: bool,
    /// See `toast_tag`.
    pub tag: &'a str,
    /// The site's toasts go together in the notification center.
    pub group: &'a str,
    /// Straight to the notification center, without the pop-up.
    pub quiet: bool,
}

impl Toasts {
    /// Registers `aumid` (see `register`) and gets its notifier.
    pub fn new(aumid: &str) -> Option<Self> {
        register(aumid, storage::DISPLAY_NAME);
        let aumid = HSTRING::from(aumid);
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&aumid).ok()?;
        Some(Self { notifier, aumid })
    }

    /// Shows a toast; its click and its closing come back as `Outcome`s
    /// under `key`.
    pub fn show(&self, toast: &Toast<'_>, key: u64) -> windows::core::Result<ToastNotification> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(toast_xml(toast.title, toast.body, toast.site, toast.silent)))?;
        let notification = ToastNotification::CreateToastNotification(&xml)?;
        notification.SetTag(&HSTRING::from(toast.tag))?;
        notification.SetGroup(&HSTRING::from(toast.group))?;
        if toast.quiet {
            notification.SetSuppressPopup(true)?;
        }
        notification.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, _| {
            report(Outcome::Clicked(key));
            Ok(())
        }))?;
        notification.Dismissed(&TypedEventHandler::<ToastNotification, ToastDismissedEventArgs>::new(
            move |_, args| {
                // A toast that timed out went to the notification center:
                // it can still be clicked there.
                let reason = args.as_ref().and_then(|a| a.Reason().ok());
                if reason != Some(ToastDismissalReason::TimedOut) {
                    report(Outcome::Closed(key));
                }
                Ok(())
            },
        ))?;
        notification.Failed(&TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(move |_, _| {
            report(Outcome::Closed(key));
            Ok(())
        }))?;
        self.notifier.Show(&notification)?;
        Ok(notification)
    }

    /// The page closed its notification.
    pub fn hide(&self, toast: &ToastNotification) {
        let _ = self.notifier.Hide(toast);
    }

    /// Takes Nea's toasts out of the notification center: a click on one
    /// after Nea closed would find no page to go to.
    pub fn clear(&self) {
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.ClearWithId(&self.aumid);
        }
    }

    /// How many of Nea's toasts the notification center holds.
    #[cfg(all(test, nea_tests))]
    pub fn in_center(&self) -> Option<u32> {
        let history = ToastNotificationManager::History().ok()?;
        history.GetHistoryWithId(&self.aumid).ok()?.Size().ok()
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/notifications.rs"]
#[rustfmt::skip]
mod tests;
