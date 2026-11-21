//! The window's side of script control: each request the pipe thread
//! queued runs here, on the UI thread, against the real app — the same
//! commands, the same field, the same tabs. Answers that need the engine
//! (page text, a click, a capture) are sent from its completion callbacks.
//! Nothing here sends keyboard or mouse input to the system: the field gets
//! its characters as messages to our own edit control.

use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CapturePreviewCompletedHandler, ExecuteScriptCompletedHandler,
    Microsoft::Web::WebView2::Win32::{COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, ICoreWebView2},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        System::Com::{STGM_CREATE, STGM_SHARE_DENY_WRITE, STGM_WRITE},
        UI::{
            Input::KeyboardAndMouse::VK_RETURN,
            Shell::SHCreateStreamOnFileEx,
            WindowsAndMessaging::{
                KillTimer, PostMessageW, SendMessageW, SetTimer, SetWindowTextW, WM_CHAR, WM_CLOSE, WM_KEYDOWN,
            },
        },
    },
    core::HSTRING,
};

use super::{App, Mode, with_app};
use crate::{
    bench::{
        self, Job, Reply,
        protocol::{self, Action},
    },
    omnibox,
    panels::PanelKind,
    shortcuts,
    tabs::Live,
    web::{Event, WebTab},
};

/// Checks waits against their deadline while no engine event comes.
const TIMER_WAITS: usize = 0x40;

struct Waiter {
    tab: u64,
    deadline: Instant,
    reply: Reply,
}

#[derive(Default)]
pub(super) struct BenchState {
    /// Tabs whose last navigation finished, and whether it succeeded.
    loaded: HashMap<u64, bool>,
    waiters: Vec<Waiter>,
    /// Background tabs to wake once the engine is up.
    pending_wake: Vec<u64>,
}

impl BenchState {
    /// Follows loads as the engine reports them, before the app handles the event.
    pub(super) fn observe(&mut self, event: &Event) {
        match event {
            Event::NavigationStarting(id) => {
                self.loaded.remove(id);
            }
            Event::NavigationCompleted(id, ok) => {
                self.loaded.insert(*id, *ok);
            }
            Event::ProcessFailed(id, _) => {
                self.loaded.remove(id);
            }
            _ => {}
        }
    }
}

/// Runs what the pipe queued. Called on the bench message, outside the app
/// borrow, because typing into the field calls back into the app.
pub(super) fn drain() {
    while let Some(job) = bench::pop() {
        if matches!(job.action, Action::Type { .. }) {
            if let Err(job) = type_into_field(job) {
                requeue(job);
                return;
            }
            continue;
        }
        let mut slot = Some(job);
        with_app(|app| {
            if let Some(job) = slot.take() {
                app.bench_job(job);
            }
        });
        if let Some(job) = slot {
            requeue(job);
            return;
        }
    }
}

/// The app was busy: the job goes back first and the message comes again
/// once it may be free.
fn requeue(job: Job) {
    bench::push_front(job);
    super::retry_later(bench::window(), bench::WM_APP_BENCH, WPARAM(0), LPARAM(0));
}

unsafe extern "system" fn on_wait_timer(_: HWND, _: u32, _: usize, _: u32) {
    with_app(|app| app.bench_poll());
}

/// Types `text` into the open field one character at a time, as WM_CHAR to
/// our own edit control, so suggestions and inline completion react as they
/// do to a person; `submit` then presses Enter on it the same way.
#[allow(
    clippy::result_large_err,
    reason = "the job goes back to its queue as it came; boxing it buys nothing on this cold path"
)]
fn type_into_field(job: Job) -> Result<(), Job> {
    let Action::Type { text, submit } = &job.action else { return Ok(()) };
    let edit = match with_app(|app| app.mode.is_some().then_some(app.omnibar.edit)) {
        None => return Err(job),
        Some(None) => {
            job.reply.error("el campo no está abierto: abrilo antes con run focus_address, new_tab, switch_tab o find");
            return Ok(());
        }
        Some(Some(edit)) => edit,
    };
    unsafe {
        let _ = SetWindowTextW(edit, &HSTRING::new());
        for unit in text.encode_utf16() {
            SendMessageW(edit, WM_CHAR, Some(WPARAM(unit as usize)), Some(LPARAM(1)));
        }
        if *submit {
            SendMessageW(edit, WM_KEYDOWN, Some(WPARAM(VK_RETURN.0 as usize)), Some(LPARAM(1)));
        }
    }
    let state = with_app(|app| json!({ "field": app.field_state() }));
    job.reply.ok(state.unwrap_or(Value::Null));
    Ok(())
}

