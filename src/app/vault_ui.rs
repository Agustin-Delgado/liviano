//! Saved passwords in the app (RF-14): what the page script reports turns
//! into an offer to save after a login that worked, a menu of accounts when
//! a login field is clicked, and the passwords panel (Ctrl+Alt+L) in the
//! single field. Nothing is ever filled without the person choosing it.
//!
//! Modal steps (the account menu, the Windows identity prompt, the import
//! menu) never run inside the app's borrow: they are posted to the window
//! and run from `on_deferred`, so engine events keep flowing meanwhile.

use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Foundation::{CloseHandle, FreeLibrary, HANDLE, HMODULE, HWND, LPARAM, POINT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        Security::{
            Credentials::{CRED_PACK_PROTECTED_CREDENTIALS, CREDUI_INFOW, CREDUIWIN_ENUMERATE_CURRENT_USER, CREDUIWIN_FLAGS},
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
        UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, KillTimer, MB_ICONWARNING, MB_OK, MF_GRAYED, MF_SEPARATOR, MF_STRING,
            MessageBoxW, PostMessageW, SetTimer, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, TrackPopupMenu, WM_APP,
        },
    },
    core::{BOOL, HSTRING, PCSTR, PCWSTR, PWSTR, w},
};

use super::{App, Mode, Row, panel_ui::Item, with_app};
use crate::{
    omnibox,
    panels::PanelKind,
    shortcuts::Command,
    tabs::Live,
    vault::{Account, Secret, Store, wipe},
    vault_bar::{Choice, VaultBar},
    vault_import,
    vault_watch::{self, Message},
};

/// Posted to the main window; `wparam` says which modal step to run.
pub(super) const WM_APP_VAULT: u32 = WM_APP + 20;
const DEFERRED_MENU: usize = 1;
const DEFERRED_REVEAL: usize = 2;
const DEFERRED_IMPORT: usize = 3;
const DEFERRED_SAVE_FAILED: usize = 4;

const TIMER_OFFER: usize = 31;
const TIMER_CLIPBOARD: usize = 32;
/// A login sent this long ago without a page after it is forgotten.
const PENDING_FOR: Duration = Duration::from_secs(180);
/// The offer goes away by itself after this.
const OFFER_MS: u32 = 60_000;
/// A copied password leaves the clipboard after this (if nothing replaced it).
const CLIPBOARD_MS: u32 = 30_000;
const MENU_MANAGE: usize = 1000;

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

struct MenuRequest {
    tab: u64,
    origin: String,
    accounts: Vec<Account>,
    at: POINT,
}

#[derive(Default)]
pub(super) struct Vault {
    store: Store,
    pending: Option<Pending>,
    offer: Option<Offer>,
    bar: Option<VaultBar>,
    menu: Option<MenuRequest>,
    reveal: Option<Account>,
}

/// The passwords panel while it is open: account names only, plus the one
/// password shown on request.
#[derive(Default)]
pub(super) struct PasswordPanel {
    accounts: Vec<Account>,
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
    if username.is_empty() { "(sin usuario)" } else { username }
}

/// "ana    ejemplo.com", the way a panel row reads.
fn account_label(account: &Account) -> String {
    format!("{}    {}", user_label(&account.username), host_of(&account.origin))
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
    let mut bytes: Vec<u8> = secret.expose().encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
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
type UnpackFn =
    unsafe extern "system" fn(u32, *const core::ffi::c_void, u32, PWSTR, *mut u32, PWSTR, *mut u32, PWSTR, *mut u32) -> BOOL;

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
        let caption = HSTRING::from("Liviano");
        let message = HSTRING::from("Confirmá que sos vos para ver la contraseña guardada.");
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
        let (mut user_len, mut domain_len, mut password_len) = (user.len() as u32, domain.len() as u32, password.len() as u32);
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

/// "EQUIPO\ana" or "MicrosoftAccount\ana@x.com" -> (user, domain); a UPN has none.
fn split_user(user: &str, domain: &str) -> (String, Option<String>) {
    match user.split_once('\\') {
        Some((domain, user)) => (user.to_string(), Some(domain.to_string())),
        None if !domain.is_empty() => (user.to_string(), Some(domain.to_string())),
        None => (user.to_string(), None),
    }
}

fn logon_is_current_user(user: &[u16], domain: &[u16], password: &[u16]) -> bool {
    let text = |buffer: &[u16]| String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())]);
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

