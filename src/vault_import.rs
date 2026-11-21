//! Importing passwords from the Chromium browsers installed for this
//! Windows user, or from a CSV exported by one. Everything happens on this
//! machine: the profile's `Login Data` is copied to a temp file (the browser
//! keeps it locked), read, decrypted with the profile key and deleted, and
//! each password goes straight into Credential Manager.

use std::path::{Path, PathBuf};

use windows::{
    Win32::{
        Foundation::{HWND, POINT},
        UI::{
            Controls::Dialogs::{OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW},
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MB_ICONINFORMATION, MB_OK, MF_GRAYED,
                MF_SEPARATOR, MF_STRING, MessageBoxW, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
            },
        },
    },
    core::{BOOL, HSTRING, PCWSTR, PWSTR, w},
};

use crate::{
    sqlite::Sqlite,
    system_dll::Library,
    vault::{self, Secret, Store},
    vault_crypto::{self, Crypto, Scheme, SecretBytes},
};

pub struct Login {
    pub origin: String,
    pub username: String,
    pub password: Secret,
}

/// A browser found on this machine: its name and its "User Data" folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    pub name: &'static str,
    pub user_data: PathBuf,
}

/// What one browser gave.
#[derive(Default)]
pub struct Outcome {
    pub name: String,
    pub logins: Vec<Login>,
    /// Values under Chrome's app-bound encryption ("v20").
    pub app_bound: usize,
    /// Values that didn't decrypt (a profile key that didn't open, a damaged value).
    pub failed: usize,
    /// The profile key or its database couldn't be read at all.
    pub unreadable: bool,
}

/// Where each browser keeps its profiles, under %LOCALAPPDATA% or %APPDATA%.
const LOCAL_BROWSERS: &[(&str, &str)] = &[
    ("Google Chrome", r"Google\Chrome\User Data"),
    ("Microsoft Edge", r"Microsoft\Edge\User Data"),
    ("Brave", r"BraveSoftware\Brave-Browser\User Data"),
    ("Vivaldi", r"Vivaldi\User Data"),
    ("Chromium", r"Chromium\User Data"),
];
const ROAMING_BROWSERS: &[(&str, &str)] =
    &[("Opera", r"Opera Software\Opera Stable"), ("Opera GX", r"Opera Software\Opera GX Stable")];
/// Arc for Windows is a Store package: its folder carries a publisher hash.
const ARC_PACKAGE: &str = "TheBrowserCompany.Arc_";
const ARC_USER_DATA: &str = r"LocalCache\Local\Arc\User Data";

/// The browsers whose `Local State` exists. Nothing is read here but that.
pub fn browsers() -> Vec<Browser> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
    browsers_in(local.as_deref(), roaming.as_deref())
}

fn browsers_in(local: Option<&Path>, roaming: Option<&Path>) -> Vec<Browser> {
    let mut found = Vec::new();
    let mut add = |name: &'static str, user_data: PathBuf| {
        if user_data.join("Local State").is_file() {
            found.push(Browser { name, user_data });
        }
    };
    if let Some(local) = local {
        for (name, relative) in LOCAL_BROWSERS {
            add(name, local.join(relative));
        }
        if let Ok(packages) = std::fs::read_dir(local.join("Packages")) {
            for package in packages.flatten() {
                if package.file_name().to_string_lossy().starts_with(ARC_PACKAGE) {
                    add("Arc", package.path().join(ARC_USER_DATA));
                }
            }
        }
    }
    if let Some(roaming) = roaming {
        for (name, relative) in ROAMING_BROWSERS {
            add(name, roaming.join(relative));
        }
    }
    found
}

/// The password databases of every profile: "Default", "Profile 2"… (Opera
/// keeps its only one in the folder itself), with the per-account store
/// newer Chrome versions add beside the local one.
fn login_files(user_data: &Path) -> Vec<PathBuf> {
    let mut folders = vec![user_data.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(user_data) {
        let mut profiles: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name == "Default" || name.starts_with("Profile ")
            })
            .map(|e| e.path())
            .collect();
        profiles.sort();
        folders.extend(profiles);
    }
    folders
        .into_iter()
        .flat_map(|folder| ["Login Data", "Login Data For Account"].map(|file| folder.join(file)))
        .filter(|path| path.is_file())
        .collect()
}

/// The profile key, from `os_crypt.encrypted_key` in Local State.
fn local_state_key(crypto: &Crypto, user_data: &Path) -> Option<SecretBytes> {
    let text = std::fs::read_to_string(user_data.join("Local State")).ok()?;
    let state: serde_json::Value = serde_json::from_str(&text).ok()?;
    crypto.profile_key(state.get("os_crypt")?.get("encrypted_key")?.as_str()?)
}

