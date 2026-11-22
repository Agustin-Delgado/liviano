//! Saved passwords in the app: what the page script reports turns
//! into an offer to save after a login that worked, a list of accounts
//! under a login field that was clicked, and the passwords panel
//! (Ctrl+Alt+L) in the single field. Nothing is ever filled without the
//! person choosing it.
//!
//! The UI thread never waits on Credential Manager: a read can take a fifth
//! of a second (the first one after a while, or after writes) and every
//! write does. Reads, writes and deletes go to a worker thread
//! (`vault_worker`) and come back as `Done`; an import runs on a thread of
//! its own and reports its progress in the panel. The account names are
//! kept in memory, so the list under a field opens at once; the passwords
//! of that list are read ahead while it shows, so a choice fills at once.
//!
//! The account list never takes focus: the caret stays in the page, which
//! hands it the arrow keys, Enter and Esc while it shows (`vault.js`).
//! Modal steps (the Windows identity prompt, the import menu) never run
//! inside the app's borrow: they are posted to the window and run from
//! `on_deferred`, so engine events keep flowing meanwhile.

use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Foundation::{CloseHandle, FreeLibrary, HANDLE, HMODULE, HWND, LPARAM, POINT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        Security::{
            Credentials::{
                CRED_PACK_PROTECTED_CREDENTIALS, CREDUI_INFOW, CREDUIWIN_ENUMERATE_CURRENT_USER, CREDUIWIN_FLAGS,
            },
            EqualSid, GetTokenInformation, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        System::{
            Com::CoTaskMemFree,
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
        UI::WindowsAndMessaging::{KillTimer, MB_ICONWARNING, MB_OK, MessageBoxW, PostMessageW, SetTimer, WM_APP},
    },
    core::{BOOL, HSTRING, PCSTR, PCWSTR, PWSTR, w},
};

use super::{App, Mode, Row, panel_ui::Item, with_app};
use crate::{
    autofill::{self, Dropdown, Input, Menu},
    omnibox,
    panels::PanelKind,
    shortcuts::Command,
    tabbar::Rect,
    tabs::Live,
    vault::{Account, Secret, Store, wipe},
    vault_bar::{Choice, VaultBar},
    vault_import,
    vault_watch::{self, MenuKey, Message},
    vault_worker::Worker,
};

/// Posted to the main window; `wparam` says which step to run.
pub(super) const WM_APP_VAULT: u32 = WM_APP + 20;
const DEFERRED_DONE: usize = 1;
const DEFERRED_REVEAL: usize = 2;
const DEFERRED_IMPORT: usize = 3;
const DEFERRED_SAVE_FAILED: usize = 4;
const DEFERRED_IMPORT_DONE: usize = 5;
const DEFERRED_IMPORT_STEP: usize = 6;

const TIMER_OFFER: usize = 31;
const TIMER_CLIPBOARD: usize = 32;
/// A login sent this long ago without a page after it is forgotten.
const PENDING_FOR: Duration = Duration::from_secs(180);
/// The offer goes away by itself after this.
const OFFER_MS: u32 = 60_000;
/// A copied password leaves the clipboard after this (if nothing replaced it).
const CLIPBOARD_MS: u32 = 30_000;

thread_local! {
    static CLIPBOARD_SEQUENCE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct Pending {
    tab: u64,
    origin: String,
    username: String,
    password: Secret,
    at: Instant,
}

struct Offer {
    tab: u64,
    origin: String,
    username: String,
    password: Secret,
}

/// What the worker thread did, back on the UI thread.
enum Done {
    /// Every account name, for the cache.
    Names(Vec<Account>),
    /// The passwords of a list's accounts (in its order), read ahead.
    Secrets {
        seq: u64,
        secrets: Vec<Option<Secret>>,
    },
    /// The password saved for what was just sent, to word the offer.
    OfferChecked {
        pending: Pending,
        saved: Option<Secret>,
    },
    Saved(bool),
    Copied {
        account: Account,
        secret: Option<Secret>,
    },
    Revealed {
        account: Account,
        secret: Option<Secret>,
    },
    Deleted {
        account: Account,
        deleted: bool,
        row: usize,
    },
}

/// The account list under a login field, while it shows (or while a choice
/// waits for its password).
struct Autofill {
    seq: u64,
    tab: u64,
    origin: String,
    accounts: Vec<Account>,
    menu: Menu,
    /// The field's box on screen.
    field: Rect,
    /// Where the page's corner was on screen: the list goes when it moves.
    corner: POINT,
    /// The passwords, once the worker has read them (wiped when dropped).
    secrets: Option<Vec<Option<Secret>>>,
}

/// A field clicked before the account names were in memory.
struct Asked {
    seq: u64,
    tab: u64,
    origin: String,
    field: Rect,
    corner: POINT,
    typed: String,
}

#[derive(Default)]
pub(super) struct Vault {
    store: Store,
    worker: Option<Worker<Done>>,
    /// Every account name (no password), refreshed after each change.
    names: Option<Vec<Account>>,
    /// The first read of the names is on its way.
    names_asked: bool,
    pending: Option<Pending>,
    offer: Option<Offer>,
    bar: Option<VaultBar>,
    autofill: Option<Autofill>,
    /// A chosen account whose password the worker had not read yet: filled
    /// the moment it arrives.
    choice: Option<(Autofill, usize)>,
    asked: Option<Asked>,
    dropdown: Option<Dropdown>,
    /// Counts the field clicks: what comes back for an older one is dropped.
    seq: u64,
    /// Where the import menu opens: under the field it was chosen in.
    import_at: Option<POINT>,
    reveal: Option<Account>,
}

/// The passwords panel while it is open: account names only, plus the one
/// password shown on request.
#[derive(Default)]
pub(super) struct PasswordPanel {
    accounts: Vec<Account>,
    /// The names are being read for the first time.
    loading: bool,
    confirm_delete: Option<Account>,
    shown: Option<(Account, Secret)>,
    notice: Option<String>,
}

fn post_deferred(hwnd: HWND, what: usize) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_VAULT, WPARAM(what), LPARAM(0));
    }
}

