//! Extensions as the person meets them: the puzzle button and its native
//! menu, the pinned ones beside it, "Install extension" in the field when a
//! Web Store link is pasted, the developer's unpacked folders, and the
//! section in Settings. What is installed lives in `extensions::Registry`;
//! what the engine has loaded is brought in line with it (`sync_extensions`)
//! whenever a view exists to reach the profile through.
//!
//! WebView2 draws no extension UI (no toolbar, no popup bubble, no side
//! panel), so a popup, options page or side panel opens as a tab
//! (docs/extensions.md).

use std::path::PathBuf;

use webview2_com::{
    Microsoft::Web::WebView2::Win32::{GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2},
    take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::{
            Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, PWSTR, w},
};

use super::{App, Row, with_app};
use crate::{
    extension_host::{self, HostEvent},
    extensions::{self, Extension, Outcome, Registry, Staged},
    inject,
    settings_window::{self, ExtensionCommand},
    tabs::Live,
    webstore::{self, Answer, StoreRequest},
};

/// Asks the window to do something that opens a modal dialog (a folder
/// picker, a confirmation) once the app is no longer borrowed.
pub(super) const WM_APP_EXTENSION_ACTION: u32 = WM_APP + 41;
const ACTION_LOAD_UNPACKED: usize = 1;
const ACTION_REMOVE: usize = 2;
/// Asks the person about the first staged store package.
const ACTION_CONSENT: usize = 3;

const MENU_BASE: usize = 1000;
const MENU_STRIDE: usize = 16;
const ITEM_POPUP: usize = 0;
const ITEM_OPTIONS: usize = 1;
const ITEM_SIDE_PANEL: usize = 2;
const ITEM_ENABLED: usize = 3;
const ITEM_PINNED: usize = 4;
const ITEM_RELOAD: usize = 5;
const ITEM_REMOVE: usize = 6;
const MENU_INSTALL_HERE: usize = 900;
const MENU_LOAD_UNPACKED: usize = 901;
const MENU_SETTINGS: usize = 902;
const MENU_NOTE: usize = 903;
const MENU_WEB_STORE: usize = 904;
/// Where "Get extensions" goes.
const WEB_STORE_URL: &str = "https://chromewebstore.google.com";
/// Used when the runtime's version can't be read.
const FALLBACK_BROWSER_VERSION: &str = "157.0.0.0";

#[derive(Default)]
pub(super) struct Extensions {
    pub(super) registry: Registry,
    /// Whether the running engine has extension support; None until the
    /// engine starts (it starts with the first page).
    engine_on: Option<bool>,
    /// Folders the engine is loading, so a second sync doesn't load them twice.
    adding: Vec<String>,
    /// Store ids being downloaded, waiting for a yes, or being committed.
    installing: Vec<String>,
    /// Verified store packages that wait for the person's yes, in order.
    consent: Vec<Staged>,
    /// Store pages that wait for the end of an install: (store id, tab).
    pages: Vec<(String, u64)>,
    /// Store pages that wait for the end of a removal: (store id, tab).
    removing: Vec<(String, u64)>,
    /// Tabs showing a page of an extension being reloaded: the engine moves
    /// them off the page while it unloads, and they go back once it is in.
    restore: Vec<(String, Vec<(u64, String)>)>,
    synced: bool,
    /// The last thing that happened, shown first in the menu.
    notice: Option<String>,
}

impl Extensions {
    pub(super) fn load() -> Self {
        let registry = Registry::load();
        extensions::sweep(&registry);
        Self { registry, ..Default::default() }
    }
}

fn on_settings_command(command: ExtensionCommand, index: Option<usize>) {
    match command {
        // Both open a modal dialog: done from the window, not from here.
        ExtensionCommand::LoadUnpacked => {
            if let Some(hwnd) = with_app(|app| app.hwnd) {
                post_action(hwnd, ACTION_LOAD_UNPACKED, 0);
            }
        }
        ExtensionCommand::Remove => {
            if let (Some(index), Some(hwnd)) = (index, with_app(|app| app.hwnd)) {
                post_action(hwnd, ACTION_REMOVE, index);
            }
        }
        _ => {
            with_app(|app| app.settings_extension_command(command, index));
        }
    }
}

