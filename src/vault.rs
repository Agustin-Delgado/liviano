//! Saved passwords: they live only in Windows Credential Manager, as
//! generic credentials named "Nea:<origin> <user name>". The app keeps no
//! copy on disk, and a password read from the store is held in a `Secret`
//! that wipes itself when dropped.

use std::fmt;

use windows::{
    Win32::{
        Foundation::{ERROR_NOT_FOUND, WIN32_ERROR},
        Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredEnumerateW, CredFree,
            CredReadW, CredWriteW,
        },
    },
    core::{HSTRING, PWSTR},
};

/// What every credential of the app is named with.
pub const PREFIX: &str = "Nea:";
/// Credential Manager rejects larger secrets (CRED_MAX_CREDENTIAL_BLOB_SIZE).
const MAX_BLOB: usize = 5 * 512;

/// Two threads that write at the same time can lose an entry, and an
/// import writes on its own thread: every write and delete takes this.
fn write_turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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

/// The credential name of an account: "Nea:https://example.com ana@x.com".
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
    if bytes.len().is_multiple_of(2) {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect();
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
/// NEA_CREDENTIAL_PREFIX, the same way NEA_DATA_DIR moves its files.
#[derive(Clone, Debug)]
pub struct Store {
    prefix: String,
}

/// A prefix named by an environment variable, when it is a usable one.
pub fn prefix_from_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|p| p.ends_with(':') && !p.contains(char::is_whitespace) && p.len() > 1)
}

/// The prefix of profile 0: NEA_CREDENTIAL_PREFIX or `PREFIX`.
pub fn base_prefix() -> String {
    prefix_from_env("NEA_CREDENTIAL_PREFIX").unwrap_or_else(|| PREFIX.to_string())
}

/// Set once at start for a profile other than 0: its own corner.
static PROFILE_PREFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Makes every default store of this process use `prefix`. Only the first
/// call counts.
pub fn use_prefix(prefix: String) {
    let _ = PROFILE_PREFIX.set(prefix);
}

impl Default for Store {
    fn default() -> Self {
        Self { prefix: PROFILE_PREFIX.get().cloned().unwrap_or_else(base_prefix) }
    }
}

/// What `Store::take_from` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Taken {
    /// Written here, read back equal, and deleted there.
    pub moved: usize,
    /// Left where they were: a write or a read back failed, or this store
    /// already had the account with another password.
    pub left: usize,
}

impl Store {
    pub fn with_prefix(prefix: &str) -> Self {
        Self { prefix: prefix.to_string() }
    }

    /// Moves every password saved under `old`'s prefix to this store's. Each
    /// one is written, read back and compared before the old entry goes, so
    /// a failure anywhere leaves it where it was. An account this store
    /// already has wins: its password is never overwritten, and the old entry
    /// is only removed when it says the same. Run again, it finds nothing.
    pub fn take_from(&self, old: &Store) -> Taken {
        let mut taken = Taken::default();
        if old.prefix == self.prefix {
            return taken;
        }
        for account in old.accounts(None) {
            let Some(password) = old.password(&account) else {
                taken.left += 1;
                continue;
            };
            let existing = self.password(&account);
            let written = match &existing {
                Some(_) => true,
                None => self.save(&account.origin, &account.username, password.expose()).is_ok(),
            };
            let verified = written && self.password(&account).is_some_and(|back| back == password);
            if verified && old.delete(&account) {
                taken.moved += 1;
            } else {
                taken.left += 1;
            }
        }
        taken
    }

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
        let mut comment = wide0("Password saved by Nea");
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
        let result = {
            let _turn = write_turn();
            unsafe { CredWriteW(&credential, 0) }
        };
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
                if let Some(account) = parse_target(&self.prefix, &name)
                    && origin.is_none_or(|o| o == account.origin)
                {
                    accounts.push(account);
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
        let deleted = {
            let _turn = write_turn();
            unsafe { CredDeleteW(&name, CRED_TYPE_GENERIC, None) }
        };
        match deleted {
            Ok(()) => true,
            Err(error) => not_found(&error),
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/vault.rs"]
#[rustfmt::skip]
mod tests;