fn host_of(origin: &str) -> &str {
    omnibox::display_host(origin)
}

fn user_label(username: &str) -> &str {
    if username.is_empty() { "(no user name)" } else { username }
}

/// "ana    ejemplo.com", the way a panel row reads.
fn account_label(account: &Account) -> String {
    format!("{}    {}", user_label(&account.username), host_of(&account.origin))
}

/// The accounts of one origin among all the names, in store order.
fn accounts_of(names: &[Account], origin: &str) -> Vec<Account> {
    names.iter().filter(|a| a.origin == origin).cloned().collect()
}

/// A report of several paragraphs on one panel row.
fn one_line(text: &str) -> String {
    text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("  ")
}

fn on_dropdown(input: Input) {
    with_app(|app| app.autofill_input(input));
}

fn on_bar_choice(choice: Choice) {
    with_app(|app| app.vault_choice(choice));
}

unsafe extern "system" fn offer_expired(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| app.vault_choice(Choice::NotNow));
}

unsafe extern "system" fn clear_clipboard(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
        if GetClipboardSequenceNumber() == CLIPBOARD_SEQUENCE.with(|s| s.get()) && OpenClipboard(Some(hwnd)).is_ok() {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

fn clipboard_block(bytes: &[u8]) -> Option<HANDLE> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).ok()?;
        let target = GlobalLock(memory) as *mut u8;
        if target.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        let _ = GlobalUnlock(memory);
        Some(HANDLE(memory.0))
    }
}

/// Puts a password on the clipboard, marked so that Windows keeps it out of
/// the clipboard history and the cloud clipboard, and takes it off again
/// after a while unless something else was copied since.
fn copy_secret(hwnd: HWND, secret: &Secret) -> bool {
    const CF_UNICODETEXT: u32 = 13;
    let mut bytes: Vec<u8> =
        secret.expose().encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
    let copied = unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            wipe(&mut bytes);
            return false;
        }
        let _ = EmptyClipboard();
        let copied = clipboard_block(&bytes).is_some_and(|block| SetClipboardData(CF_UNICODETEXT, Some(block)).is_ok());
        for (name, value) in [
            (w!("ExcludeClipboardContentFromMonitorProcessing"), 0u32),
            (w!("CanIncludeInClipboardHistory"), 0),
            (w!("CanUploadToCloudClipboard"), 0),
        ] {
            let format = RegisterClipboardFormatW(name);
            if let Some(block) = clipboard_block(&value.to_le_bytes()) {
                let _ = SetClipboardData(format, Some(block));
            }
        }
        let _ = CloseClipboard();
        copied
    };
    wipe(&mut bytes);
    CLIPBOARD_SEQUENCE.with(|s| s.set(unsafe { GetClipboardSequenceNumber() }));
    unsafe {
        SetTimer(Some(hwnd), TIMER_CLIPBOARD, CLIPBOARD_MS, Some(clear_clipboard));
    }
    copied
}

/// The user of a token, as raw TOKEN_USER bytes (the SID points inside).
fn token_user(token: HANDLE) -> Option<Vec<u8>> {
    let mut size = 0u32;
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buffer = vec![0u8; size as usize];
        GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr() as *mut _), size, &mut size).ok()?;
        Some(buffer)
    }
}