fn post_action(hwnd: HWND, action: usize, index: usize) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_EXTENSION_ACTION, WPARAM(action), LPARAM(index as isize));
    }
}

/// The window messages that belong to extensions; None for any other.
pub(super) fn window_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        extensions::WM_APP_EXTENSION_INSTALLED => {
            if with_app(|app| app.extensions_installed()).is_none() {
                super::retry_later(hwnd, msg, wparam, lparam);
            }
            Some(LRESULT(0))
        }
        WM_APP_EXTENSION_ACTION => {
            match wparam.0 {
                ACTION_LOAD_UNPACKED => {
                    if let Some(folder) = pick_folder(hwnd) {
                        with_app(|app| app.add_unpacked(folder));
                    }
                }
                ACTION_CONSENT => {
                    let next =
                        with_app(|app| (!app.extensions.consent.is_empty()).then(|| app.extensions.consent.remove(0)));
                    match next {
                        None => super::retry_later(hwnd, msg, wparam, lparam),
                        Some(None) => {}
                        Some(Some(staged)) => {
                            let yes = confirm(hwnd, &extensions::consent_text(&staged.extension, &staged.warnings));
                            let mut staged = Some(staged);
                            with_app(|app| {
                                if let Some(staged) = staged.take() {
                                    app.consent_answered(staged, yes);
                                }
                            });
                            // No app to tell: the files go, and the next start sweeps the rest.
                            if let Some(staged) = staged {
                                extensions::discard_in_background(staged);
                            }
                        }
                    }
                }
                ACTION_REMOVE => {
                    let index = lparam.0 as usize;
                    let name =
                        with_app(|app| app.extensions.registry.extensions.get(index).map(|e| e.name.clone())).flatten();
                    if let Some(name) = name {
                        let yes = confirm(
                            hwnd,
                            &format!("Remove '{name}'?\n\nNea deletes its data, and it stops working in all tabs."),
                        );
                        with_app(|app| app.removal_answered(index, &name, yes));
                    }
                }
                _ => {}
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

fn confirm(owner: HWND, text: &str) -> bool {
    let _modal = crate::diag::modal();
    unsafe {
        MessageBoxW(
            Some(owner),
            &HSTRING::from(text),
            w!("Extensions — Nea"),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDYES
    }
}

/// The system folder picker (IFileOpenDialog with FOS_PICKFOLDERS).
fn pick_folder(owner: HWND) -> Option<PathBuf> {
    let _modal = crate::diag::modal();
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(w!("Load unpacked extension: select the folder with manifest.json"));
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        Some(PathBuf::from(take_pwstr(path)))
    }
}

/// The runtime's version, which the store wants to pick a compatible package.
fn browser_version() -> String {
    let mut version = PWSTR::null();
    unsafe {
        if GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version).is_ok() && !version.is_null() {
            let text = take_pwstr(version);
            // "157.0.3800.10 beta" -> the number.
            if let Some(number) = text.split_whitespace().next().filter(|n| !n.is_empty()) {
                return number.to_string();
            }
        }
    }
    FALLBACK_BROWSER_VERSION.to_string()
}

fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    unsafe {
        let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text));
    }
}

/// One line of the menu that shows when no extension is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyLine {
    /// Text that only informs: it is greyed and does nothing.
    Note(&'static str),
    Separator,
    /// A command: its menu id and its text.
    Command(usize, &'static str),
}

/// The menu with no extension installed: it says so and tells how to get
/// one. `store_here`: the page in view is the Web Store page of an extension.
fn empty_menu_lines(store_here: bool) -> Vec<EmptyLine> {
    let mut lines = vec![EmptyLine::Note("No extensions installed"), EmptyLine::Separator];
    if store_here {
        lines.push(EmptyLine::Command(MENU_INSTALL_HERE, "Install extension from this page"));
    }
    lines.push(EmptyLine::Command(MENU_WEB_STORE, "Get extensions from the Chrome Web Store…"));
    lines.push(EmptyLine::Command(MENU_SETTINGS, "Manage extensions…"));
    lines
}

fn append_empty_lines(menu: HMENU, lines: &[EmptyLine]) {
    for line in lines {
        match *line {
            EmptyLine::Note(text) => append(menu, MF_STRING | MF_GRAYED, MENU_NOTE, text),
            EmptyLine::Separator => append(menu, MF_SEPARATOR, 0, ""),
            EmptyLine::Command(id, text) => append(menu, MF_STRING, id, text),
        }
    }
}

/// Takes out the tabs that wait for `id`.
fn take_pages(pages: &mut Vec<(String, u64)>, id: &str) -> Vec<u64> {
    let mut taken = Vec::new();
    pages.retain(|(i, tab)| {
        if i == id {
            taken.push(*tab);
            false
        } else {
            true
        }
    });
    taken
}

fn source_label(extension: &Extension) -> &'static str {
    if extension.unpacked() { "Unpacked" } else { "Chrome Web Store" }
}

