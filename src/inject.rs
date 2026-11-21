//! Scripts the app injects into pages (reader, curtain, floating video):
//! the JavaScript lives in `assets/` and is embedded in the executable, so
//! nothing is read from disk and nothing is loaded until a feature is used.

use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, ExecuteScriptCompletedHandler,
    Microsoft::Web::WebView2::Win32::ICoreWebView2,
};
use windows::core::HSTRING;

pub const TOAST: &str = include_str!("../assets/toast.js");

/// Fills `/*NAME*/` placeholders in order, so a large filler goes last and
/// is never searched. Debug builds (tests included, which build every
/// script) catch a template and its fillers drifting apart.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut script = template.to_string();
    for (name, value) in values {
        let marker = format!("/*{name}*/");
        debug_assert!(script.contains(&marker), "missing placeholder {marker}");
        script = script.replace(&marker, value);
    }
    script
}

/// A JavaScript string literal (JSON is valid JavaScript).
pub fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

/// Runs a script in the page; the result is not needed.
pub fn execute(webview: &ICoreWebView2, script: &str) {
    let handler = ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(())));
    unsafe {
        let _ = webview.ExecuteScript(&HSTRING::from(script), &handler);
    }
}

/// Runs a script without handing the page a user gesture. `ExecuteScript`
/// does hand one (measured: a `window.open` the page makes right after it
/// reaches the app as initiated by the user), so anything the app runs on
/// its own — not because the person just asked for it — goes through here,
/// or the popup blocker would let the page's next window through.
pub fn execute_without_gesture(webview: &ICoreWebView2, script: &str) {
    let params = serde_json::json!({ "expression": script, "userGesture": false });
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
    unsafe {
        let _ = webview.CallDevToolsProtocolMethod(
            &HSTRING::from("Runtime.evaluate"),
            &HSTRING::from(params.to_string()),
            &handler,
        );
    }
}

/// Runs a script as if the person had just interacted with the page (DevTools
/// `Runtime.evaluate` with `userGesture`): some APIs, like Picture-in-Picture,
/// refuse a plain `ExecuteScript`, and a key handled by the app never reaches
/// the page as a gesture.
pub fn execute_with_gesture(webview: &ICoreWebView2, script: &str) {
    let params = serde_json::json!({
        "expression": script,
        "userGesture": true,
        "awaitPromise": true,
        "returnByValue": true,
    });
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
    unsafe {
        let _ = webview.CallDevToolsProtocolMethod(
            &HSTRING::from("Runtime.evaluate"),
            &HSTRING::from(params.to_string()),
            &handler,
        );
    }
}

/// A short note at the bottom of the page.
pub fn toast_script(text: &str) -> String {
    format!("(() => {{ {TOAST} neaToast({}); }})();", js_string(text))
}