type PromptFn = unsafe extern "system" fn(
    *const CREDUI_INFOW,
    u32,
    *mut u32,
    *const core::ffi::c_void,
    u32,
    *mut *mut core::ffi::c_void,
    *mut u32,
    *mut BOOL,
    CREDUIWIN_FLAGS,
) -> u32;
type UnpackFn = unsafe extern "system" fn(
    u32,
    *const core::ffi::c_void,
    u32,
    PWSTR,
    *mut u32,
    PWSTR,
    *mut u32,
    PWSTR,
    *mut u32,
) -> BOOL;

/// Asks Windows to confirm that the person at the keyboard is the signed-in
/// user, with the system's own credential prompt, and checks the answer
/// against this process's user. credui.dll is loaded only for this.
fn confirm_windows_user(owner: HWND) -> bool {
    unsafe {
        let Ok(module) = LoadLibraryExW(w!("credui.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else { return false };
        let result = confirm_with(owner, module);
        let _ = FreeLibrary(module);
        result
    }
}

unsafe fn confirm_with(owner: HWND, module: HMODULE) -> bool {
    unsafe {
        let (Some(prompt), Some(unpack)) = (
            GetProcAddress(module, PCSTR(c"CredUIPromptForWindowsCredentialsW".as_ptr() as *const u8)),
            GetProcAddress(module, PCSTR(c"CredUnPackAuthenticationBufferW".as_ptr() as *const u8)),
        ) else {
            return false;
        };
        let prompt: PromptFn = std::mem::transmute(prompt);
        let unpack: UnpackFn = std::mem::transmute(unpack);
        let caption = HSTRING::from("Nea");
        let message = HSTRING::from("Confirm your identity to show the saved password.");
        let info = CREDUI_INFOW {
            cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
            hwndParent: owner,
            pszMessageText: PCWSTR(message.as_ptr()),
            pszCaptionText: PCWSTR(caption.as_ptr()),
            ..Default::default()
        };
        let mut package = 0u32;
        let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut out_size = 0u32;
        let status = prompt(
            &info,
            0,
            &mut package,
            std::ptr::null(),
            0,
            &mut out,
            &mut out_size,
            std::ptr::null_mut(),
            CREDUIWIN_ENUMERATE_CURRENT_USER,
        );
        if status != 0 || out.is_null() {
            return false;
        }
        let mut user = [0u16; 514];
        let mut domain = [0u16; 338];
        let mut password = [0u16; 512];
        let (mut user_len, mut domain_len, mut password_len) =
            (user.len() as u32, domain.len() as u32, password.len() as u32);
        let unpacked = unpack(
            CRED_PACK_PROTECTED_CREDENTIALS.0,
            out,
            out_size,
            PWSTR(user.as_mut_ptr()),
            &mut user_len,
            PWSTR(domain.as_mut_ptr()),
            &mut domain_len,
            PWSTR(password.as_mut_ptr()),
            &mut password_len,
        )
        .as_bool();
        wipe(std::slice::from_raw_parts_mut(out as *mut u8, out_size as usize));
        CoTaskMemFree(Some(out));
        let verified = unpacked && logon_is_current_user(&user, &domain, &password);
        wipe(std::slice::from_raw_parts_mut(password.as_mut_ptr() as *mut u8, password.len() * 2));
        verified
    }
}

/// "PC\ana" or "MicrosoftAccount\ana@x.com" -> (user, domain); a UPN has none.
fn split_user(user: &str, domain: &str) -> (String, Option<String>) {
    match user.split_once('\\') {
        Some((domain, user)) => (user.to_string(), Some(domain.to_string())),
        None if !domain.is_empty() => (user.to_string(), Some(domain.to_string())),
        None => (user.to_string(), None),
    }
}

fn logon_is_current_user(user: &[u16], domain: &[u16], password: &[u16]) -> bool {
    let text = |buffer: &[u16]| {
        String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())])
    };
    let (user, domain) = split_user(&text(user), &text(domain));
    let user = HSTRING::from(user);
    let domain = domain.map(HSTRING::from);
    let mut token = HANDLE::default();
    unsafe {
        let logged = LogonUserW(
            &user,
            domain.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            PCWSTR(password.as_ptr()),
            LOGON32_LOGON_NETWORK,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        );
        if logged.is_err() {
            return false;
        }
        let mut own = HANDLE::default();
        let same = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut own).is_ok()
            && match (token_user(token), token_user(own)) {
                (Some(a), Some(b)) => {
                    let (a, b) = (&*(a.as_ptr() as *const TOKEN_USER), &*(b.as_ptr() as *const TOKEN_USER));
                    EqualSid(a.User.Sid, b.User.Sid).is_ok()
                }
                _ => false,
            };
        let _ = CloseHandle(token);
        if !own.is_invalid() {
            let _ = CloseHandle(own);
        }
        same
    }
}