impl App {
    // ---------- what the rest of the app asks ----------

    /// The engine is about to start: extension support only if one is
    /// enabled, since each extension adds processes.
    pub(super) fn extensions_for_environment(&mut self) -> bool {
        let on = self.extensions.registry.any_enabled();
        self.extensions.engine_on = Some(on);
        on
    }

    /// For the strip: the button always shows (with no extension, its menu
    /// tells how to get one), with this many pinned ones beside it.
    pub(super) fn extension_slots(&self) -> Option<usize> {
        Some(self.extensions.registry.pinned().len())
    }

    pub(super) fn extension_initials(&self) -> Vec<char> {
        self.extensions.registry.pinned().into_iter().map(|(_, c)| c).collect()
    }

    /// A Web Store link in the field becomes a row that installs it.
    pub(super) fn extension_row(&self, text: &str) -> Option<(String, Row)> {
        let id = webstore::extension_id_from_link(text)?;
        let label = match self.extensions.registry.find_store(&id).map(|i| &self.extensions.registry.extensions[i]) {
            Some(installed) => format!("Update the extension '{}' from the Chrome Web Store", installed.name),
            None => format!("Install extension from the Chrome Web Store    {id}"),
        };
        Some((label, Row::InstallExtension(id)))
    }

    fn restart_needed(&self) -> bool {
        matches!(self.extensions.engine_on, Some(on) if on != self.extensions.registry.any_enabled())
    }

    fn any_webview(&self) -> Option<ICoreWebView2> {
        let active = self.tabs.active_tab().and_then(|t| match &t.live {
            Live::Ready(web) => Some(web.webview.clone()),
            _ => None,
        });
        active.or_else(|| {
            self.tabs.tabs.iter().find_map(|t| match &t.live {
                Live::Ready(web) => Some(web.webview.clone()),
                _ => None,
            })
        })
    }

    /// Says what happened on the page in view (and keeps it for the menu).
    fn extension_notice(&mut self, text: String) {
        if let Some(web) = self.tabs.active_tab().and_then(|t| match &t.live {
            Live::Ready(web) => Some(web.webview.clone()),
            _ => None,
        }) {
            inject::execute(&web, &inject::toast_script(&text));
        }
        self.extensions.notice = Some(text);
        self.refresh_extension_views();
    }

    /// Strip, Settings and storage after a change of the list.
    fn refresh_extension_views(&mut self) {
        self.relayout_soft();
        self.sync_settings_extensions();
    }

    fn save_extensions(&mut self) {
        self.extensions.registry.save();
        self.refresh_extension_views();
    }

    // ---------- the engine ----------

    /// The first view after start: bring the profile in line once.
    pub(super) fn extensions_view_ready(&mut self) {
        if !self.extensions.synced {
            self.extensions.synced = true;
            self.sync_extensions();
        }
    }

    /// Loads (or reloads in place) a folder, keeping its open pages.
    fn load_folder(&mut self, webview: &ICoreWebView2, path: &str) {
        self.extensions.adding.push(path.to_string());
        let open = self
            .extensions
            .registry
            .find_path(path)
            .and_then(|i| self.extensions.registry.extensions[i].engine_id.clone())
            .map(|id| self.extension_tabs(&id))
            .unwrap_or_default();
        if !open.is_empty() {
            self.extensions.restore.push((path.to_string(), open));
        }
        extension_host::add(webview, path);
    }

