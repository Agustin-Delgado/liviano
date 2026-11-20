//! The script control protocol: one JSON object per line each way. Pure, so
//! the parsing and the replies are tested without a pipe or a window. The
//! example client (`examples/bench.rs`) includes this file too, so both ends
//! speak exactly the same shapes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a script can ask for. `tab` left out means the active tab.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Ping,
    /// Opens a test tab: never in history, session or the reopen list.
    Open {
        url: String,
        /// Load it without selecting it.
        #[serde(default)]
        background: bool,
    },
    /// Waits until the tab's page has finished loading.
    Wait { tab: Option<u64>, timeout_ms: Option<u64> },
    /// `document.body.innerText`.
    Text { tab: Option<u64> },
    /// A PNG of the tab's view, written to `path` (or a temporary file).
    Screenshot { tab: Option<u64>, path: Option<String> },
    /// Clicks the first element that matches a CSS selector.
    Click { tab: Option<u64>, selector: String },
    /// Runs an app command by its settings name ("new_tab") or by the chord
    /// bound to it ("Ctrl+T"), without any keyboard input.
    Run { name: Option<String>, keys: Option<String> },
    /// Replaces the text of the single field (it has to be open) and,
    /// with `submit`, presses Enter on it.
    Type {
        text: String,
        #[serde(default)]
        submit: bool,
    },
    /// Tabs, active tab, layout and the field's mode.
    State,
    /// Private working set of the app and of the engine's process tree.
    Memory,
    /// Requests seen and blocked by the ad blocker, and the size of its
    /// compiled rules.
    Shield,
    /// Puts every background test tab to sleep (its view destroyed), as the
    /// sleep timer would; the person's own tabs are left alone.
    Discard,
    /// Closes one test tab, or all of them when `tab` is left out.
    Close { tab: Option<u64> },
    /// Closes the window as the person would (the session is saved).
    Quit,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Request {
    /// Echoed back so a client can match replies; any JSON value.
    #[serde(default)]
    pub id: Value,
    #[serde(flatten)]
    pub action: Action,
}

/// How long the pipe thread waits for the window to answer, beyond any
/// timeout the request carries itself.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_WAIT_MS: u64 = 15_000;

impl Request {
    pub fn parse(line: &str) -> Result<Request, String> {
        serde_json::from_str(line.trim()).map_err(|e| format!("petición inválida: {e}"))
    }

    /// The longest the reply may take.
    pub fn reply_timeout_ms(&self) -> u64 {
        match &self.action {
            Action::Wait { timeout_ms, .. } => timeout_ms.unwrap_or(DEFAULT_WAIT_MS) + 2_000,
            _ => DEFAULT_TIMEOUT_MS,
        }
    }
}

/// One reply line (without the line break).
pub fn ok(id: &Value, result: Value) -> String {
    json!({ "id": id, "ok": true, "result": result }).to_string()
}

pub fn error(id: &Value, message: &str) -> String {
    json!({ "id": id, "ok": false, "error": message }).to_string()
}

/// The pipe of the current user. Only letters, digits, '-' and '_' make it
/// into the name, so a user name with spaces or symbols is still one name.
pub fn pipe_name(user: &str, suffix: Option<&str>) -> String {
    let clean = |text: &str| -> String {
        text.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
    };
    match suffix.filter(|s| !s.trim().is_empty()) {
        Some(suffix) => format!(r"\\.\pipe\liviano-bench-{}-{}", clean(user), clean(suffix)),
        None => format!(r"\\.\pipe\liviano-bench-{}", clean(user)),
    }
}

/// The script that reads a page's text; its result is a JSON string.
pub const TEXT_SCRIPT: &str = "document.body ? document.body.innerText : ''";

/// The script that clicks `selector`. Its result is a JSON object:
/// `{"found":bool}` or `{"error":"..."}` for a selector the page rejects.
pub fn click_script(selector: &str) -> String {
    let selector = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(() => {{ try {{ const el = document.querySelector({selector}); if (!el) return {{ found: false }}; \
         el.scrollIntoView({{ block: 'center' }}); el.click(); return {{ found: true }}; }} \
         catch (e) {{ return {{ error: String(e && e.message || e) }}; }} }})()"
    )
}