pub fn read_browser(browser: &Browser) -> Outcome {
    let mut outcome = Outcome { name: browser.name.to_string(), ..Default::default() };
    let (Some(sqlite), Some(crypto)) = (Sqlite::load(), Crypto::load()) else {
        outcome.unreadable = true;
        return outcome;
    };
    let key = local_state_key(&crypto, &browser.user_data);
    let files = login_files(&browser.user_data);
    if files.is_empty() {
        outcome.unreadable = key.is_none();
    }
    for file in files {
        if !read_login_file(&sqlite, &crypto, &file, key.as_ref(), &mut outcome) {
            outcome.unreadable = true;
        }
    }
    outcome
}

fn temp_copy_path() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("nea-import-{}-{n}.db", std::process::id()))
}

/// Reads one `Login Data` through a temp copy. False when it can't be read.
fn read_login_file(
    sqlite: &Sqlite,
    crypto: &Crypto,
    file: &Path,
    key: Option<&SecretBytes>,
    outcome: &mut Outcome,
) -> bool {
    let copy = temp_copy_path();
    if std::fs::copy(file, &copy).is_err() {
        return false;
    }
    let read = sqlite.open_read_only(&copy).and_then(|db| {
        let mut rows = Vec::new();
        db.query(
            "SELECT origin_url, signon_realm, username_value, password_value, blacklisted_by_user FROM logins",
            |row| rows.push((row.text(0), row.text(1), row.text(2), SecretBytes(row.blob(3)), row.int(4) != 0)),
        )
        .or_else(|| {
            db.query("SELECT origin_url, signon_realm, username_value, password_value FROM logins", |row| {
                rows.push((row.text(0), row.text(1), row.text(2), SecretBytes(row.blob(3)), false))
            })
        })
        .map(|_| rows)
    });
    let _ = std::fs::remove_file(&copy);
    let Some(rows) = read else { return false };
    for (origin_url, realm, username, value, never) in rows {
        if never {
            continue;
        }
        let Some(origin) = vault::origin_of(&origin_url).or_else(|| vault::origin_of(&realm)) else { continue };
        let plain = match vault_crypto::scheme_of(&value.0) {
            Scheme::Empty => continue,
            Scheme::AppBound => {
                outcome.app_bound += 1;
                continue;
            }
            Scheme::ProfileKey => key.and_then(|k| crypto.decrypt_v10(&k.0, &value.0)),
            Scheme::Dpapi => crypto.dpapi_unprotect(&value.0),
        };
        match plain.and_then(|p| String::from_utf8(p.0.clone()).ok()) {
            Some(text) if !text.is_empty() => {
                outcome.logins.push(Login { origin, username, password: Secret::new(text) })
            }
            Some(_) => {}
            None => outcome.failed += 1,
        }
    }
    true
}