    /// Tabs on one of the extension's pages: (tab, address).
    fn extension_tabs(&self, engine_id: &str) -> Vec<(u64, String)> {
        let prefix = format!("chrome-extension://{engine_id}/");
        self.tabs.tabs.iter().filter(|t| t.url.starts_with(&prefix)).map(|t| (t.id, t.url.clone())).collect()
    }

    /// An extension that goes away takes its pages with it, as in Chrome.
    fn close_extension_tabs(&mut self, engine_id: Option<&str>) {
        let Some(engine_id) = engine_id else { return };
        for (tab, _) in self.extension_tabs(engine_id) {
            self.close_tab(tab);
        }
    }

    fn sync_extensions(&mut self) {
        if self.extensions.engine_on != Some(true) {
            return;
        }
        if let Some(webview) = self.any_webview() {
            extension_host::list(&webview);
        } else {
            // No view yet: the next one syncs.
            self.extensions.synced = false;
        }
    }

    pub(super) fn on_extension_event(&mut self, event: HostEvent) {
        match event {
            HostEvent::Listed(Ok(engine)) => {
                let plan = extensions::plan(&self.extensions.registry, &engine);
                self.extensions.registry.removed.retain(|id| engine.contains_key(id) && !plan.remove.contains(id));
                let Some(webview) = self.any_webview() else { return };
                for index in plan.add {
                    let path = self.extensions.registry.extensions[index].path.clone();
                    if !self.extensions.adding.contains(&path) {
                        self.load_folder(&webview, &path);
                    }
                }
                extension_host::apply(&webview, plan.enable, plan.remove);
                self.extensions.registry.save();
            }
            HostEvent::Listed(Err(error)) => self.extension_notice(format!("Extensions: {error}")),
            HostEvent::Added(path, result) => {
                self.extensions.adding.retain(|p| *p != path);
                if let Some(at) = self.extensions.restore.iter().position(|(p, _)| *p == path) {
                    let (_, open) = self.extensions.restore.remove(at);
                    for (id, url) in open {
                        if let Some(tab) = self.tabs.get_mut(id) {
                            tab.url = url.clone();
                            if let Live::Ready(web) = &tab.live {
                                web.navigate(&url);
                            }
                        }
                    }
                }
                let Some(index) = self.extensions.registry.find_path(&path) else { return };
                match result {
                    Ok((id, _)) => {
                        let extension = &mut self.extensions.registry.extensions[index];
                        extension.engine_id = Some(id.clone());
                        let (enabled, name) = (extension.enabled, extension.name.clone());
                        if !enabled && let Some(webview) = self.any_webview() {
                            extension_host::apply(&webview, vec![(id, false)], Vec::new());
                        }
                        self.save_extensions();
                        self.extension_notice(format!("Extension ready: {name}"));
                    }
                    Err(error) => {
                        let name = self.extensions.registry.extensions[index].name.clone();
                        self.extension_notice(format!("Cannot load '{name}': {error}"));
                    }
                }
            }
            HostEvent::Changed => {}
            HostEvent::StoreDownload(tab, id) => self.install_from_store(&id, Some(tab)),
        }
    }

    // ---------- installing ----------

    /// Installs a store extension: the download, the check and the unpack
    /// run on a worker thread; then the person says yes or no; then the
    /// files go in place on a worker thread. `page`: the store tab that
    /// waits for the answer, if the store's own button started it.
    pub(super) fn install_from_store(&mut self, id: &str, page: Option<u64>) {
        if let Some(tab) = page
            && !self.extensions.pages.iter().any(|(i, t)| i == id && *t == tab)
        {
            self.extensions.pages.push((id.to_string(), tab));
        }
        if self.extensions.installing.iter().any(|i| i == id) {
            return;
        }
        self.extensions.installing.push(id.to_string());
        self.extension_notice("Downloading the extension from the Chrome Web Store…".to_string());
        let hwnd = self.hwnd.0 as isize;
        let id = id.to_string();
        // The store gives some extensions only to signed-in persons: the
        // worker sends the session of the engine profile again if the first
        // try is refused.
        let webview = page.and_then(|tab| self.tab_webview(tab)).or_else(|| self.any_webview());
        let start = move |cookie: Option<String>| {
            extensions::stage_in_background(hwnd, id, browser_version, cookie);
        };
        match webview {
            Some(webview) => extension_host::cookie_header(&webview, webstore::UPDATE_SERVICE, start),
            None => start(None),
        }
    }

