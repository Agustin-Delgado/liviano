//! Hiding elements per site. A picker in the page turns a click into
//! a CSS selector; the app keeps the selectors per host in curtain.json and
//! hands every view one script, run as each document is created, that hides
//! them before the page paints. The page talks back through `postMessage`,
//! which arrives as `Event::WebMessage` like any other engine callback.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2,
    WebMessageReceivedEventHandler, take_pwstr,
};
use windows::{
    Win32::Foundation::HWND,
    core::{HSTRING, PWSTR},
};

use crate::{
    inject,
    menu::{Anchor, HAlign, Menu, VAlign},
    omnibox, storage,
    web::{self, Event, WebTab},
};

const BOOT: &str = include_str!("../assets/curtain-boot.js");
const APPLY: &str = include_str!("../assets/curtain-apply.js");
const PICK: &str = include_str!("../assets/curtain-pick.js");

const MAX_SELECTOR: usize = 512;
const MAX_LABEL: usize = 80;
const MAX_RULES_PER_SITE: usize = 200;

/// Selectors that would blank the whole site rather than hide a part of it.
const TOO_BROAD: &[&str] = &["*", "html", "body", ":root", "head"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub selector: String,
    /// What the person saw ("div.cookie-banner — Aceptamos cookies…").
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// Host (lowercase, without "www.") -> rules, in the order they were made.
    pub sites: BTreeMap<String, Vec<Rule>>,
}

/// The key a page is stored under: its host, for web pages only.
pub fn host_of(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let host = omnibox::display_host(&lower).trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_string())
}

/// True when `c` appears in `text` without a backslash escaping it.
fn has_bare(text: &str, c: char) -> bool {
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == c {
            return true;
        }
    }
    false
}

