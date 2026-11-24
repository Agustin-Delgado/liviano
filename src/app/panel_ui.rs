//! The bookmarks, history and downloads panels, drawn by the single field in
//! its panel modes. What a panel shows is read when it opens and dropped when
//! it closes; only the downloads still in progress live in memory.

use std::path::Path;

use windows::{
    Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    core::{HSTRING, PCWSTR, w},
};

use super::{App, Mode, Row, unix_now};
use crate::{
    bookmarks::Bookmarks,
    download_watch,
    downloads::{self, Download, State},
    omnibox,
    panels::{self, PanelKind},
    shortcuts::{self, Command},
    tabs::Live,
};

/// Rows a panel lists at most; the search narrows the rest.
const PANEL_LIMIT: usize = 200;
/// Rows a panel shows at once (the list scrolls).
pub(super) const PANEL_VISIBLE_ROWS: usize = 16;
/// Bookmarks offered first among the field's suggestions.
const BOOKMARK_SUGGESTIONS: usize = 3;

#[derive(Clone, Debug)]
pub(super) enum Item {
    Url(String),
    Download(u64),
    ClearHistory,
    ConfirmClearHistory,
    EngineDownloads,
    /// A saved password (see `vault_ui`).
    Account(crate::vault::Account),
    NeverSite(String),
    ImportPasswords,
}

#[derive(Default)]
pub(super) struct Panels {
    /// Read when the field or the bookmarks panel opens.
    bookmarks: Option<Bookmarks>,
    /// downloads.json plus live progress, while the downloads panel is open.
    downloads: Option<Vec<Download>>,
    /// Downloads in progress: the only panel data always in memory.
    running: Vec<Download>,
    /// A first line saying what just happened (a page marked).
    notice: Option<String>,
    confirm_clear: bool,
    /// The passwords panel, while it is open.
    pub(super) passwords: Option<super::vault_ui::PasswordPanel>,
}

impl Panels {
    pub(super) fn release(&mut self) {
        self.bookmarks = None;
        self.downloads = None;
        self.notice = None;
        self.confirm_clear = false;
        self.passwords = None;
    }
}

fn info(text: impl Into<String>) -> (String, Row) {
    (text.into(), Row::Info)
}

/// "Title    address", the way the field's rows read.
fn page_label(title: &str, url: &str) -> String {
    let address = omnibox::strip_for_match(url);
    let title = if title.trim().is_empty() { address } else { title };
    format!("{title}    {address}")
}

/// The local time zone, as seconds east of UTC (from the clock itself, so
/// summer time is included).
fn local_offset() -> i64 {
    use windows::Win32::{
        Foundation::SYSTEMTIME,
        System::SystemInformation::{GetLocalTime, GetSystemTime},
    };
    let seconds = |t: SYSTEMTIME| {
        panels::days_from_civil(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 86_400
            + t.wHour as i64 * 3600
            + t.wMinute as i64 * 60
    };
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let offset = seconds(local) - seconds(utc);
    // Zones are whole quarters of an hour; this drops a minute ticking between the two reads.
    (offset as f64 / 900.0).round() as i64 * 900
}

fn shell_open(file: &str, parameters: Option<&str>) {
    let parameters = parameters.map(HSTRING::from);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(file),
            parameters.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr())),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Opens Explorer on the file's folder with the file selected.
fn show_in_folder(path: &str) {
    if Path::new(path).exists() {
        shell_open("explorer.exe", Some(&format!("/select,\"{path}\"")));
    } else if let Some(folder) = Path::new(path).parent().filter(|f| f.exists()) {
        shell_open(&folder.to_string_lossy(), None);
    }
}

impl App {
    /// What a mode needs from disk, read as it opens (the previous mode's
    /// copy is dropped first).
    pub(super) fn load_panel_data(&mut self, mode: Mode) {
        self.panels.release();
        match mode {
            Mode::Address | Mode::NewTab | Mode::NewPrivateTab | Mode::Panel(PanelKind::Bookmarks) => {
                self.panels.bookmarks = Some(Bookmarks::load());
            }
            Mode::Panel(PanelKind::Downloads) => {
                self.panels.downloads = Some(downloads::merged(downloads::load(), &self.panels.running));
            }
            Mode::Panel(PanelKind::Passwords) => {
                self.panels.passwords = Some(self.password_panel_load());
            }
            Mode::Panel(PanelKind::History)
            | Mode::Switch
            | Mode::Find
            | Mode::Split
            | Mode::RenameGroup(_)
            | Mode::RenameSpace(_)
            | Mode::RenameBookmark => {}
        }
    }

    pub(super) fn open_panel(&mut self, kind: PanelKind) {
        self.open_omnibar(Mode::Panel(kind));
    }