    fn tab_webview(&self, tab: u64) -> Option<ICoreWebView2> {
        self.tabs.get(tab).and_then(|t| match &t.live {
            Live::Ready(web) => Some(web.webview.clone()),
            _ => None,
        })
    }

    /// Gives the store pages that wait for `id` their answer.
    fn answer_store_pages(&mut self, id: &str, answer: Answer, installed: Option<&Extension>) {
        let (name, version) = installed.map_or(("", ""), |e| (e.name.as_str(), e.version.as_str()));
        let script = webstore::answer_script(id, answer, name, version);
        let pages = take_pages(&mut self.extensions.pages, id);
        self.run_in_tabs(&pages, &script);
    }

    fn run_in_tabs(&self, tabs: &[u64], script: &str) {
        for tab in tabs {
            if let Some(webview) = self.tab_webview(*tab) {
                inject::execute(&webview, script);
            }
        }
    }

    /// What the store page script asks (its own buttons).
    pub(super) fn store_page_request(&mut self, tab: u64, request: StoreRequest) {
        match request {
            StoreRequest::List => {
                let items: Vec<webstore::Listed> = self
                    .extensions
                    .registry
                    .extensions
                    .iter()
                    .filter_map(|e| {
                        Some(webstore::Listed {
                            id: e.store_id.clone()?,
                            name: e.name.clone(),
                            version: e.version.clone(),
                            enabled: e.enabled,
                        })
                    })
                    .collect();
                self.run_in_tabs(&[tab], &webstore::list_script(&items));
            }
            StoreRequest::Install(id) => self.install_from_store(&id, Some(tab)),
            StoreRequest::Remove(id) => match self.extensions.registry.find_store(&id) {
                Some(index) => {
                    if !self.extensions.removing.iter().any(|(i, t)| *i == id && *t == tab) {
                        self.extensions.removing.push((id, tab));
                    }
                    post_action(self.hwnd, ACTION_REMOVE, index);
                }
                None => self.run_in_tabs(&[tab], &webstore::removed_script(&id, false)),
            },
        }
    }

    /// The person answered the question about a removal.
    fn removal_answered(&mut self, index: usize, name: &str, yes: bool) {
        let store_id = self.extensions.registry.extensions.get(index).and_then(|e| e.store_id.clone());
        if yes {
            self.remove_extension(index, name);
        }
        if let Some(id) = store_id {
            let pages = take_pages(&mut self.extensions.removing, &id);
            self.run_in_tabs(&pages, &webstore::removed_script(&id, yes));
        }
    }

    /// The end of an install that did not happen.
    fn install_failed(&mut self, id: &str, answer: Answer, notice: String) {
        self.extensions.installing.retain(|i| i != id);
        self.answer_store_pages(id, answer, None);
        self.extension_notice(notice);
    }

    /// The worker thread finished one or more steps of store installs.
    fn extensions_installed(&mut self) {
        for (id, outcome) in extensions::take_finished() {
            match outcome {
                Outcome::Staged(staged) => {
                    self.extensions.consent.push(staged);
                    post_action(self.hwnd, ACTION_CONSENT, 0);
                }
                Outcome::Installed(extension) => {
                    self.extensions.installing.retain(|i| *i != id);
                    self.answer_store_pages(&id, Answer::Installed, Some(&extension));
                    self.extension_installed(extension);
                }
                Outcome::Failed(error) => {
                    self.install_failed(&id, Answer::Failed, format!("Cannot install the extension: {error}"))
                }
            }
        }
    }

    /// The person answered the question about a staged package.
    fn consent_answered(&mut self, staged: Staged, yes: bool) {
        if yes {
            self.extension_notice(format!("Installing: {}…", staged.extension.name));
            extensions::commit_in_background(self.hwnd.0 as isize, staged);
        } else {
            let (id, name) = (staged.id.clone(), staged.extension.name.clone());
            extensions::discard_in_background(staged);
            self.install_failed(&id, Answer::Cancelled, format!("Not installed: {name}"));
        }
    }