/// Starts importing from `source` on a thread of its own (`main` is the
/// main window) and opens the passwords panel, which shows how far it is
/// and has the row that stops it.
pub(crate) fn start_import(main: HWND, source: vault_import::Source) {
    let Some(store) = with_app(|app| app.vault.store.clone()) else { return };
    let started = vault_import::start(
        main,
        store,
        source,
        (WM_APP_VAULT, DEFERRED_IMPORT_STEP),
        (WM_APP_VAULT, DEFERRED_IMPORT_DONE),
    );
    with_app(|app| {
        app.run(Command::Passwords);
        if !started {
            app.vault_import_notice("Nea is already importing passwords".to_string());
        }
    });
}

/// Runs a step posted by the app or its threads (see `WM_APP_VAULT`).
pub(super) fn on_deferred(hwnd: HWND, what: usize) {
    let _doing = crate::diag::doing(&"vault:step", what as u64);
    match what {
        DEFERRED_DONE => {
            with_app(|app| app.vault_done());
        }
        DEFERRED_REVEAL => {
            let Some(account) = with_app(|app| app.vault.reveal.take()).flatten() else { return };
            let confirmed = confirm_windows_user(hwnd);
            with_app(|app| app.vault_reveal(account, confirmed));
        }
        DEFERRED_IMPORT => {
            let at = with_app(|app| app.vault.import_at.take()).flatten();
            crate::diag::trace(|| "vault: import menu".to_string());
            if let Some(source) = vault_import::choose(hwnd, at) {
                crate::diag::trace(|| "vault: import chosen".to_string());
                start_import(hwnd, source);
                crate::diag::trace(|| "vault: import started".to_string());
            }
        }
        DEFERRED_IMPORT_STEP => {
            with_app(|app| app.vault_import_step());
        }
        DEFERRED_IMPORT_DONE => {
            let Some(text) = vault_import::take_result() else { return };
            let shown = with_app(|app| app.vault_import_done(&text)).unwrap_or(false);
            if !shown {
                vault_import::show_result(hwnd, &text);
            }
        }
        DEFERRED_SAVE_FAILED => unsafe {
            MessageBoxW(
                Some(hwnd),
                w!("Cannot save the password in Windows Credential Manager."),
                w!("Nea"),
                MB_OK | MB_ICONWARNING,
            );
        },
        _ => {}
    }
}

impl App {
    /// A tab whose page may use saved passwords (a private tab may not).
    fn vault_tab(&self, tab: u64) -> bool {
        self.tabs.get(tab).is_some_and(|t| matches!(&t.live, Live::Ready(web) if web.passwords))
    }

    /// Hands a Credential Manager job to the worker thread; its result
    /// comes back to `vault_done`.
    fn vault_job(&mut self, job: impl FnOnce(&Store) -> Done + Send + 'static) {
        if self.vault.worker.is_none() {
            let hwnd = self.hwnd.0 as isize;
            self.vault.worker = Worker::new("nea-vault", move || post_deferred(HWND(hwnd as _), DEFERRED_DONE));
        }
        let store = self.vault.store.clone();
        match &self.vault.worker {
            Some(worker) => worker.run(move || job(&store)),
            // No thread: the job runs here rather than not at all.
            None => {
                let done = job(&store);
                self.vault_apply(done);
            }
        }
    }

    /// Reads every account name again (after a change, or the first time).
    fn vault_refresh_names(&mut self) {
        self.vault_job(|store| Done::Names(store.accounts(None)));
    }

    fn vault_done(&mut self) {
        let results = self.vault.worker.as_ref().map(Worker::take).unwrap_or_default();
        for done in results {
            self.vault_apply(done);
        }
    }