/// A selector as the page sent it, or None when it can't be one rule's
/// selector: the stored text is pasted into `selector{display:none}`, so a
/// brace, a comment or a list (one click is one element) is refused, and so
/// is anything that would hide the whole page.
pub fn normalize_selector(raw: &str) -> Option<String> {
    let selector = raw.trim();
    if selector.is_empty() || selector.len() > MAX_SELECTOR {
        return None;
    }
    if selector.chars().any(char::is_control) || selector.contains("/*") {
        return None;
    }
    if ['{', '}', ',', ';'].into_iter().any(|c| has_bare(selector, c)) {
        return None;
    }
    if TOO_BROAD.contains(&selector.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(selector.to_string())
}

pub fn clean_label(raw: &str) -> String {
    let flat: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_LABEL {
        return flat;
    }
    let mut cut: String = flat.chars().take(MAX_LABEL - 1).collect();
    cut.push('…');
    cut
}

impl Store {
    pub fn rules(&self, host: &str) -> &[Rule] {
        self.sites.get(host).map_or(&[], Vec::as_slice)
    }

    /// Adds a rule; false when it is invalid, repeated or the site is full.
    pub fn hide(&mut self, host: &str, selector: &str, label: &str) -> bool {
        let Some(selector) = normalize_selector(selector) else { return false };
        let rules = self.sites.entry(host.to_string()).or_default();
        if rules.len() >= MAX_RULES_PER_SITE || rules.iter().any(|r| r.selector == selector) {
            return false;
        }
        rules.push(Rule { selector, label: clean_label(label) });
        true
    }

    pub fn restore(&mut self, host: &str, index: usize) -> bool {
        let Some(rules) = self.sites.get_mut(host) else { return false };
        if index >= rules.len() {
            return false;
        }
        rules.remove(index);
        if rules.is_empty() {
            self.sites.remove(host);
        }
        true
    }

    pub fn restore_all(&mut self, host: &str) -> bool {
        self.sites.remove(host).is_some()
    }

    fn selectors(&self, host: &str) -> Vec<&str> {
        self.rules(host).iter().map(|r| r.selector.as_str()).collect()
    }

    /// The document-created script, or None when nothing is hidden anywhere
    /// (then no script is registered at all).
    pub fn boot_script(&self) -> Option<String> {
        if self.sites.is_empty() {
            return None;
        }
        let sites: BTreeMap<&str, Vec<&str>> = self.sites.keys().map(|h| (h.as_str(), self.selectors(h))).collect();
        let json = serde_json::to_string(&sites).ok()?;
        Some(inject::fill(BOOT, &[("SITES", &json)]))
    }

    /// Re-applies the site's current set to a page already loaded.
    pub fn apply_script(&self, host: &str) -> String {
        let json = serde_json::to_string(&self.selectors(host)).unwrap_or_else(|_| "[]".to_string());
        inject::fill(APPLY, &[("SELECTORS", &json)])
    }
}

/// What the picker in the page says.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind")]
pub enum Message {
    #[serde(rename = "curtain.hide")]
    Hide {
        selector: String,
        #[serde(default)]
        label: String,
    },
    #[serde(rename = "curtain.done")]
    Done,
}

impl Message {
    /// None for anything else a page might post.
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

/// The document-created script registered on each view, by tab. A newer
/// registration may be requested before the engine answered the previous
/// one, so each answer checks it is still the latest.
struct Slot {
    generation: u64,
    id: Option<String>,
}

thread_local! {
    static SCRIPTS: RefCell<HashMap<u64, Slot>> = RefCell::new(HashMap::new());
}

fn register(webview: &ICoreWebView2, tab: u64, script: Option<&str>) {
    let generation = SCRIPTS.with(|scripts| {
        let mut scripts = scripts.borrow_mut();
        let slot = scripts.entry(tab).or_insert(Slot { generation: 0, id: None });
        slot.generation += 1;
        if let Some(old) = slot.id.take() {
            unsafe {
                let _ = webview.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(old));
            }
        }
        slot.generation
    });
    let Some(script) = script else { return };
    let view = webview.clone();
    let handler = AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |result, id| {
        if result.is_err() {
            return Ok(());
        }
        let latest = SCRIPTS.with(|scripts| match scripts.borrow_mut().get_mut(&tab) {
            Some(slot) if slot.generation == generation => {
                slot.id = Some(id.clone());
                true
            }
            _ => false,
        });
        if !latest {
            unsafe {
                let _ = view.RemoveScriptToExecuteOnDocumentCreated(&HSTRING::from(id));
            }
        }
        Ok(())
    }));
    unsafe {
        let _ = webview.AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script), &handler);
    }
}

pub enum Choice {
    Restore(usize),
    RestoreAll,
}

const MENU_ALL: usize = 10_000;

/// The list of what is hidden on `host`, at screen point (x, y).
fn choose(owner: HWND, (x, y): (i32, i32), host: &str, rules: &[Rule]) -> Option<Choice> {
    let mut menu = Menu::new();
    menu.note(&format!("Hidden on {host}: select an element to show again"));
    menu.separator();
    for (index, rule) in rules.iter().enumerate() {
        menu.item(index + 1, if rule.label.is_empty() { &rule.selector } else { &rule.label });
    }
    menu.separator();
    menu.item(MENU_ALL, "Show all");
    match crate::menu::track(owner, menu, Anchor::aligned(x, y, HAlign::Center, VAlign::Top)) {
        0 => None,
        MENU_ALL => Some(Choice::RestoreAll),
        n => Some(Choice::Restore(n - 1)),
    }
}

/// The feature's state in the app: the stored rules and the tabs whose page
/// has the picker open (only those may add rules, so a page can't hide
/// things on its own by posting the picker's message).
pub struct Curtain {
    store: Store,
    path: PathBuf,
    picking: HashSet<u64>,
}