    /// A store extension is in its folder: the registry and the engine.
    fn extension_installed(&mut self, extension: Extension) {
        let name = extension.name.clone();
        let path = extension.path.clone();
        self.extensions.registry.upsert(extension);
        self.save_extensions();
        match self.extensions.engine_on {
            Some(true) => {
                // New or an update over loaded files: adding the
                // folder loads it, or reloads it in place.
                if let Some(webview) = self.any_webview() {
                    self.load_folder(&webview, &path);
                    self.extension_notice(format!("Installed: {name}. Loading…"));
                } else {
                    self.extensions.synced = false;
                    self.extension_notice(format!("Installed: {name}. It loads with the next tab."));
                }
            }
            Some(false) => self.extension_notice(format!(
                "Installed: {name}. It starts when you restart Nea. With no extensions, the engine starts without extension support to save memory."
            )),
            None => self.extension_notice(format!("Installed: {name}.")),
        }
    }

    fn add_unpacked(&mut self, folder: PathBuf) {
        match extensions::from_unpacked(&folder) {
            Ok(extension) => {
                let name = extension.name.clone();
                let path = extension.path.clone();
                self.extensions.registry.upsert(extension);
                self.save_extensions();
                match self.extensions.engine_on {
                    Some(true) => match self.any_webview() {
                        Some(webview) => {
                            self.load_folder(&webview, &path);
                        }
                        None => self.extensions.synced = false,
                    },
                    Some(false) => self.extension_notice(format!("Added: {name}. It starts when you restart Nea.")),
                    None => self.extension_notice(format!("Added: {name}.")),
                }
            }
            Err(error) => self.extension_notice(format!("Cannot load {}: {error}", folder.display())),
        }
    }

    // ---------- the person's actions ----------

    fn toggle_extension(&mut self, index: usize) {
        let Some(extension) = self.extensions.registry.extensions.get_mut(index) else { return };
        extension.enabled = !extension.enabled;
        let (enabled, name, engine_id) = (extension.enabled, extension.name.clone(), extension.engine_id.clone());
        if !enabled {
            self.close_extension_tabs(engine_id.as_deref());
        }
        self.save_extensions();
        if self.extensions.engine_on == Some(true) {
            self.sync_extensions();
        }
        let state = if enabled { "enabled" } else { "disabled" };
        let tail = if self.restart_needed() { " Restart Nea to turn extension support on or off." } else { "" };
        self.extension_notice(format!("{name}: {state}.{tail}"));
    }

    fn toggle_pin(&mut self, index: usize) {
        if let Some(extension) = self.extensions.registry.extensions.get_mut(index) {
            extension.pinned = !extension.pinned;
            self.save_extensions();
        }
    }

    fn reload_extension(&mut self, index: usize) {
        let Some(extension) = self.extensions.registry.extensions.get_mut(index) else { return };
        if let Err(error) = extensions::refresh(extension) {
            let name = extension.name.clone();
            self.extension_notice(format!("Cannot reload '{name}': {error}"));
            return;
        }
        let (path, name) = (extension.path.clone(), extension.name.clone());
        self.save_extensions();
        match (self.extensions.engine_on, self.any_webview()) {
            (Some(true), Some(webview)) => {
                self.load_folder(&webview, &path);
                self.extension_notice(format!("Reloading '{name}'…"));
            }
            _ => self.extension_notice(format!("'{name}' reloads when the engine has extension support.")),
        }
    }

    fn remove_extension(&mut self, index: usize, name: &str) {
        let Some(extension) = self.extensions.registry.remove(index) else { return };
        self.close_extension_tabs(extension.engine_id.as_deref());
        if let (Some(true), Some(webview), Some(id)) =
            (self.extensions.engine_on, self.any_webview(), extension.engine_id.clone())
        {
            extension_host::apply(&webview, Vec::new(), vec![id.clone()]);
            self.extensions.registry.removed.retain(|r| *r != id);
        }
        extensions::delete_files(&extension);
        self.save_extensions();
        self.extension_notice(format!("Removed: {name}"));
    }

