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
    /// The person clicked a login field; physical pixels from the view's corner.
    Field { origin: String, left: i32, bottom: i32 },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    liviano: String,
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
    bottom: f64,
}

/// Reads one web message. None for anything that isn't the vault script's
/// (other scripts of the app share the channel) or comes from a page with no
/// origin to save under.
pub fn parse(json: &str, source: &str) -> Option<Message> {
    let raw: Raw = serde_json::from_str(json).ok()?;
    if raw.liviano != "vault" {
        return None;
    }
    let origin = vault::origin_of(source)?;
    let clamp = |v: f64| v.clamp(-100_000.0, 100_000.0) as i32;
    Some(match raw.kind.as_str() {
        "submit" if !raw.password.is_empty() => Message::Submit { origin, username: raw.username, password: raw.password },
        "page" => Message::Page { origin, has_password: raw.has_password },
        "field" => Message::Field { origin, left: clamp(raw.left), bottom: clamp(raw.bottom) },
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
        "window.__livianoVaultFill && window.__livianoVaultFill({}, {}, {})",
        quote(origin),
        quote(username),
        quote(password.expose())
    ));
    unsafe {
        let _ = webview.ExecuteScript(
            &HSTRING::from(script.expose()),
            &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))),
        );
    }
}