impl Curtain {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("curtain.json");
        Self { store: storage::read_json(&path), path, picking: HashSet::new() }
    }

    fn save(&self) {
        storage::write_json_later(self.path.clone(), self.store.clone());
    }

    /// Wires a new view: its messages to the queue, and the hiding script.
    pub fn install(&self, web: &WebTab, tab: u64) {
        let mut token = 0i64;
        unsafe {
            let _ = web.webview.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let (mut source, mut json) = (PWSTR::null(), PWSTR::null());
                    args.Source(&mut source)?;
                    args.WebMessageAsJson(&mut json)?;
                    web::push(Event::WebMessage(tab, take_pwstr(source), take_pwstr(json)));
                    Ok(())
                })),
                &mut token,
            );
        }
        // A view made again for a tab (woken from sleep) starts with none.
        SCRIPTS.with(|scripts| {
            if let Some(slot) = scripts.borrow_mut().get_mut(&tab) {
                slot.id = None;
            }
        });
        register(&web.webview, tab, self.store.boot_script().as_deref());
    }

    /// A closed tab leaves nothing behind: one entry per tab ever opened
    /// added up in a long session.
    pub fn closed(&mut self, tab: u64) {
        self.picking.remove(&tab);
        SCRIPTS.with(|scripts| scripts.borrow_mut().remove(&tab));
    }

    /// A navigation ends the picker of that tab's page.
    pub fn navigated(&mut self, tab: u64) {
        self.picking.remove(&tab);
    }

    /// Ctrl+Shift+H: opens the picker, or closes it when it is open.
    pub fn toggle_picker(&mut self, tab: u64, web: &WebTab) {
        if host_of(&web.source()).is_none() {
            inject::execute(&web.webview, &inject::toast_script("You cannot hide elements on this page"));
            return;
        }
        // Closing posts "done", which takes the tab out again.
        self.picking.insert(tab);
        inject::execute(&web.webview, PICK);
    }

    pub fn on_message<'a>(
        &mut self,
        tab: u64,
        source: &str,
        json: &str,
        live: impl Iterator<Item = (u64, &'a WebTab)>,
    ) {
        match Message::parse(json) {
            Some(Message::Done) => {
                self.picking.remove(&tab);
            }
            Some(Message::Hide { selector, label }) => {
                // The host comes from the engine, not from what the page says.
                let Some(host) = host_of(source) else { return };
                if self.picking.contains(&tab) && self.store.hide(&host, &selector, &label) {
                    self.save();
                    self.refresh(live, None);
                }
            }
            None => {}
        }
    }

    /// Ctrl+Shift+U: lists what is hidden on the tab's site and puts back
    /// what the person picks, on every open page of the site at once. The
    /// caller gives the focus back to the page afterwards.
    pub fn show_hidden<'a>(
        &mut self,
        owner: HWND,
        at: (i32, i32),
        web: &WebTab,
        live: impl Iterator<Item = (u64, &'a WebTab)>,
    ) {
        let Some(host) = host_of(&web.source()) else {
            inject::execute(&web.webview, &inject::toast_script("You cannot hide elements on this page"));
            return;
        };
        if self.store.rules(&host).is_empty() {
            inject::execute(&web.webview, &inject::toast_script("No hidden elements on this site"));
            return;
        }
        let changed = match choose(owner, at, &host, self.store.rules(&host)) {
            Some(Choice::Restore(index)) => self.store.restore(&host, index),
            Some(Choice::RestoreAll) => self.store.restore_all(&host),
            None => false,
        };
        if changed {
            self.save();
            self.refresh(live, Some(&host));
        }
    }

    /// New registrations on every live view; with `host`, the pages of that
    /// site also get the current set right away.
    fn refresh<'a>(&self, live: impl Iterator<Item = (u64, &'a WebTab)>, host: Option<&str>) {
        let boot = self.store.boot_script();
        let apply = host.map(|h| self.store.apply_script(h));
        for (tab, web) in live {
            register(&web.webview, tab, boot.as_deref());
            if let (Some(host), Some(apply)) = (host, &apply)
                && host_of(&web.source()).as_deref() == Some(host)
            {
                inject::execute(&web.webview, apply);
            }
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/curtain.rs"]
#[rustfmt::skip]
mod tests;