    /// Opens one of the extension's pages in a tab next to the active one.
    fn open_extension_page(&mut self, index: usize, page: Option<String>) {
        let Some(extension) = self.extensions.registry.extensions.get(index) else { return };
        let Some(url) = page.and_then(|p| extension.page_url(&p)) else { return };
        let at = self.tabs.after_active();
        self.open_tab(&url, at);
    }

    /// A click on a pinned extension: its popup (as a tab), else its options,
    /// else the menu.
    pub(super) fn open_pinned_extension(&mut self, nth: usize) {
        let Some((index, _)) = self.extensions.registry.pinned().get(nth).copied() else { return };
        let extension = &self.extensions.registry.extensions[index];
        let usable = self.extensions.engine_on == Some(true) && extension.enabled && extension.engine_id.is_some();
        let page = extension.popup.clone().or_else(|| extension.options.clone());
        if usable && page.is_some() {
            self.open_extension_page(index, page);
        } else {
            self.extensions_menu();
        }
    }

    /// The puzzle button's menu: every extension with what can be done with
    /// it, then installing and Settings. With no extension: a short menu
    /// that tells how to get one.
    pub(super) fn extensions_menu(&mut self) {
        let store_here = self
            .tabs
            .active_tab()
            .and_then(|t| webstore::extension_id_from_link(&t.url))
            .filter(|id| self.extensions.registry.find_store(id).is_none());
        let engine_on = self.extensions.engine_on == Some(true);
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
            let grey = |on: bool| if on { MF_ENABLED } else { MF_GRAYED };
            if let Some(notice) = &self.extensions.notice {
                let mut short: String = notice.chars().take(90).collect();
                if short.len() < notice.len() {
                    short.push('…');
                }
                append(menu, MF_STRING | MF_GRAYED, MENU_NOTE, &short);
                append(menu, MF_SEPARATOR, 0, "");
            }
            if self.extensions.registry.extensions.is_empty() {
                append_empty_lines(menu, &empty_menu_lines(store_here.is_some()));
            }
            for (i, extension) in self.extensions.registry.extensions.iter().enumerate() {
                let Ok(sub) = CreatePopupMenu() else { continue };
                let base = MENU_BASE + i * MENU_STRIDE;
                let usable = engine_on && extension.enabled && extension.engine_id.is_some();
                let pages = [
                    (ITEM_POPUP, &extension.popup, "Open pop-up (in a tab)"),
                    (ITEM_OPTIONS, &extension.options, "Options"),
                    (ITEM_SIDE_PANEL, &extension.side_panel, "Side panel (in a tab)"),
                ];
                for (item, page, label) in pages {
                    if page.is_some() {
                        append(sub, MF_STRING | grey(usable), base + item, label);
                    }
                }
                if pages.iter().any(|(_, p, _)| p.is_some()) {
                    append(sub, MF_SEPARATOR, 0, "");
                }
                append(sub, MF_STRING | check(extension.enabled), base + ITEM_ENABLED, "Enabled");
                append(sub, MF_STRING | check(extension.pinned), base + ITEM_PINNED, "Pin next to the button");
                if extension.unpacked() {
                    append(sub, MF_STRING, base + ITEM_RELOAD, "Reload from folder");
                }
                append(sub, MF_STRING, base + ITEM_REMOVE, "Remove…");
                append(sub, MF_SEPARATOR, 0, "");
                let about = format!("Version {} · {}", extension.version, source_label(extension));
                append(sub, MF_STRING | MF_GRAYED, MENU_NOTE, &about);
                let title =
                    if extension.enabled { extension.name.clone() } else { format!("{} (disabled)", extension.name) };
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, &HSTRING::from(title));
            }
            if !self.extensions.registry.extensions.is_empty() {
                append(menu, MF_SEPARATOR, 0, "");
                if store_here.is_some() {
                    append(menu, MF_STRING, MENU_INSTALL_HERE, "Install extension from this page");
                }
                append(menu, MF_STRING, MENU_LOAD_UNPACKED, "Load unpacked extension…");
                append(menu, MF_STRING, MENU_SETTINGS, "Extensions in Settings…");
                append(menu, MF_SEPARATOR, 0, "");
                let note = if self.restart_needed() {
                    "Restart Nea to apply the extension changes"
                } else {
                    "Each enabled extension adds processes and memory"
                };
                append(menu, MF_STRING | MF_GRAYED, MENU_NOTE, note);
            }

