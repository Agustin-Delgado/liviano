//! The engine side of saved passwords: the script every document starts
//! with (vault.js) and the messages it sends, queued for the app like every
//! other engine event. The origin of a message is the one the engine says
//! sent it, never what the page claims.

use serde::Deserialize;
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{HSTRING, Interface, PWSTR, Result};

use crate::{
    vault::{self, Secret},
    web::{self, Event},
};

const SCRIPT: &str = include_str!("vault.js");

#[derive(Debug)]
pub enum Message {
    /// A login form was sent with these values.
    Submit { origin: String, username: String, password: Secret },
    /// A document finished loading (or a login's password field went away).
    Page { origin: String, has_password: bool },
    /// The person clicked a login field: its box in physical pixels from
    /// the view's corner, and what it holds (empty for a password field).
    Field { origin: String, rect: [i32; 4], typed: String },
    /// A key pressed in that field while the account list shows.
    Key(MenuKey),
    /// The field's text changed while the list shows.
    Typed(String),
    /// The list should go: the field lost focus, the page scrolled, a click
    /// landed elsewhere.
    Dismiss,
}

/// The keys the page hands to the account list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKey {
    Down,
    Up,
    Enter,
    Escape,
}

impl MenuKey {
    fn of(key: &str) -> Option<Self> {
        Some(match key {
            "ArrowDown" => Self::Down,
            "ArrowUp" => Self::Up,
            "Enter" => Self::Enter,
            "Escape" => Self::Escape,
            _ => return None,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    nea: String,
    kind: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: Secret,
    #[serde(default)]
    has_password: bool,
    #[serde(default)]
    left: f64,
    #[serde(default)]
    top: f64,
    #[serde(default)]
    right: f64,
    #[serde(default)]
    bottom: f64,
    #[serde(default)]
    value: String,
    #[serde(default)]
    key: String,
}

/// Reads one web message. None for anything that isn't the vault script's
/// (other scripts of the app share the channel) or comes from a page with no
/// origin to save under.
pub fn parse(json: &str, source: &str) -> Option<Message> {
    let raw: Raw = serde_json::from_str(json).ok()?;
    if raw.nea != "vault" {
        return None;
    }
    let origin = vault::origin_of(source)?;
    let clamp = |v: f64| v.clamp(-100_000.0, 100_000.0) as i32;
    Some(match raw.kind.as_str() {
        "submit" if !raw.password.is_empty() => {
            Message::Submit { origin, username: raw.username, password: raw.password }
        }
        "page" => Message::Page { origin, has_password: raw.has_password },
        "field" => Message::Field {
            origin,
            rect: [clamp(raw.left), clamp(raw.top), clamp(raw.right), clamp(raw.bottom)],
            typed: raw.value,
        },
        "key" => Message::Key(MenuKey::of(&raw.key)?),
        "typed" => Message::Typed(raw.value),
        "dismiss" => Message::Dismiss,
        _ => return None,
    })
}

/// Starts the script in every document of the view and forwards its
/// messages. Not called for private tabs: they neither offer nor fill.
pub fn watch(webview: &ICoreWebView2, tab_id: u64) -> Result<()> {
    let mut token = 0i64;
    unsafe {
        // The engine's own password manager (off by default) would keep them
        // in the profile folder: passwords live in Credential Manager only.
        if let Ok(settings) = webview.Settings().and_then(|s| s.cast::<ICoreWebView2Settings4>()) {
            let _ = settings.SetIsPasswordAutosaveEnabled(false);
        }
        webview.AddScriptToExecuteOnDocumentCreated(
            &HSTRING::from(SCRIPT),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
        )?;
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut json = PWSTR::null();
                let mut source = PWSTR::null();
                if args.WebMessageAsJson(&mut json).is_err() || args.Source(&mut source).is_err() {
                    return Ok(());
                }
                let source = take_pwstr(source);
                // The JSON may hold a password: it is read once and wiped.
                let json = Secret::new(take_pwstr(json));
                if let Some(message) = parse(json.expose(), &source) {
                    web::push(Event::Vault(tab_id, message));
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Fills the login form of the page with the account the person chose.
/// The script checks the page's origin again before touching anything.
pub fn fill(webview: &ICoreWebView2, origin: &str, username: &str, password: &Secret) {
    let quote = |text: &str| serde_json::to_string(text).unwrap_or_default();
    let script = Secret::new(format!(
        "window.__neaVaultFill && window.__neaVaultFill({}, {}, {})",
        quote(origin),
        quote(username),
        quote(password.expose())
    ));
    unsafe {
        let _ = webview.ExecuteScript(
            &HSTRING::from(script.expose()),
            &ExecuteScriptCompletedHandler::create(Box::new(|_, result| {
                crate::diag::trace(|| format!("vault: fill done {result}"));
                Ok(())
            })),
        );
    }
}

/// The script call that tells the page whether the account list shows (and
/// with a row picked), so that it hands the list's keys to the app.
pub fn menu_script(open: bool, picked: bool) -> String {
    let state = match (open, picked) {
        (false, _) => 0,
        (true, false) => 1,
        (true, true) => 2,
    };
    format!("window.__neaVaultMenu && window.__neaVaultMenu({state})")
}

pub fn set_menu(webview: &ICoreWebView2, open: bool, picked: bool) {
    unsafe {
        let _ = webview.ExecuteScript(
            &HSTRING::from(menu_script(open, picked)),
            &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))),
        );
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/vault_watch.rs"]
#[rustfmt::skip]
mod tests;
