//! Whether the keyboard of a page is in a place that types: a text field,
//! a text area, an editable element or an editor that types through a
//! hidden text area (Monaco, Google Docs). A chord that also moves the
//! caret (Ctrl+Left: a word back) goes to the page there, not to Nea.
//!
//! A small script in the top document tells the app on every focus change.
//! The keys come to the app before the page sees them
//! (`AcceleratorKeyPressed`), so the answer has to be known already: the
//! flag is kept beside the view and read when a key comes.
//!
//! What a frame holds can't be read from the top document (another site's
//! frame keeps its own focus): focus in a frame counts as a text field, so
//! a word move in it is never taken. A page can post the message itself;
//! the most it gets is its own Ctrl+Left back or not.

use std::{cell::Cell, rc::Rc};

use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2,
    WebMessageReceivedEventHandler, take_pwstr,
};
use windows::core::{HSTRING, PWSTR, Result};

pub const SCRIPT: &str = r#"(() => {
  if (window.top !== window || !window.chrome || !chrome.webview) return;
  const NOT_TEXT = new Set(['button', 'checkbox', 'color', 'file', 'hidden', 'image', 'radio', 'range', 'reset', 'submit']);
  const TEXT_ROLES = new Set(['textbox', 'searchbox', 'combobox']);
  const typing = () => {
    if (document.designMode === 'on') return true;
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    if (!el || el === document.body || el === document.documentElement) return false;
    const tag = el.tagName;
    if (tag === 'TEXTAREA') return true;
    if (tag === 'INPUT') return !NOT_TEXT.has(String(el.type).toLowerCase());
    if (tag === 'IFRAME' || tag === 'FRAME' || tag === 'OBJECT' || tag === 'EMBED') return true;
    if (el.isContentEditable) return true;
    return TEXT_ROLES.has(String(el.getAttribute('role')).toLowerCase());
  };
  let last = null;
  const tell = (now) => {
    if (now === last) return;
    last = now;
    chrome.webview.postMessage({ nea: 'text-focus', typing: now });
  };
  const report = () => tell(typing());
  // focusin: an element; focus and blur: the window, as a field that got
  // its focus while the window was in the back only says so then.
  addEventListener('focusin', report, true);
  addEventListener('focus', report, true);
  addEventListener('focusout', () => setTimeout(report, 0), true);
  addEventListener('blur', () => setTimeout(report, 0), true);
  addEventListener('pageshow', report, true);
  addEventListener('pagehide', () => tell(false), true);
  report();
})();"#;

/// The script's message: Some(whether the focus types), None for any
/// other message.
pub fn parse(json: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value.get("nea")?.as_str()? != "text-focus" {
        return None;
    }
    value.get("typing")?.as_bool()
}

/// Starts the script in the view and keeps `typing` up to date.
pub fn watch(webview: &ICoreWebView2, typing: Rc<Cell<bool>>) -> Result<()> {
    let mut token = 0i64;
    unsafe {
        webview.AddScriptToExecuteOnDocumentCreated(
            &HSTRING::from(SCRIPT),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
        )?;
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut json = PWSTR::null();
                if args.WebMessageAsJson(&mut json).is_err() {
                    return Ok(());
                }
                if let Some(now) = parse(&take_pwstr(json)) {
                    typing.set(now);
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/text_focus.rs"]
#[rustfmt::skip]
mod tests;
