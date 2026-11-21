//! The cryptography of importing from Chromium browsers, all of it from
//! Windows itself (DPAPI in crypt32, AES-GCM in bcrypt), so the app carries
//! no crypto library. Both are loaded only while an import runs: `Crypto`
//! holds them and unloads them when dropped.

use std::ffi::c_void;

use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree, NTSTATUS},
        Security::Cryptography::{
            BCRYPT_AES_GCM_ALG_HANDLE, BCRYPT_ALG_HANDLE, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO,
            BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION, BCRYPT_KEY_HANDLE, CRYPT_INTEGER_BLOB,
            CRYPTPROTECT_UI_FORBIDDEN,
        },
    },
    core::{BOOL, PWSTR, w},
};

use crate::{system_dll::Library, vault::wipe};

/// Bytes that must not outlive their use (a profile key, a decrypted password).
pub struct SecretBytes(pub Vec<u8>);

impl Drop for SecretBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

/// How a Chromium `password_value` is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// "v10"/"v11": AES-256-GCM with the profile key from Local State.
    ProfileKey,
    /// "v20": app-bound encryption (Chrome 127+). Its key is unwrapped by a
    /// service running as SYSTEM for Chrome only: another program can't.
    AppBound,
    /// No prefix: the whole value is a DPAPI blob (Chrome before 80).
    Dpapi,
    Empty,
}

pub fn scheme_of(value: &[u8]) -> Scheme {
    match value {
        [] => Scheme::Empty,
        [b'v', b'1', b'0' | b'1', ..] => Scheme::ProfileKey,
        [b'v', b'2', b'0', ..] => Scheme::AppBound,
        _ => Scheme::Dpapi,
    }
}

/// Standard base64 (with or without padding; whitespace is skipped).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let symbols: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let data = match symbols.iter().position(|&c| c == b'=') {
        Some(at) if symbols[at..].iter().all(|&c| c == b'=') && symbols.len() - at <= 2 => &symbols[..at],
        Some(_) => return None,
        None => &symbols[..],
    };
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut acc = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            acc |= value(c)? << (18 - 6 * i);
        }
        let bytes = acc.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// The base64 encoder only the tests need, to write a Local State of their own.