/// Runs a modal step posted by the app (see `WM_APP_VAULT`).
pub(super) fn on_deferred(hwnd: HWND, what: usize) {
    match what {
        DEFERRED_MENU => {
            let Some(request) = with_app(|app| app.vault.menu.take()).flatten() else { return };
            let chosen = account_menu(hwnd, &request);
            with_app(|app| app.vault_menu_chosen(request, chosen));
        }
        DEFERRED_REVEAL => {
            let Some(account) = with_app(|app| app.vault.reveal.take()).flatten() else { return };
            let confirmed = confirm_windows_user(hwnd);
            with_app(|app| app.vault_reveal(account, confirmed));
        }
        DEFERRED_IMPORT => {
            // Under the field the panel is drawn in, not wherever the mouse is.
            let at = with_app(|app| {
                let (x, y, _) = app.omnibar_anchor();
                POINT { x, y: y + (40.0 * app.scale).round() as i32 }
            });
            vault_import::run(hwnd, at);
            with_app(|app| app.vault_reload_panel());
        }
        DEFERRED_SAVE_FAILED => unsafe {
            MessageBoxW(
                Some(hwnd),
                w!("No se pudo guardar la contraseña en el Administrador de credenciales de Windows."),
                w!("Liviano"),
                MB_OK | MB_ICONWARNING,
            );
        },
        _ => {}
    }
}

/// The accounts of the site, beside the field that was clicked.
fn account_menu(hwnd: HWND, request: &MenuRequest) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(format!("Contraseñas de {}", host_of(&request.origin))));
        for (i, account) in request.accounts.iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING, i + 1, &HSTRING::from(user_label(&account.username)));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_MANAGE, w!("Administrar contraseñas…"));
        let chosen =
            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, request.at.x, request.at.y, None, hwnd, None).0
                as usize;
        let _ = DestroyMenu(menu);
        chosen
    }
}

impl App {
    /// A tab whose page may use saved passwords (a private tab may not).
    fn vault_tab(&self, tab: u64) -> bool {
        self.tabs.get(tab).is_some_and(|t| matches!(&t.live, Live::Ready(web) if web.passwords))
    }

