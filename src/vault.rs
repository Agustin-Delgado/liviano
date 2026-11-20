//! Saved passwords (RF-14): they live only in Windows Credential Manager, as
//! generic credentials named "Liviano:<origin> <user name>". The app keeps no
//! copy on disk, and a password read from the store is held in a `Secret`
//! that wipes itself when dropped.

use std::fmt;

use windows::{
    Win32::{
        Foundation::{ERROR_NOT_FOUND, WIN32_ERROR},
        Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW, CredFree, CredReadW,
            CredWriteW,
        },
    },
    core::{HSTRING, PWSTR},
};

/// What every credential of the app is named with.
pub const PREFIX: &str = "Liviano:";
/// Credential Manager rejects larger secrets (CRED_MAX_CREDENTIAL_BLOB_SIZE).
const MAX_BLOB: usize = 5 * 512;

/// A password in memory. Its bytes are overwritten when it is dropped, and
/// it never prints.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(text: String) -> Self {
        Self(text)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(unsafe { self.0.as_mut_vec() });
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

/// Overwrites bytes in a way the optimizer can't drop.
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Account {
    /// "https://example.com" (scheme, host and a port that isn't the default).
    pub origin: String,
    pub username: String,
}

/// The origin passwords are kept under: only http and https pages have one.
/// "https://User@Example.com:443/login?x" -> "https://example.com".
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "https" => "443",
        "http" => "80",
        _ => return None,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_ascii_lowercase();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port == default_port => host.to_string(),
        Some((_, port)) if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) => return None,
        _ => authority,
    };
    let valid = !host.is_empty()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']' | '_'));
    valid.then(|| format!("{scheme}://{host}"))
}

/// The credential name of an account: "Liviano:https://example.com ana@x.com".
/// An origin never holds a space, so the first one ends it.
pub fn target(prefix: &str, origin: &str, username: &str) -> String {
    format!("{prefix}{origin} {username}")
}

/// Reads a credential name back; None for one that isn't ours.
pub fn parse_target(prefix: &str, target: &str) -> Option<Account> {
    let rest = target.strip_prefix(prefix)?;
    let (origin, username) = rest.split_once(' ')?;
    (origin_of(origin).as_deref() == Some(origin))
        .then(|| Account { origin: origin.to_string(), username: username.to_string() })
}

fn wide0(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The password bytes as Credential Manager stores generic secrets: UTF-16LE.
fn encode_secret(password: &str) -> Vec<u8> {
    password.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn decode_secret(bytes: &[u8]) -> Secret {
    if bytes.len() % 2 == 0 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        if let Ok(text) = String::from_utf16(&units) {
            return Secret(text);
        }
    }
    Secret(String::from_utf8_lossy(bytes).into_owned())
}

fn not_found(error: &windows::core::Error) -> bool {
    error.code() == WIN32_ERROR(ERROR_NOT_FOUND.0).to_hresult()
}

/// The app's corner of Credential Manager. Tests use a prefix of their own
/// (so they never touch real entries) and so can the app, through
/// LIVIANO_CREDENTIAL_PREFIX, the same way LIVIANO_DATA_DIR moves its files.
#[derive(Clone, Debug)]
pub struct Store {
    prefix: String,
}

impl Default for Store {
    fn default() -> Self {
        let prefix = std::env::var("LIVIANO_CREDENTIAL_PREFIX")
            .ok()
            .filter(|p| p.ends_with(':') && !p.contains(char::is_whitespace) && p.len() > 1)
            .unwrap_or_else(|| PREFIX.to_string());
        Self { prefix }
    }
}

impl Store {

    /// Saves (or replaces) the password of an account. Kept on this machine
    /// only: never in the roaming part of the profile.
    pub fn save(&self, origin: &str, username: &str, password: &str) -> windows::core::Result<()> {
        let mut blob = encode_secret(password);
        if blob.len() > MAX_BLOB {
            wipe(&mut blob);
            return Err(windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG));
        }
        let mut target = wide0(&target(&self.prefix, origin, username));
        let mut user = wide0(username);
        let mut comment = wide0("Contraseña guardada por Liviano");
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR(comment.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: if blob.is_empty() { std::ptr::null_mut() } else { blob.as_mut_ptr() },
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        let result = unsafe { CredWriteW(&credential, 0) };
        wipe(&mut blob);
        result
    }

    /// The accounts of one origin, or all of them (sorted by site, then user).
    /// Only names are read here, never a password.
    pub fn accounts(&self, origin: Option<&str>) -> Vec<Account> {
        let filter = match origin {
            Some(origin) => format!("{}{origin} *", self.prefix),
            None => format!("{}*", self.prefix),
        };
        let mut count = 0u32;
        let mut list: *mut *mut CREDENTIALW = std::ptr::null_mut();
        let mut accounts = Vec::new();
        unsafe {
            if CredEnumerateW(&HSTRING::from(filter), None, &mut count, &mut list).is_err() {
                return accounts;
            }
            for i in 0..count as usize {
                let credential = &**list.add(i);
                if credential.Type != CRED_TYPE_GENERIC || credential.TargetName.is_null() {
                    continue;
                }
                let name = credential.TargetName.to_string().unwrap_or_default();
                if let Some(account) = parse_target(&self.prefix, &name) {
                    if origin.is_none_or(|o| o == account.origin) {
                        accounts.push(account);
                    }
                }
            }
            CredFree(list as *const _);
        }
        accounts.sort();
        accounts
    }

    /// The stored password, read only when it is about to be used.
    pub fn password(&self, account: &Account) -> Option<Secret> {
        let name = HSTRING::from(target(&self.prefix, &account.origin, &account.username));
        let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            CredReadW(&name, CRED_TYPE_GENERIC, None, &mut credential).ok()?;
            let c = &*credential;
            let bytes = if c.CredentialBlob.is_null() {
                &mut [][..]
            } else {
                std::slice::from_raw_parts_mut(c.CredentialBlob, c.CredentialBlobSize as usize)
            };
            let secret = decode_secret(bytes);
            wipe(bytes);
            CredFree(credential as *const _);
            Some(secret)
        }
    }

    /// True when the account is gone (also when it wasn't there).
    pub fn delete(&self, account: &Account) -> bool {
        let name = HSTRING::from(target(&self.prefix, &account.origin, &account.username));
        match unsafe { CredDeleteW(&name, CRED_TYPE_GENERIC, None) } {
            Ok(()) => true,
            Err(error) => not_found(&error),
        }
    }
}