fn live_name<W>(live: &Live<W>, suspended: impl Fn(&W) -> bool) -> &'static str {
    match live {
        Live::Asleep => "asleep",
        Live::Creating => "creating",
        Live::Ready(web) if suspended(web) => "suspended",
        Live::Ready(_) => "ready",
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Address => "address",
        Mode::NewTab => "new_tab",
        Mode::Switch => "switch",
        Mode::Find => "find",
        Mode::NewPrivateTab => "new_private_tab",
        Mode::Split => "split",
        Mode::RenameGroup(_) => "rename_group",
        Mode::Panel(PanelKind::Bookmarks) => "bookmarks",
        Mode::Panel(PanelKind::History) => "history",
        Mode::Panel(PanelKind::Downloads) => "downloads",
        Mode::Panel(PanelKind::Passwords) => "passwords",
    }
}

fn screenshot_path(tab: u64, asked: Option<&str>) -> std::path::PathBuf {
    if let Some(path) = asked.filter(|p| !p.trim().is_empty()) {
        return std::path::PathBuf::from(path);
    }
    let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    std::env::temp_dir().join("nea-bench").join(format!("tab-{tab}-{millis}.png"))
}

impl App {
    fn bench_job(&mut self, job: Job) {
        let Job { action, reply } = job;
        match action {
            Action::Open { url, background } => {
                let url = omnibox::resolve(&url, &self.settings.search_url).unwrap_or(url);
                let id = self.tabs.insert(&url, "", false, None);
                self.tabs.mark_test(id);
                if background {
                    self.bench.pending_wake.push(id);
                    self.bench_poll();
                    self.relayout_soft();
                } else {
                    // As when a person opens one: the field closes, the page has focus.
                    if self.mode.is_some() {
                        self.close_omnibar();
                    }
                    self.tabs.select(id);
                    self.relayout();
                    self.focus_page();
                }
                reply.ok(json!({ "tab": id }));
            }
            Action::Wait { tab, timeout_ms } => {
                let id = match self.bench_tab(tab) {
                    Ok(id) => id,
                    Err(message) => return reply.error(&message),
                };
                let timeout = Duration::from_millis(timeout_ms.unwrap_or(protocol::DEFAULT_WAIT_MS));
                self.bench.waiters.push(Waiter { tab: id, deadline: Instant::now() + timeout, reply });
                self.bench_poll();
            }
            Action::Text { tab } => {
                let webview = match self.bench_web(tab) {
                    Ok(webview) => webview,
                    Err(message) => return reply.error(&message),
                };
                let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
                    match result.map(|_| serde_json::from_str::<String>(&json)) {
                        Ok(Ok(text)) => reply.ok(json!({ "text": text })),
                        Ok(Err(_)) => reply.ok(json!({ "text": "" })),
                        Err(e) => reply.error(&format!("no se pudo leer la página: {e}")),
                    }
                    Ok(())
                }));
                // On failure the handler, and the reply in it, is dropped: the
                // pipe thread then reports the missing answer.
                unsafe {
                    let _ = webview.ExecuteScript(&HSTRING::from(protocol::TEXT_SCRIPT), &handler);
                }
            }
            Action::Click { tab, selector, gesture } => {
                let webview = match self.bench_web(tab) {
                    Ok(webview) => webview,
                    Err(message) => return reply.error(&message),
                };
                let shown = selector.clone();
                if gesture {
                    let params = json!({
                        "expression": protocol::click_script(&selector),
                        "userGesture": true,
                        "returnByValue": true,
                    });
                    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                        let value: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
                        let value = &value["result"]["value"];
                        match (result, value.get("found").and_then(Value::as_bool)) {
                            (Err(e), _) => reply.error(&format!("no se pudo ejecutar el clic: {e}")),
                            (_, Some(true)) => reply.ok(json!({ "clicked": shown })),
                            _ => reply.error(&format!("ningún elemento coincide con «{shown}»")),
                        }
                        Ok(())
                    }));
                    unsafe {
                        let _ = webview.CallDevToolsProtocolMethod(
                            &HSTRING::from("Runtime.evaluate"),
                            &HSTRING::from(params.to_string()),
                            &handler,
                        );
                    }
                    return;
                }
                let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
                    let value: Value = serde_json::from_str(&json).unwrap_or(Value::Null);
                    match (result, value.get("found").and_then(Value::as_bool), value.get("error")) {
                        (Err(e), _, _) => reply.error(&format!("no se pudo ejecutar el clic: {e}")),
                        (_, _, Some(error)) => reply.error(&format!("selector inválido «{shown}»: {error}")),
                        (_, Some(true), _) => reply.ok(json!({ "clicked": shown })),
                        _ => reply.error(&format!("ningún elemento coincide con «{shown}»")),
                    }
                    Ok(())
                }));
                unsafe {
                    let _ = webview.ExecuteScript(&HSTRING::from(protocol::click_script(&selector)), &handler);
                }
            }
            Action::Screenshot { tab, path } => self.bench_screenshot(tab, path, reply),
            Action::Run { name, keys } => {
                let command = match (&name, &keys) {
                    (Some(name), _) => shortcuts::all_commands().into_iter().find(|c| &c.name() == name),
                    (None, Some(keys)) => shortcuts::parse_chord(keys).and_then(|chord| self.shortcuts.lookup(chord)),
                    (None, None) => None,
                };
                let Some(command) = command else {
                    let asked = name.or(keys).unwrap_or_default();
                    return reply.error(&format!("no hay ningún comando «{asked}»"));
                };
                // Answered first: a command may open a menu, whose loop
                // would hold the answer until it closes.
                reply.ok(json!({ "command": command.name() }));
                self.run(command);
            }
            Action::State => reply.ok(self.bench_state()),
            Action::Hover { x, y } => {
                let hit = self.layout.hit(x, y);
                self.set_hover(hit);
                reply.ok(json!({ "hit": hit.map(|h| format!("{h:?}")) }));
            }
            Action::Shield => {
                let stats = crate::shield::stats();
                reply.ok(json!({
                    "requests": stats.requests,
                    "blocked": stats.blocked,
                    "rules": stats.rules,
                    "heap_bytes": stats.heap_bytes,
                    "popups_blocked": self.popups.blocked,
                    "popups_recent": self.popups.recent,
                    "cookie_rules": stats.cosmetic_rules,
                    "cookie_heap_bytes": stats.cosmetic_heap_bytes,
                }));
            }
            Action::Discard => {
                let background: Vec<u64> = self
                    .tabs
                    .tabs
                    .iter()
                    .filter(|t| t.test && Some(t.id) != self.tabs.active && matches!(t.live, Live::Ready(_)))
                    .map(|t| t.id)
                    .collect();
                for id in &background {
                    self.discard(*id);
                }
                self.invalidate();
                reply.ok(json!({ "discarded": background.len() }));
            }
            Action::Close { tab } => {
                let ids: Vec<u64> = match tab {
                    Some(id) => match self.tabs.get(id) {
                        Some(t) if t.test => vec![id],
                        Some(_) => return reply.error("sólo se cierran pestañas de prueba"),
                        None => return reply.error(&format!("no hay ninguna pestaña {id}")),
                    },
                    None => self.tabs.tabs.iter().filter(|t| t.test).map(|t| t.id).collect(),
                };
                for id in &ids {
                    self.bench.loaded.remove(id);
                    self.close_tab(*id);
                }
                reply.ok(json!({ "closed": ids.len() }));
            }
            Action::Quit => {
                reply.ok(json!({ "closing": true }));
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            Action::Group { tabs, group, name, color, collapsed } => {
                match self.bench_group(&tabs, group, name, color, collapsed) {
                    Ok(id) => reply.ok(json!({ "group": id, "groups": self.groups_state() })),
                    Err(message) => reply.error(&message),
                }
            }
            // Answered on the pipe thread; never queued.
            Action::Ping | Action::Memory | Action::Type { .. } => reply.error("petición fuera de lugar"),
        }
    }

    /// The tab a request names, or the active one.
    fn bench_tab(&self, tab: Option<u64>) -> Result<u64, String> {
        match tab.or(self.tabs.active) {
            Some(id) if self.tabs.get(id).is_some() => Ok(id),
            Some(id) => Err(format!("no hay ninguna pestaña {id}")),
            None => Err("no hay ninguna pestaña activa".to_string()),
        }
    }

    /// The live view of the tab a request names.
    fn bench_web(&self, tab: Option<u64>) -> Result<ICoreWebView2, String> {
        let id = self.bench_tab(tab)?;
        match self.tabs.get(id).map(|t| &t.live) {
            Some(Live::Ready(web)) => Ok(web.webview.clone()),
            _ => Err(format!("la pestaña {id} no tiene la página cargada (está dormida o creándose)")),
        }
    }

    fn bench_screenshot(&mut self, tab: Option<u64>, path: Option<String>, reply: Reply) {
        let id = match self.bench_tab(tab) {
            Ok(id) => id,
            Err(message) => return reply.error(&message),
        };
        // A hidden view draws nothing: the tab is shown first.
        if self.tabs.active != Some(id) {
            self.select(id);
        }
        let webview = match self.bench_web(Some(id)) {
            Ok(webview) => webview,
            Err(message) => return reply.error(&message),
        };
        let path = screenshot_path(id, path.as_deref());
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let stream = unsafe {
            SHCreateStreamOnFileEx(
                &HSTRING::from(path.as_os_str()),
                (STGM_CREATE | STGM_WRITE | STGM_SHARE_DENY_WRITE).0,
                FILE_ATTRIBUTE_NORMAL.0,
                true,
                None,
            )
        };
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => return reply.error(&format!("no se pudo crear {}: {e}", path.display())),
        };
        let held = stream.clone();
        let shown = path.display().to_string();
        let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
            // Releasing the stream closes the file before it is answered.
            drop(held);
            match result {
                Ok(()) => {
                    let bytes = std::fs::metadata(&shown).map_or(0, |m| m.len());
                    reply.ok(json!({ "path": shown, "bytes": bytes }));
                }
                Err(e) => reply.error(&format!("no se pudo capturar la pestaña: {e}")),
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler);
        }
    }

    /// Wakes background test tabs once the engine exists and answers waits
    /// that are done, gone or out of time. Runs after every batch of engine
    /// events and on a short timer while a wait is open.
    pub(super) fn bench_poll(&mut self) {
        if self.env.is_some() {
            for id in std::mem::take(&mut self.bench.pending_wake) {
                self.wake(id);
            }
        } else if !self.bench.pending_wake.is_empty() {
            self.request_environment();
        }
        if self.bench.waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        for waiter in std::mem::take(&mut self.bench.waiters) {
            let Some(tab) = self.tabs.get(waiter.tab) else {
                waiter.reply.error(&format!("la pestaña {} se cerró", waiter.tab));
                continue;
            };
            match self.bench.loaded.get(&waiter.tab) {
                Some(ok) if matches!(tab.live, Live::Ready(_)) => {
                    waiter.reply.ok(json!({ "tab": tab.id, "ok": ok, "url": tab.url, "title": tab.title }));
                }
                _ if now >= waiter.deadline => {
                    waiter.reply.error(&format!("la pestaña {} no terminó de cargar a tiempo", waiter.tab));
                }
                _ => self.bench.waiters.push(waiter),
            }
        }
        unsafe {
            if self.bench.waiters.is_empty() {
                let _ = KillTimer(Some(self.hwnd), TIMER_WAITS);
            } else {
                SetTimer(Some(self.hwnd), TIMER_WAITS, 200, Some(on_wait_timer));
            }
        }
    }

    /// The `group` request: made or changed through the same calls as the menus.
    fn bench_group(
        &mut self,
        tabs: &[u64],
        group: Option<u64>,
        name: Option<String>,
        color: Option<String>,
        collapsed: Option<bool>,
    ) -> Result<u64, String> {
        if let Some(missing) = tabs.iter().find(|id| self.tabs.get(**id).is_none()) {
            return Err(format!("no hay ninguna pestaña {missing}"));
        }
        let id = match group {
            Some(id) if self.tabs.group(id).is_some() => id,
            Some(id) => return Err(format!("no hay ningún grupo {id}")),
            None => {
                let first = *tabs.first().ok_or("hacen falta pestañas para un grupo nuevo")?;
                self.tabs.create_group(first).ok_or("una pestaña fijada no va en un grupo")?
            }
        };
        for &tab in tabs {
            if self.tabs.group_of(tab) != Some(id) {
                self.tabs.add_to_group(tab, id);
            }
        }
        if let Some(name) = name {
            self.tabs.rename_group(id, &name);
        }
        if let Some(color) = color {
            let color =
                crate::tab_groups::GroupColor::from_name(&color).ok_or(format!("no hay ningún color «{color}»"))?;
            self.tabs.set_group_color(id, color);
        }
        self.session_dirty = true;
        match collapsed {
            Some(folded) if self.tabs.group(id).is_some_and(|g| g.collapsed != folded) => self.toggle_group(id),
            _ => self.relayout(),
        }
        Ok(id)
    }

    /// The groups and where their labels are, for scripts that click them.
    fn groups_state(&self) -> Vec<Value> {
        self.group_infos()
            .iter()
            .map(|g| {
                let label = self.layout.labels.iter().find(|l| l.id == g.id).map(|l| l.rect);
                json!({
                    "id": g.id,
                    "name": g.name,
                    "color": g.color.name(),
                    "collapsed": g.collapsed,
                    "tabs": self.tabs.members(g.id),
                    "label": label.map(|r| [r.left, r.top, r.right, r.bottom]),
                })
            })
            .collect()
    }

    fn field_state(&self) -> Value {
        match self.mode {
            None => json!({ "open": false }),
            Some(mode) => json!({
                "open": true,
                "mode": mode_name(mode),
                "text": self.omnibar.text(),
                "rows": self.omnibar.row_labels(),
                "selected_row": self.omnibar.selected_row(),
            }),
        }
    }

    fn bench_state(&self) -> Value {
        let tabs: Vec<Value> = self
            .tabs
            .tabs
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "url": t.url,
                    "title": t.title,
                    "label": t.label(),
                    "pinned": t.pinned,
                    "private": t.private,
                    "group": t.group,
                    "rect": self.layout.items.iter().find(|i| i.id == t.id).map(|i| [i.rect.left, i.rect.top, i.rect.right, i.rect.bottom]),
                    "test": t.test,
                    "loading": t.loading,
                    "loaded": self.bench.loaded.get(&t.id).copied(),
                    "live": live_name(&t.live, |w: &WebTab| w.suspended),
                })
            })
            .collect();
        json!({
            "tabs": tabs,
            "active": self.tabs.active,
            "layout": if self.vertical { "vertical" } else { "horizontal" },
            "sidebar_collapsed": self.collapsed,
            "splits": self.split_pairs().iter().map(|(l, r)| json!([l, r])).collect::<Vec<_>>(),
            "groups": self.groups_state(),
            "extensions_button": self.layout.extensions.width() > 0,
            "card": self.card_state(),
            "tooltip": self.tip_state(),
            "anim": crate::app::tooltip_ui::anim_state(),
            "field": self.field_state(),
            "engine": if self.env.is_some() { "ready" } else if self.env_requested { "starting" } else { "off" },
        })
    }
}