    pub(super) fn chord_hint(&self, command: Command) -> String {
        shortcuts::chords_of(&self.settings.shortcuts, command)
            .first()
            .map(|c| shortcuts::format_chord(*c))
            .unwrap_or_default()
    }

    pub(super) fn panel_rows(&self, kind: PanelKind, query: &str) -> Vec<(String, Row)> {
        let mut rows: Vec<(String, Row)> = Vec::new();
        if let Some(notice) = &self.panels.notice {
            rows.push(info(notice.clone()));
        }
        match kind {
            PanelKind::Bookmarks => {
                let found = self.panels.bookmarks.as_ref().map(|b| b.search(query)).unwrap_or_default();
                if found.is_empty() {
                    rows.push(info(if query.trim().is_empty() {
                        format!(
                            "No bookmarks yet. {} bookmarks the current page.",
                            self.chord_hint(Command::ToggleBookmark)
                        )
                    } else {
                        "No results".to_string()
                    }));
                }
                rows.extend(
                    found
                        .into_iter()
                        .take(PANEL_LIMIT)
                        .map(|b| (page_label(&b.title, &b.url), Row::Panel(Item::Url(b.url.clone())))),
                );
            }
            PanelKind::History => {
                let found = self.history.recent(query, PANEL_LIMIT);
                if query.trim().is_empty() && !self.history.entries().is_empty() {
                    rows.push(if self.panels.confirm_clear {
                        (
                            format!(
                                "Are you sure? Press Enter again to delete all {} history entries · Esc cancels",
                                self.history.entries().len()
                            ),
                            Row::Panel(Item::ConfirmClearHistory),
                        )
                    } else {
                        ("Clear all history…".to_string(), Row::Panel(Item::ClearHistory))
                    });
                }
                if found.is_empty() {
                    rows.push(info(if query.trim().is_empty() { "History is empty" } else { "No results" }));
                }
                let (now, offset) = (unix_now(), local_offset());
                rows.extend(found.into_iter().map(|e| {
                    let label =
                        format!("{}    {}", page_label(&e.title, &e.url), panels::format_when(e.last, now, offset));
                    (label, Row::Panel(Item::Url(e.url.clone())))
                }));
            }
            PanelKind::Passwords => rows.extend(self.password_rows(query)),
            PanelKind::Downloads => {
                let list = self.panels.downloads.as_deref().unwrap_or_default();
                let found = downloads::search(list, query);
                if found.is_empty() {
                    rows.push(info(if query.trim().is_empty() { "No downloads" } else { "No results" }));
                }
                rows.extend(found.into_iter().take(PANEL_LIMIT).map(|d| {
                    let exists = d.state != State::Completed || Path::new(&d.path).exists();
                    (d.row(exists), Row::Panel(Item::Download(d.id)))
                }));
                if self.tabs.active_tab().is_some_and(|t| matches!(t.live, Live::Ready(_))) {
                    rows.push((
                        "Open the engine's downloads (pause, resume, retry)".to_string(),
                        Row::Panel(Item::EngineDownloads),
                    ));
                }
            }
        }
        rows
    }

    /// The field's suggestions with matching bookmarks first, and no address twice.
    pub(super) fn with_bookmark_suggestions(&self, query: &str, history: Vec<(String, Row)>) -> Vec<(String, Row)> {
        let Some(bookmarks) = &self.panels.bookmarks else { return history };
        let mut rows: Vec<(String, Row)> = bookmarks
            .suggest(query, BOOKMARK_SUGGESTIONS)
            .into_iter()
            .map(|b| (format!("★ {}", page_label(&b.title, &b.url)), Row::Go(b.url.clone())))
            .collect();
        let limit = history.len().max(rows.len());
        for row in history {
            let duplicate =
                matches!(&row.1, Row::Go(url) if rows.iter().any(|(_, r)| matches!(r, Row::Go(u) if u == url)));
            if !duplicate {
                rows.push(row);
            }
        }
        rows.truncate(limit.max(BOOKMARK_SUGGESTIONS));
        rows
    }

    /// Enter on a panel. With nothing selected, the first entry is taken.
    pub(super) fn panel_submit(&mut self, kind: PanelKind, row: Option<usize>, shift: bool) {
        let chosen = row.and_then(|r| self.rows.get(r)).cloned().or_else(|| {
            self.rows
                .iter()
                .find(|r| matches!(r, Row::Panel(Item::Url(_) | Item::Download(_) | Item::Account(_))))
                .cloned()
        });
        let Some(Row::Panel(item)) = chosen else { return };
        match item {
            Item::Url(url) => self.go(if shift { Mode::NewTab } else { Mode::Address }, &url),
            Item::Download(id) => {
                let Some(download) = self.find_download(id) else { return };
                if shift || download.state != State::Completed || !Path::new(&download.path).exists() {
                    show_in_folder(&download.path);
                } else {
                    shell_open(&download.path, None);
                }
                self.close_omnibar();
            }
            Item::ClearHistory => {
                self.panels.confirm_clear = true;
                self.refresh_panel(kind, Some(0));
            }
            Item::ConfirmClearHistory => {
                self.history.clear();
                self.save(false);
                self.panels.confirm_clear = false;
                self.refresh_panel(kind, None);
            }
            Item::EngineDownloads => {
                self.close_omnibar();
                self.with_active_web(|w| w.open_downloads());
            }
            Item::Account(_) | Item::NeverSite(_) | Item::ImportPasswords => self.password_submit(item, shift),
        }
    }