#[cfg(all(test, nea_tests))]
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut acc = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= (b as u32) << (16 - 8 * i);
        }
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(acc >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

type DpapiFn = unsafe extern "system" fn(
    *const CRYPT_INTEGER_BLOB,
    *mut PWSTR,
    *const CRYPT_INTEGER_BLOB,
    *const c_void,
    *const c_void,
    u32,
    *mut CRYPT_INTEGER_BLOB,
) -> BOOL;
type GenerateKeyFn =
    unsafe extern "system" fn(BCRYPT_ALG_HANDLE, *mut BCRYPT_KEY_HANDLE, *mut u8, u32, *const u8, u32, u32) -> NTSTATUS;
type CipherFn = unsafe extern "system" fn(
    BCRYPT_KEY_HANDLE,
    *const u8,
    u32,
    *const c_void,
    *mut u8,
    u32,
    *mut u8,
    u32,
    *mut u32,
    u32,
) -> NTSTATUS;
type DestroyKeyFn = unsafe extern "system" fn(BCRYPT_KEY_HANDLE) -> NTSTATUS;

/// DPAPI and AES-GCM, loaded for one import.
pub struct Crypto {
    unprotect: DpapiFn,
    #[cfg(all(test, nea_tests))]
    protect: DpapiFn,
    generate_key: GenerateKeyFn,
    decrypt: CipherFn,
    #[cfg(all(test, nea_tests))]
    encrypt: CipherFn,
    destroy_key: DestroyKeyFn,
    // Last, so the functions above are never used after the libraries go.
    _crypt32: Library,
    _bcrypt: Library,
}

impl Crypto {
    pub fn load() -> Option<Self> {
        let crypt32 = Library::load(w!("crypt32.dll"))?;
        let bcrypt = Library::load(w!("bcrypt.dll"))?;
        unsafe {
            Some(Self {
                unprotect: crypt32.function(c"CryptUnprotectData")?,
                #[cfg(all(test, nea_tests))]
                protect: crypt32.function(c"CryptProtectData")?,
                generate_key: bcrypt.function(c"BCryptGenerateSymmetricKey")?,
                decrypt: bcrypt.function(c"BCryptDecrypt")?,
                #[cfg(all(test, nea_tests))]
                encrypt: bcrypt.function(c"BCryptEncrypt")?,
                destroy_key: bcrypt.function(c"BCryptDestroyKey")?,
                _crypt32: crypt32,
                _bcrypt: bcrypt,
            })
        }
    }

    fn dpapi(&self, f: DpapiFn, data: &[u8]) -> Option<SecretBytes> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            let ok = f(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            );
            if !ok.as_bool() || output.pbData.is_null() {
                return None;
            }
            let plain = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
            let copy = SecretBytes(plain.to_vec());
            wipe(plain);
            let _ = LocalFree(Some(HLOCAL(output.pbData as _)));
            Some(copy)
        }
    }

    /// Undoes DPAPI for the current Windows user (no prompt is ever shown).
    pub fn dpapi_unprotect(&self, data: &[u8]) -> Option<SecretBytes> {
        self.dpapi(self.unprotect, data)
    }

    #[cfg(all(test, nea_tests))]
    pub fn dpapi_protect(&self, data: &[u8]) -> Vec<u8> {
        // CryptProtectData's second argument is a description; none is given.
        self.dpapi(self.protect, data).map(|b| b.0.clone()).unwrap()
    }

    /// The profile key of a Chromium browser, from Local State's
    /// `os_crypt.encrypted_key`: base64 of "DPAPI" followed by a DPAPI blob.
    pub fn profile_key(&self, encrypted_key_b64: &str) -> Option<SecretBytes> {
        let raw = base64_decode(encrypted_key_b64)?;
        let blob = raw.strip_prefix(b"DPAPI")?;
        let key = self.dpapi_unprotect(blob)?;
        (key.0.len() == 32).then_some(key)
    }

    /// AES-256-GCM as Chromium uses it: no associated data, 16-byte tag.
    /// `data` is the plaintext or the ciphertext; on decryption the tag is
    /// checked. None when the key is wrong or the data was altered.
    fn gcm(&self, f: CipherFn, key: &[u8], nonce: &[u8], data: &[u8], tag: &mut [u8]) -> Option<SecretBytes> {
        if key.len() != 32 || nonce.len() != NONCE_LEN || tag.len() != TAG_LEN {
            return None;
        }
        let mut nonce = nonce.to_vec();
        let info = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
            cbSize: std::mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
            dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
            pbNonce: nonce.as_mut_ptr(),
            cbNonce: nonce.len() as u32,
            pbTag: tag.as_mut_ptr(),
            cbTag: tag.len() as u32,
            ..Default::default()
        };
        let mut handle = BCRYPT_KEY_HANDLE::default();
        unsafe {
            let status = (self.generate_key)(
                BCRYPT_AES_GCM_ALG_HANDLE,
                &mut handle,
                std::ptr::null_mut(),
                0,
                key.as_ptr(),
                key.len() as u32,
                0,
            );
            if status.is_err() {
                return None;
            }
            let mut out = SecretBytes(vec![0u8; data.len()]);
            let mut written = 0u32;
            let status = f(
                handle,
                data.as_ptr(),
                data.len() as u32,
                &info as *const _ as *const c_void,
                std::ptr::null_mut(),
                0,
                out.0.as_mut_ptr(),
                out.0.len() as u32,
                &mut written,
                0,
            );
            let _ = (self.destroy_key)(handle);
            status.is_ok().then(|| {
                out.0.truncate(written as usize);
                out
            })
        }
    }

    pub fn aes_gcm_decrypt(&self, key: &[u8], nonce: &[u8], ciphertext: &[u8], tag: &[u8]) -> Option<SecretBytes> {
        self.gcm(self.decrypt, key, nonce, ciphertext, &mut tag.to_vec())
    }

    #[cfg(all(test, nea_tests))]
    pub fn aes_gcm_encrypt(&self, key: &[u8], nonce: &[u8], plain: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut tag = vec![0u8; TAG_LEN];
        let cipher = self.gcm(self.encrypt, key, nonce, plain, &mut tag).unwrap();
        (cipher.0.clone(), tag)
    }

    /// Decrypts a "v10"/"v11" value with the profile key.
    pub fn decrypt_v10(&self, key: &[u8], value: &[u8]) -> Option<SecretBytes> {
        let body = value.get(3..)?;
        if body.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let (nonce, rest) = body.split_at(NONCE_LEN);
        let (ciphertext, tag) = rest.split_at(rest.len() - TAG_LEN);
        self.aes_gcm_decrypt(key, nonce, ciphertext, tag)
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/vault_crypto.rs"]
#[rustfmt::skip]
mod tests;