    fn vault_apply(&mut self, done: Done) {
        match done {
            Done::Names(names) => {
                self.vault.names = Some(names.clone());
                if let Some(asked) = self.vault.asked.take() {
                    self.autofill_open(asked);
                }
                if let Some(panel) = &mut self.panels.passwords {
                    panel.loading = false;
                    panel.accounts = names;
                    self.refresh_password_panel(None);
                }
            }
            Done::Secrets { seq, secrets } => {
                if let Some(autofill) = self.vault.autofill.as_mut().filter(|a| a.seq == seq) {
                    autofill.secrets = Some(secrets);
                } else if let Some((mut autofill, index)) = self.vault.choice.take().filter(|(a, _)| a.seq == seq) {
                    autofill.secrets = Some(secrets);
                    self.autofill_fill(&autofill, index);
                }
            }
            Done::OfferChecked { pending, saved } => self.vault_offer(pending, saved),
            Done::Saved(saved) => {
                if !saved {
                    post_deferred(self.hwnd, DEFERRED_SAVE_FAILED);
                }
            }
            Done::Copied { account, secret } => {
                let text = match secret {
                    Some(secret) if copy_secret(self.hwnd, &secret) => format!(
                        "Password of {} copied. Nea clears the clipboard in {} s",
                        account_label(&account),
                        CLIPBOARD_MS / 1000
                    ),
                    Some(_) => "Cannot use the clipboard".to_string(),
                    None => "That password is not in Credential Manager now".to_string(),
                };
                self.panel_notice(text);
                self.refresh_password_panel(Some(0));
            }
            Done::Revealed { account, secret } => {
                if self.mode != Some(Mode::Panel(PanelKind::Passwords)) {
                    return;
                }
                match secret {
                    Some(secret) => {
                        if let Some(panel) = &mut self.panels.passwords {
                            panel.notice = None;
                            panel.shown = Some((account, secret));
                        }
                    }
                    None => self.panel_notice("That password is not in Credential Manager now".to_string()),
                }
                self.refresh_password_panel(Some(0));
            }
            Done::Deleted { account, deleted, row } => {
                let text = if deleted {
                    format!("Deleted the password of {}", account_label(&account))
                } else {
                    "Cannot delete it".to_string()
                };
                self.panel_notice(text);
                self.refresh_password_panel(Some(row));
            }
        }
    }

    pub(super) fn on_vault_message(&mut self, tab: u64, message: Message) {
        let _doing = crate::diag::doing(&"vault:message", tab);
        if !self.vault_tab(tab) {
            return;
        }
        match message {
            Message::Submit { origin, username, password } => {
                if self.settings.passwords_offer && !self.settings.passwords_never.contains(&origin) {
                    self.vault.pending = Some(Pending { tab, origin, username, password, at: Instant::now() });
                }
            }
            Message::Page { origin, has_password } => {
                // A page that asks for a password: the names are read now,
                // so a click on its field opens the list at once.
                if has_password && self.vault.names.is_none() && !self.vault.names_asked {
                    self.vault.names_asked = true;
                    self.vault_refresh_names();
                }
                // And the list's window is made now, not on the click.
                if has_password && self.vault.dropdown.is_none() {
                    self.vault.dropdown = Dropdown::create(self.hwnd, on_dropdown).ok();
                }
                let Some(pending) = self.vault.pending.take_if(|p| p.tab == tab) else { return };
                // The same site asking for a password again: the login failed.
                let failed = has_password && origin == pending.origin;
                if !failed && pending.at.elapsed() < PENDING_FOR {
                    self.vault_job(move |store| {
                        let account = Account { origin: pending.origin.clone(), username: pending.username.clone() };
                        let saved = store.password(&account);
                        Done::OfferChecked { pending, saved }
                    });
                }
            }
            Message::Field { origin, rect, typed } => {
                crate::diag::trace(|| "vault: field message".to_string());
                if self.tabs.active != Some(tab) || self.mode.is_some() {
                    return;
                }
                let corner = self.page_corner();
                let field = Rect::new(corner.x + rect[0], corner.y + rect[1], corner.x + rect[2], corner.y + rect[3]);
                self.vault.seq += 1;
                let asked = Asked { seq: self.vault.seq, tab, origin, field, corner, typed };
                if self.vault.names.is_some() {
                    self.autofill_open(asked);
                } else {
                    // The first field of the session: the names come first.
                    self.vault.asked = Some(asked);
                    self.vault_refresh_names();
                }
            }
            Message::Key(key) => self.autofill_key(tab, key),
            Message::Typed(typed) => {
                let Some(autofill) = self.vault.autofill.as_mut().filter(|a| a.tab == tab) else { return };
                if autofill.menu.filter(&typed) {
                    self.autofill_draw();
                } else {
                    self.autofill_close();
                }
            }
            Message::Dismiss => {
                if self.vault.autofill.as_ref().is_none_or(|a| a.tab == tab) {
                    self.autofill_close();
                }
            }
        }
    }

    // ---------- the account list under a login field ----------