    /// Supr on a panel row: the bookmark or history entry goes; a download
    /// in progress is cancelled, a finished one leaves the list (the file stays).
    pub(super) fn panel_delete(&mut self, kind: PanelKind, row: usize) {
        let Some(Row::Panel(item)) = self.rows.get(row).cloned() else { return };
        match item {
            Item::Url(url) if kind == PanelKind::Bookmarks => {
                if let Some(bookmarks) = &mut self.panels.bookmarks
                    && bookmarks.remove(&url)
                    && bookmarks.save().is_ok()
                {
                    let items = bookmarks.items.clone();
                    self.bookmarks_changed(&items);
                }
            }
            Item::Url(url) => {
                self.history.remove(&url);
            }
            Item::Download(id) => {
                if !download_watch::cancel(id) {
                    downloads::update_file(|list| list.retain(|d| d.id != id));
                    if let Some(list) = &mut self.panels.downloads {
                        list.retain(|d| d.id != id);
                    }
                }
            }
            Item::Account(_) | Item::NeverSite(_) => return self.password_delete(item, row),
            Item::ClearHistory | Item::ConfirmClearHistory | Item::EngineDownloads | Item::ImportPasswords => return,
        }
        self.panels.notice = None;
        self.refresh_panel(kind, Some(row));
    }

    /// Rebuilds the open panel's rows, keeping the selection near `keep`.
    pub(super) fn refresh_panel(&mut self, kind: PanelKind, keep: Option<usize>) {
        let rows = self.panel_rows(kind, &self.omnibar.text());
        let count = rows.len();
        self.set_rows(rows);
        self.omnibar.select_row(keep.filter(|_| count > 0).map(|r| r.min(count - 1)));
    }

    fn find_download(&self, id: u64) -> Option<Download> {
        self.panels
            .downloads
            .as_ref()
            .and_then(|list| list.iter().find(|d| d.id == id))
            .or_else(|| self.panels.running.iter().find(|d| d.id == id))
            .cloned()
    }

    /// A download started, moved or ended. The file is written only when it
    /// starts and ends; progress stays in memory.
    pub(super) fn on_download(&mut self, download: Download) {
        let index = self.panels.running.iter().position(|d| d.id == download.id);
        let state_changed = match index {
            Some(i) if download.state == State::InProgress => {
                self.panels.running[i] = download.clone();
                false
            }
            Some(i) => {
                self.panels.running.remove(i);
                true
            }
            None => {
                if download.state == State::InProgress {
                    self.panels.running.push(download.clone());
                }
                true
            }
        };
        if state_changed {
            let saved = download.clone();
            downloads::update_file(move |list| downloads::upsert(list, saved));
        }
        if let Some(list) = &mut self.panels.downloads {
            downloads::upsert(list, download);
            if self.mode == Some(Mode::Panel(PanelKind::Downloads)) {
                let selected = self.omnibar.selected_row();
                self.refresh_panel(PanelKind::Downloads, selected);
            }
        }
    }

    /// Ctrl+Shift+B: marks the page, or unmarks it, and says so in the
    /// bookmarks panel (Esc closes it).
    pub(super) fn toggle_bookmark(&mut self) {
        let Some(tab) = self.tabs.active_tab() else { return };
        if tab.url.is_empty() || tab.url.starts_with("about:") {
            return;
        }
        let (url, title) = (tab.url.clone(), tab.title.clone());
        let mut bookmarks = Bookmarks::load();
        let marked = bookmarks.toggle(&url, &title, unix_now());
        if bookmarks.save().is_err() {
            return;
        }
        self.bookmarks_changed(&bookmarks.items);
        self.open_panel(PanelKind::Bookmarks);
        let name = if title.trim().is_empty() { omnibox::display_host(&url).to_string() } else { title };
        self.panels.notice = Some(if marked {
            format!("★ Added to bookmarks: {name}    {} removes it", self.chord_hint(Command::ToggleBookmark))
        } else {
            format!("Removed from bookmarks: {name}")
        });
        self.refresh_panel(PanelKind::Bookmarks, None);
    }
}