            let button = self.layout.extensions;
            let mut point = POINT { x: button.left, y: button.bottom };
            let flags = if self.layout.vertical {
                point = POINT { x: button.right, y: button.top };
                TPM_RETURNCMD | TPM_LEFTALIGN | TPM_BOTTOMALIGN
            } else {
                TPM_RETURNCMD | TPM_RIGHTALIGN | TPM_TOPALIGN
            };
            if !self.layout.vertical {
                point.x = button.right;
            }
            let _ = ClientToScreen(self.hwnd, &mut point);
            // Its time is the person reading the menu.
            self.save_before_modal();
            let modal = crate::diag::modal();
            let chosen = TrackPopupMenu(menu, flags, point.x, point.y, None, self.hwnd, None).0 as usize;
            drop(modal);
            let _ = DestroyMenu(menu);
            chosen
        };
        match chosen {
            0 | MENU_NOTE => {}
            MENU_INSTALL_HERE => {
                if let Some(id) = store_here {
                    self.install_from_store(&id, None);
                }
            }
            MENU_LOAD_UNPACKED => post_action(self.hwnd, ACTION_LOAD_UNPACKED, 0),
            MENU_WEB_STORE => {
                let at = self.tabs.after_active();
                self.open_tab(WEB_STORE_URL, at);
            }
            MENU_SETTINGS => self.open_settings(),
            id if id >= MENU_BASE => {
                let (index, item) = ((id - MENU_BASE) / MENU_STRIDE, (id - MENU_BASE) % MENU_STRIDE);
                let Some(extension) = self.extensions.registry.extensions.get(index) else { return };
                match item {
                    ITEM_POPUP => self.open_extension_page(index, extension.popup.clone()),
                    ITEM_OPTIONS => self.open_extension_page(index, extension.options.clone()),
                    ITEM_SIDE_PANEL => self.open_extension_page(index, extension.side_panel.clone()),
                    ITEM_ENABLED => self.toggle_extension(index),
                    ITEM_PINNED => self.toggle_pin(index),
                    ITEM_RELOAD => self.reload_extension(index),
                    ITEM_REMOVE => post_action(self.hwnd, ACTION_REMOVE, index),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // ---------- Settings ----------

    /// Wires the Extensiones section of an open settings window.
    pub(super) fn attach_settings_extensions(&mut self) {
        settings_window::set_extension_handler(on_settings_command);
        self.sync_settings_extensions();
    }

    fn sync_settings_extensions(&self) {
        let rows: Vec<String> = self
            .extensions
            .registry
            .extensions
            .iter()
            .map(|e| {
                let mut parts = vec![e.name.clone()];
                if !e.version.is_empty() {
                    parts.push(e.version.clone());
                }
                parts.push(if e.enabled { "enabled" } else { "disabled" }.to_string());
                if e.pinned {
                    parts.push("pinned".to_string());
                }
                parts.push(source_label(e).to_string());
                parts.join("  ·  ")
            })
            .collect();
        let enabled = self.extensions.registry.extensions.iter().filter(|e| e.enabled).count();
        let note = if self.restart_needed() {
            "Restart Nea to apply the changes. The engine sets extension support when it starts.".to_string()
        } else if enabled == 0 {
            "With no enabled extensions, the engine starts without extension support and uses no memory for it."
                .to_string()
        } else {
            format!("{enabled} enabled. Each one adds its own processes and memory (about 20-40 MB).")
        };
        settings_window::set_extensions(&rows, &note);
    }

    fn settings_extension_command(&mut self, command: ExtensionCommand, index: Option<usize>) {
        let Some(index) = index.filter(|i| *i < self.extensions.registry.extensions.len()) else { return };
        match command {
            ExtensionCommand::Toggle => self.toggle_extension(index),
            ExtensionCommand::Pin => self.toggle_pin(index),
            ExtensionCommand::Reload => self.reload_extension(index),
            ExtensionCommand::Remove | ExtensionCommand::LoadUnpacked => {}
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/extensions_ui.rs"]
#[rustfmt::skip]
mod tests;