    /// The page's top left corner on screen.
    fn page_corner(&self) -> POINT {
        let content = self.layout.content;
        let mut corner = POINT { x: content.left, y: content.top };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut corner);
        }
        corner
    }

    fn autofill_open(&mut self, asked: Asked) {
        if asked.seq != self.vault.seq || self.tabs.active != Some(asked.tab) || self.mode.is_some() {
            return;
        }
        let accounts = accounts_of(self.vault.names.as_deref().unwrap_or_default(), &asked.origin);
        let names = accounts.iter().map(|a| a.username.clone()).collect();
        let Some(menu) = Menu::new(names, &asked.typed) else {
            self.autofill_close();
            return;
        };
        let wanted = accounts.clone();
        let seq = asked.seq;
        self.vault.autofill = Some(Autofill {
            seq,
            tab: asked.tab,
            origin: asked.origin,
            accounts,
            menu,
            field: asked.field,
            corner: asked.corner,
            secrets: None,
        });
        if self.vault.dropdown.is_none() {
            self.vault.dropdown = Dropdown::create(self.hwnd, on_dropdown).ok();
        }
        self.autofill_draw();
        crate::diag::trace(|| "vault: list shown".to_string());
        // Read ahead: a choice then fills without waiting on Credential Manager.
        self.vault_job(move |store| Done::Secrets {
            seq,
            secrets: wanted.iter().map(|account| store.password(account)).collect(),
        });
    }

    /// Draws the list as the model says and tells the page what it shows.
    fn autofill_draw(&self) {
        let (Some(autofill), Some(dropdown)) = (&self.vault.autofill, &self.vault.dropdown) else { return };
        let header = format!("Passwords for {}", host_of(&autofill.origin));
        dropdown.show(&autofill.menu, &header, autofill.field, &self.palette, self.scale);
        self.autofill_tell_page(autofill.tab, true, autofill.menu.picked().is_some());
    }

    fn autofill_tell_page(&self, tab: u64, open: bool, picked: bool) {
        if let Some(Live::Ready(web)) = self.tabs.get(tab).map(|t| &t.live) {
            vault_watch::set_menu(&web.webview, open, picked);
        }
    }

    /// Closes the list (if it shows) and forgets a lookup on its way.
    pub(super) fn autofill_close(&mut self) {
        self.vault.seq += 1;
        self.vault.asked = None;
        let Some(autofill) = self.vault.autofill.take() else { return };
        if let Some(dropdown) = &self.vault.dropdown {
            dropdown.hide();
        }
        self.autofill_tell_page(autofill.tab, false, false);
    }

    /// The list goes when its page is no longer where it was: another tab,
    /// the window moved or resized, the page moved in it.
    pub(super) fn autofill_follow(&mut self) {
        let Some(autofill) = &self.vault.autofill else { return };
        let corner = self.page_corner();
        if self.tabs.active != Some(autofill.tab) || (corner.x, corner.y) != (autofill.corner.x, autofill.corner.y) {
            self.autofill_close();
        }
    }

    fn autofill_input(&mut self, input: Input) {
        match input {
            Input::Click(row) => self.autofill_choose(row),
            Input::Wheel(lines) => {
                if let Some(autofill) = &mut self.vault.autofill {
                    autofill.menu.scroll(lines);
                    self.autofill_draw();
                }
            }
        }
    }

    fn autofill_key(&mut self, tab: u64, key: MenuKey) {
        let Some(autofill) = self.vault.autofill.as_mut().filter(|a| a.tab == tab) else { return };
        match key {
            MenuKey::Down | MenuKey::Up => {
                autofill.menu.step(key == MenuKey::Down);
                self.autofill_draw();
            }
            MenuKey::Enter => {
                if let Some(row) = autofill.menu.picked() {
                    self.autofill_choose(row);
                }
            }
            MenuKey::Escape => self.autofill_close(),
        }
    }

    /// A row was chosen (clicked, or Enter): the form is filled first, the
    /// list's fade after.
    fn autofill_choose(&mut self, row: usize) {
        let Some(autofill) = self.vault.autofill.take() else { return };
        crate::diag::trace(|| format!("vault: chosen {row}"));
        let chosen = autofill.menu.rows().get(row).copied();
        let tab = autofill.tab;
        match chosen {
            Some(autofill::Row::Account(index)) if autofill.secrets.is_some() => self.autofill_fill(&autofill, index),
            // Chosen before the read ahead came back: it fills on arrival.
            Some(autofill::Row::Account(index)) => self.vault.choice = Some((autofill, index)),
            Some(autofill::Row::Manage) => {}
            None => {
                self.vault.autofill = Some(autofill);
                return;
            }
        }
        if let Some(dropdown) = &self.vault.dropdown {
            dropdown.hide();
        }
        self.autofill_tell_page(tab, false, false);
        if chosen == Some(autofill::Row::Manage) {
            self.run(Command::Passwords);
        }
    }

    fn autofill_fill(&self, autofill: &Autofill, index: usize) {
        let (Some(account), Some(Some(password))) =
            (autofill.accounts.get(index), autofill.secrets.as_ref().and_then(|s| s.get(index)))
        else {
            return;
        };
        if self.tabs.active != Some(autofill.tab) {
            return;
        }
        let Some(tab) = self.tabs.get(autofill.tab) else { return };
        if let Live::Ready(web) = &tab.live {
            // The page may have moved on while the list was open.
            if crate::vault::origin_of(&web.source()).as_deref() == Some(autofill.origin.as_str()) {
                vault_watch::fill(&web.webview, &autofill.origin, &account.username, password);
                crate::diag::trace(|| "vault: fill sent".to_string());
            }
        }
    }

    // ---------- the offer to save ----------

    /// Offers to save (or update) what was sent, unless it is saved already.
    fn vault_offer(&mut self, pending: Pending, saved: Option<Secret>) {
        if saved.as_ref() == Some(&pending.password) {
            return;
        }
        let host = host_of(&pending.origin).to_string();
        let question = match (&saved, pending.username.is_empty()) {
            (Some(_), _) => {
                format!("Update the saved password of {} for {host}?", user_label(&pending.username))
            }
            (None, true) => format!("Save the password for {host}?"),
            (None, false) => format!("Save the password of {} for {host}?", pending.username),
        };
        let save = if saved.is_some() { "Update" } else { "Save" };
        self.vault.bar = None;
        self.vault.bar = VaultBar::create(
            self.hwnd,
            &self.palette,
            self.font,
            self.scale,
            [&question, save, "Never for this site", "Not now"],
            on_bar_choice,
        );
        self.vault.offer = Some(Offer {
            tab: pending.tab,
            origin: pending.origin,
            username: pending.username,
            password: pending.password,
        });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_OFFER, OFFER_MS, Some(offer_expired));
        }
        self.place_vault_bar();
    }

    /// The theme or the DPI scale changed: the offer and the account list
    /// that are open take the new look.
    pub(super) fn restyle_vault(&mut self) {
        let (palette, font, scale) = (self.palette, self.font, self.scale);
        if let Some(bar) = &mut self.vault.bar {
            bar.restyle(&palette, font, scale);
        }
        self.place_vault_bar();
        self.autofill_draw();
    }

    /// Keeps the offer at the page's top right corner, shown only over its tab.
    pub(super) fn place_vault_bar(&self) {
        let (Some(offer), Some(bar)) = (&self.vault.offer, &self.vault.bar) else { return };
        if self.tabs.active != Some(offer.tab) {
            bar.hide();
            return;
        }
        let content = self.layout.content;
        let margin = (8.0 * self.scale).round() as i32;
        let mut corner = POINT { x: content.right - margin, y: content.top + margin };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut corner);
        }
        bar.place(corner.x, corner.y);
    }

    fn vault_choice(&mut self, choice: Choice) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_OFFER);
        }
        self.vault.bar = None;
        let Some(offer) = self.vault.offer.take() else { return };
        match choice {
            Choice::Save => {
                self.vault_job(move |store| {
                    Done::Saved(store.save(&offer.origin, &offer.username, offer.password.expose()).is_ok())
                });
                self.vault_refresh_names();
            }
            Choice::Never => {
                self.settings.passwords_never.insert(offer.origin);
                self.save_settings();
                crate::settings_window::sync(&self.settings);
            }
            Choice::NotNow => {}
        }
        self.focus_page();
    }

    // ---------- the passwords panel ----------

    /// Opens with the names in memory (read again behind it): no wait.
    pub(super) fn password_panel_load(&mut self) -> PasswordPanel {
        let names = self.vault.names.clone();
        self.vault_refresh_names();
        PasswordPanel { loading: names.is_none(), accounts: names.unwrap_or_default(), ..Default::default() }
    }

    pub(super) fn password_rows(&self, query: &str) -> Vec<(String, Row)> {
        let mut rows = Vec::new();
        let Some(panel) = &self.panels.passwords else { return rows };
        let info = |text: String| (text, Row::Info);
        let importing = vault_import::progress();
        if let Some(progress) = &importing {
            rows.push(info(progress.line()));
        }
        if let Some((account, secret)) = &panel.shown {
            rows.push(info(format!("{}: {}    hidden when the panel closes", account_label(account), secret.expose())));
        } else if let Some(notice) = &panel.notice {
            rows.push(info(notice.clone()));
        }
        let found: Vec<&Account> = panel
            .accounts
            .iter()
            .filter(|a| crate::panels::matches(query, &[&a.username, host_of(&a.origin)]))
            .collect();
        if panel.loading {
            rows.push(info("Reading the saved passwords…".to_string()));
        } else if panel.accounts.is_empty() {
            rows.push(info("No saved passwords yet. Nea offers to save a password when you sign in.".to_string()));
        } else if found.is_empty() {
            rows.push(info("No results".to_string()));
        }
        rows.extend(found.into_iter().map(|a| (account_label(a), Row::Panel(Item::Account(a.clone())))));
        rows.extend(
            self.settings
                .passwords_never
                .iter()
                .filter(|origin| crate::panels::matches(query, &[host_of(origin)]))
                .map(|origin| {
                    (
                        format!(
                            "Nea does not offer to save passwords on {}    Delete offers it again",
                            host_of(origin)
                        ),
                        Row::Panel(Item::NeverSite(origin.clone())),
                    )
                }),
        );
        let import =
            if importing.is_some() { "Stop the import" } else { "Import passwords from another browser or a CSV…" };
        rows.push((import.to_string(), Row::Panel(Item::ImportPasswords)));
        rows
    }

    fn panel_notice(&mut self, text: String) {
        if let Some(panel) = &mut self.panels.passwords {
            panel.shown = None;
            panel.notice = Some(text);
        }
    }

    pub(super) fn password_submit(&mut self, item: Item, shift: bool) {
        match item {
            Item::Account(account) if shift => {
                self.vault.reveal = Some(account);
                post_deferred(self.hwnd, DEFERRED_REVEAL);
            }
            Item::Account(account) => {
                self.vault_job(move |store| {
                    let secret = store.password(&account);
                    Done::Copied { account, secret }
                });
            }
            Item::ImportPasswords if vault_import::running() => {
                vault_import::stop();
                self.panel_notice("Stopping the import after the password it is saving…".to_string());
                self.refresh_password_panel(None);
            }
            Item::ImportPasswords => {
                crate::diag::trace(|| "vault: import row".to_string());
                // The field closes first: the menu and the file dialog never
                // fight it for the keyboard.
                self.vault.import_at = Some(self.omnibar.menu_anchor());
                self.close_omnibar();
                post_deferred(self.hwnd, DEFERRED_IMPORT);
            }
            _ => {}
        }
    }

    /// Supr: a password is deleted on the second press; a "never" site goes
    /// back to being offered.
    pub(super) fn password_delete(&mut self, item: Item, row: usize) {
        match item {
            Item::Account(account) => {
                let confirmed =
                    self.panels.passwords.as_ref().is_some_and(|p| p.confirm_delete.as_ref() == Some(&account));
                if confirmed {
                    // Off the list at once; the worker deletes it and says so.
                    if let Some(panel) = &mut self.panels.passwords {
                        panel.confirm_delete = None;
                        panel.accounts.retain(|a| *a != account);
                    }
                    if let Some(names) = &mut self.vault.names {
                        names.retain(|a| *a != account);
                    }
                    self.vault_job(move |store| {
                        let deleted = store.delete(&account);
                        Done::Deleted { account, deleted, row }
                    });
                    self.vault_refresh_names();
                    self.refresh_password_panel(Some(row));
                    return;
                }
                let text =
                    format!("Press Delete again to delete the password of {} · Esc cancels", account_label(&account));
                self.panel_notice(text);
                if let Some(panel) = &mut self.panels.passwords {
                    panel.confirm_delete = Some(account);
                }
                self.refresh_password_panel(Some(0));
            }
            Item::NeverSite(origin) => {
                self.settings.passwords_never.remove(&origin);
                self.save_settings();
                crate::settings_window::sync(&self.settings);
                self.panel_notice(format!("Nea offers to save passwords on {} again", host_of(&origin)));
                self.refresh_password_panel(Some(row));
            }
            _ => {}
        }
    }

    fn vault_reveal(&mut self, account: Account, confirmed: bool) {
        if self.mode != Some(Mode::Panel(PanelKind::Passwords)) {
            return;
        }
        if confirmed {
            self.vault_job(move |store| {
                let secret = store.password(&account);
                Done::Revealed { account, secret }
            });
        } else {
            self.panel_notice(
                "Windows did not confirm your identity. The password stays hidden (Enter copies it)".to_string(),
            );
            self.refresh_password_panel(Some(0));
        }
        self.restore_focus();
    }

    // ---------- importing ----------

    fn vault_import_notice(&mut self, text: String) {
        self.panel_notice(text);
        if self.mode == Some(Mode::Panel(PanelKind::Passwords)) {
            self.refresh_password_panel(None);
        }
    }

    /// The import moved on: the panel's first row says how far.
    fn vault_import_step(&mut self) {
        if self.mode == Some(Mode::Panel(PanelKind::Passwords)) {
            self.refresh_password_panel(None);
        }
    }

    /// The import ended: the names are read again and the panel, when it is
    /// open, says what came in. False when it is not (a message says it).
    fn vault_import_done(&mut self, text: &str) -> bool {
        self.vault_refresh_names();
        if self.mode != Some(Mode::Panel(PanelKind::Passwords)) {
            return false;
        }
        self.panel_notice(one_line(text));
        self.refresh_password_panel(None);
        true
    }

    fn refresh_password_panel(&mut self, keep: Option<usize>) {
        self.refresh_panel(PanelKind::Passwords, keep);
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/vault_ui.rs"]
#[rustfmt::skip]
mod tests;