    pub(super) fn on_vault_message(&mut self, tab: u64, message: Message) {
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
                let Some(pending) = self.vault.pending.take_if(|p| p.tab == tab) else { return };
                // The same site asking for a password again: the login failed.
                let failed = has_password && origin == pending.origin;
                if !failed && pending.at.elapsed() < PENDING_FOR {
                    self.vault_offer(pending);
                }
            }
            Message::Field { origin, left, bottom } => {
                if self.tabs.active != Some(tab) || self.vault.menu.is_some() || self.mode.is_some() {
                    return;
                }
                let accounts = self.vault.store.accounts(Some(&origin));
                if accounts.is_empty() {
                    return;
                }
                let content = self.layout.content;
                let mut at = POINT { x: content.left + left, y: content.top + bottom };
                unsafe {
                    let _ = ClientToScreen(self.hwnd, &mut at);
                }
                self.vault.menu = Some(MenuRequest { tab, origin, accounts, at });
                post_deferred(self.hwnd, DEFERRED_MENU);
            }
        }
    }

    /// Offers to save (or update) what was sent, unless it is saved already.
    fn vault_offer(&mut self, pending: Pending) {
        let account = Account { origin: pending.origin.clone(), username: pending.username.clone() };
        let saved = self.vault.store.password(&account);
        if saved.as_ref() == Some(&pending.password) {
            return;
        }
        let host = host_of(&pending.origin).to_string();
        let question = match (&saved, pending.username.is_empty()) {
            (Some(_), _) => format!("¿Actualizar la contraseña guardada de {} para {host}?", user_label(&pending.username)),
            (None, true) => format!("¿Guardar la contraseña para {host}?"),
            (None, false) => format!("¿Guardar la contraseña de {} para {host}?", pending.username),
        };
        let save = if saved.is_some() { "Actualizar" } else { "Guardar" };
        self.vault.bar = None;
        self.vault.bar =
            VaultBar::create(self.hwnd, &self.palette, self.font, self.scale, [&question, save, "Nunca para este sitio", "Ahora no"], on_bar_choice);
        self.vault.offer = Some(Offer { tab: pending.tab, origin: pending.origin, username: pending.username, password: pending.password });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_OFFER, OFFER_MS, Some(offer_expired));
        }
        self.place_vault_bar();
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
                if self.vault.store.save(&offer.origin, &offer.username, offer.password.expose()).is_err() {
                    post_deferred(self.hwnd, DEFERRED_SAVE_FAILED);
                }
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

    fn vault_menu_chosen(&mut self, request: MenuRequest, chosen: usize) {
        if chosen == MENU_MANAGE {
            self.run(Command::Passwords);
            return;
        }
        let Some(account) = chosen.checked_sub(1).and_then(|i| request.accounts.get(i)) else {
            self.focus_page();
            return;
        };
        if self.tabs.active != Some(request.tab) {
            return;
        }
        let Some(password) = self.vault.store.password(account) else { return };
        self.focus_page();
        let Some(tab) = self.tabs.get(request.tab) else { return };
        if let Live::Ready(web) = &tab.live {
            // The page may have moved on while the menu was open.
            if crate::vault::origin_of(&web.source()).as_deref() == Some(request.origin.as_str()) {
                vault_watch::fill(&web.webview, &request.origin, &account.username, &password);
            }
        }
    }

    // ---------- the passwords panel ----------

    pub(super) fn password_panel_load(&mut self) -> PasswordPanel {
        PasswordPanel { accounts: self.vault.store.accounts(None), ..Default::default() }
    }

    pub(super) fn password_rows(&self, query: &str) -> Vec<(String, Row)> {
        let mut rows = Vec::new();
        let Some(panel) = &self.panels.passwords else { return rows };
        let info = |text: String| (text, Row::Info);
        if let Some((account, secret)) = &panel.shown {
            rows.push(info(format!("{}: {}    se oculta al cerrar el panel", account_label(account), secret.expose())));
        } else if let Some(notice) = &panel.notice {
            rows.push(info(notice.clone()));
        }
        let found: Vec<&Account> = panel
            .accounts
            .iter()
            .filter(|a| crate::panels::matches(query, &[&a.username, host_of(&a.origin)]))
            .collect();
        if panel.accounts.is_empty() {
            rows.push(info("Todavía no hay contraseñas guardadas: se ofrece guardarlas al iniciar sesión.".to_string()));
        } else if found.is_empty() {
            rows.push(info("Sin resultados".to_string()));
        }
        rows.extend(found.into_iter().map(|a| (account_label(a), Row::Panel(Item::Account(a.clone())))));
        rows.extend(
            self.settings
                .passwords_never
                .iter()
                .filter(|origin| crate::panels::matches(query, &[host_of(origin)]))
                .map(|origin| {
                    (format!("No se ofrece guardar en {}    Supr vuelve a ofrecerlo", host_of(origin)), Row::Panel(Item::NeverSite(origin.clone())))
                }),
        );
        rows.push(("Importar contraseñas de otro navegador o de un CSV…".to_string(), Row::Panel(Item::ImportPasswords)));
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
                let text = match self.vault.store.password(&account) {
                    Some(secret) if copy_secret(self.hwnd, &secret) => format!(
                        "Contraseña de {} copiada: se borra del portapapeles en {} s",
                        account_label(&account),
                        CLIPBOARD_MS / 1000
                    ),
                    Some(_) => "No se pudo usar el portapapeles".to_string(),
                    None => "Esa contraseña ya no está en el Administrador de credenciales".to_string(),
                };
                self.panel_notice(text);
                self.refresh_password_panel(Some(0));
            }
            Item::ImportPasswords => post_deferred(self.hwnd, DEFERRED_IMPORT),
            _ => {}
        }
    }

    /// Supr: a password is deleted on the second press; a "never" site goes
    /// back to being offered.
    pub(super) fn password_delete(&mut self, item: Item, row: usize) {
        match item {
            Item::Account(account) => {
                let confirmed = self.panels.passwords.as_ref().is_some_and(|p| p.confirm_delete.as_ref() == Some(&account));
                let text = if confirmed {
                    if self.vault.store.delete(&account) {
                        format!("Se borró la contraseña de {}", account_label(&account))
                    } else {
                        "No se pudo borrar".to_string()
                    }
                } else {
                    format!("Supr otra vez borra la contraseña de {} · Esc cancela", account_label(&account))
                };
                let accounts = confirmed.then(|| self.vault.store.accounts(None));
                self.panel_notice(text);
                if let Some(panel) = &mut self.panels.passwords {
                    panel.confirm_delete = (!confirmed).then_some(account);
                    if let Some(accounts) = accounts {
                        panel.accounts = accounts;
                    }
                }
                self.refresh_password_panel(Some(if confirmed { row } else { 0 }));
            }
            Item::NeverSite(origin) => {
                self.settings.passwords_never.remove(&origin);
                self.save_settings();
                crate::settings_window::sync(&self.settings);
                self.panel_notice(format!("Se vuelve a ofrecer guardar contraseñas en {}", host_of(&origin)));
                self.refresh_password_panel(Some(row));
            }
            _ => {}
        }
    }

    fn vault_reveal(&mut self, account: Account, confirmed: bool) {
        if self.mode != Some(Mode::Panel(PanelKind::Passwords)) {
            return;
        }
        let secret = if confirmed { self.vault.store.password(&account) } else { None };
        match secret {
            Some(secret) => {
                if let Some(panel) = &mut self.panels.passwords {
                    panel.notice = None;
                    panel.shown = Some((account, secret));
                }
            }
            None if confirmed => self.panel_notice("Esa contraseña ya no está en el Administrador de credenciales".to_string()),
            None => self.panel_notice("Windows no confirmó tu identidad: la contraseña sigue oculta (Enter la copia)".to_string()),
        }
        self.refresh_password_panel(Some(0));
        self.restore_focus();
    }

    /// After an import: the open panel lists what came in.
    fn vault_reload_panel(&mut self) {
        if self.mode != Some(Mode::Panel(PanelKind::Passwords)) {
            return;
        }
        let accounts = self.vault.store.accounts(None);
        if let Some(panel) = &mut self.panels.passwords {
            panel.accounts = accounts;
        }
        self.refresh_password_panel(None);
        self.restore_focus();
    }

    fn refresh_password_panel(&mut self, keep: Option<usize>) {
        self.refresh_panel(PanelKind::Passwords, keep);
    }
}