/// RFC 4180: commas, quoted fields with "" for a quote, line breaks inside
/// quotes, CRLF or LF. Every cell is a `Secret`: the passwords are among them.
pub fn parse_csv(text: &str) -> Vec<Vec<Secret>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row: Vec<Secret> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(Secret::new(std::mem::take(&mut field))),
            '\r' => {}
            '\n' => {
                row.push(Secret::new(std::mem::take(&mut field)));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(Secret::new(field));
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

/// The logins of an exported CSV (Chrome and Edge write name,url,username,
/// password,note; Firefox and others name their columns alike), and how many
/// rows were skipped. None when the header has no address or password column.
pub fn logins_from_csv(text: &str) -> Option<(Vec<Login>, usize)> {
    let rows = parse_csv(text);
    let header: Vec<String> = rows.first()?.iter().map(|c| c.expose().trim().to_ascii_lowercase()).collect();
    let column = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let url = column(&["url", "login_uri", "website", "origin", "web site"])?;
    let password = column(&["password", "login_password"])?;
    let username = column(&["username", "login_username", "user name", "login", "user"]);
    let mut logins = Vec::new();
    let mut skipped = 0;
    for row in rows.iter().skip(1) {
        let cell = |i: usize| row.get(i).map(|c| c.expose()).unwrap_or("");
        let origin = vault::origin_of(cell(url));
        match origin {
            Some(origin) if !cell(password).is_empty() => logins.push(Login {
                origin,
                username: username.map(|i| cell(i).to_string()).unwrap_or_default(),
                password: Secret::new(cell(password).to_string()),
            }),
            _ => skipped += 1,
        }
    }
    Some((logins, skipped))
}

/// Saves what isn't saved yet. An account already in the store keeps its
/// password: an import never overwrites. Returns (added, already there, failed).
pub fn import(store: &Store, logins: &[Login]) -> (usize, usize, usize) {
    let (mut added, mut existing, mut failed) = (0, 0, 0);
    for login in logins {
        let account = vault::Account { origin: login.origin.clone(), username: login.username.clone() };
        if store.password(&account).is_some() {
            existing += 1;
        } else if store.save(&login.origin, &login.username, login.password.expose()).is_ok() {
            added += 1;
        } else {
            failed += 1;
        }
    }
    (added, existing, failed)
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

fn could(n: usize) -> &'static str {
    if n == 1 { "pudo" } else { "pudieron" }
}

/// What the person reads after an import.
pub fn report(outcomes: &[Outcome], added: usize, existing: usize, failed_to_save: usize) -> String {
    let mut lines = Vec::new();
    if added > 0 {
        lines.push(format!("Se importaron {}.", count(added, "contraseña", "contraseñas")));
    }
    if existing == 1 {
        lines.push("1 cuenta ya estaba guardada y no se tocó.".to_string());
    } else if existing > 1 {
        lines.push(format!("{existing} cuentas ya estaban guardadas y no se tocaron."));
    }
    if failed_to_save > 0 {
        lines.push(format!(
            "{} no se {} guardar en el Administrador de credenciales.",
            count(failed_to_save, "contraseña", "contraseñas"),
            could(failed_to_save)
        ));
    }
    for outcome in outcomes {
        if outcome.app_bound > 0 {
            lines.push(format!(
                "No se {} importar {} de {name} (cifrado ligado a la aplicación); exportalas a CSV desde {name} e importá el CSV.",
                could(outcome.app_bound),
                count(outcome.app_bound, "contraseña", "contraseñas"),
                name = outcome.name
            ));
        }
        if outcome.failed > 0 {
            lines.push(format!(
                "{} de {} no se {} descifrar.",
                count(outcome.failed, "contraseña", "contraseñas"),
                outcome.name,
                could(outcome.failed)
            ));
        }
        if outcome.unreadable {
            lines.push(format!("No se pudo leer el perfil de {}.", outcome.name));
        }
    }
    if lines.is_empty() {
        lines.push("No había contraseñas para importar.".to_string());
    }
    lines.join("\n\n")
}

// ---------- the dialog ----------

const MENU_CSV: usize = 1000;

fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

/// "Importar…": a menu of the browsers found and a CSV file, then a message
/// saying what came in. The menu opens at `at` (screen coordinates), or at
/// the mouse. Modal: call it outside any borrow of the app state.
pub fn run(owner: HWND, at: Option<POINT>) {
    let found = browsers();
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, browser) in found.iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING, i + 1, &wide(&format!("Desde {}", browser.name)));
        }
        if found.is_empty() {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("No se encontraron otros navegadores"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_CSV, w!("Desde un archivo CSV…"));
        let point = at.unwrap_or_else(|| {
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            point
        });
        let chosen =
            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, owner, None).0 as usize;
        let _ = DestroyMenu(menu);
        chosen
    };
    let store = Store::default();
    let text = match chosen {
        0 => return,
        MENU_CSV => {
            let Some(path) = pick_csv(owner) else { return };
            import_csv(&store, &path)
        }
        i => {
            let Some(browser) = found.get(i - 1) else { return };
            let outcome = read_browser(browser);
            let (added, existing, failed) = import(&store, &outcome.logins);
            report(std::slice::from_ref(&outcome), added, existing, failed)
        }
    };
    unsafe {
        MessageBoxW(Some(owner), &wide(&text), w!("Importar contraseñas — Nea"), MB_OK | MB_ICONINFORMATION);
    }
}

fn import_csv(store: &Store, path: &Path) -> String {
    let Ok(bytes) = std::fs::read(path) else { return "No se pudo leer el archivo.".to_string() };
    let text = Secret::new(String::from_utf8(bytes).unwrap_or_else(|e| {
        let mut bytes = e.into_bytes();
        let lossy = String::from_utf8_lossy(&bytes).into_owned();
        vault::wipe(&mut bytes);
        lossy
    }));
    let Some((logins, skipped)) = logins_from_csv(text.expose()) else {
        return "El archivo no parece una exportación de contraseñas: le faltan las columnas url y password."
            .to_string();
    };
    let (added, existing, failed) = import(store, &logins);
    let mut message = report(&[], added, existing, failed);
    if skipped > 0 {
        message.push_str(&format!(
            "\n\n{} sin dirección web o sin contraseña.",
            count(skipped, "fila quedó afuera", "filas quedaron afuera")
        ));
    }
    message.push_str("\n\nConviene borrar el archivo CSV: guarda las contraseñas sin cifrar.");
    message
}

/// The open dialog; comdlg32.dll is loaded only while it is shown.
fn pick_csv(owner: HWND) -> Option<PathBuf> {
    type OpenFn = unsafe extern "system" fn(*mut OPENFILENAMEW) -> BOOL;
    let comdlg32 = Library::load(w!("comdlg32.dll"))?;
    let open: OpenFn = unsafe { comdlg32.function(c"GetOpenFileNameW") }?;
    let mut buffer = vec![0u16; 1024];
    let filter: Vec<u16> =
        "Contraseñas exportadas (*.csv)\0*.csv\0Todos los archivos\0*.*\0\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: w!("Importar contraseñas desde un CSV"),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    unsafe { open(&mut dialog).as_bool() }.then(|| {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        PathBuf::from(String::from_utf16_lossy(&buffer[..len]))
    })
}
