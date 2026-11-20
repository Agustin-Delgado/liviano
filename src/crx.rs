//! CRX3, the Chrome Web Store package: "Cr24", version 3, a protobuf header
//! (`CrxFileHeader`) and the zip. The header carries one or more key proofs
//! (`AsymmetricKeyProof`: a public key and its signature) and, signed with
//! them, the extension id. Verification follows Chromium's `crx_verifier`:
//! every proof must verify, one of them must be the developer's key (the
//! one the id is derived from) and, for a store download, one must be the
//! Web Store's publisher key. Hashing and signatures go through Windows CNG,
//! so no crypto crate ships in the executable.

use windows::{
    Win32::{
        Foundation::NTSTATUS,
        Security::Cryptography::{
            BCRYPT_FLAGS, BCRYPT_HASH_HANDLE, BCRYPT_KEY_HANDLE, BCRYPT_PAD_PKCS1, BCRYPT_PKCS1_PADDING_INFO,
            BCRYPT_SHA256_ALG_HANDLE, BCRYPT_SHA256_ALGORITHM, BCryptCreateHash, BCryptDestroyHash, BCryptDestroyKey,
            BCryptFinishHash, BCryptHashData, BCryptVerifySignature, CERT_PUBLIC_KEY_INFO, CRYPT_IMPORT_PUBLIC_KEY_FLAGS,
            CryptDecodeObjectEx, CryptImportPublicKeyInfoEx2, X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO,
        },
    },
};

const MAGIC: &[u8; 4] = b"Cr24";
/// What the signature covers before the header data and the archive.
const SIGNATURE_CONTEXT: &[u8] = b"CRX3 SignedData\x00";
/// SHA-256 of the Chrome Web Store's publisher key (Chromium's
/// `kPublisherKeyHash`). A package the store served carries a proof made
/// with that key; one built by anybody else can't.
pub const PUBLISHER_KEY_HASH: [u8; 32] = [
    0x61, 0xf7, 0xf2, 0xa6, 0xbf, 0xcf, 0x74, 0xcd, 0x0b, 0xc1, 0xfe, 0x24, 0x97, 0xcc, 0x9b, 0x04, 0x25, 0x4c, 0x65,
    0x8f, 0x79, 0xf2, 0x14, 0x53, 0x92, 0x86, 0x7e, 0xa8, 0x36, 0x63, 0x67, 0xcf,
];
/// Headers are a few kilobytes; anything near this is not a CRX.
const MAX_HEADER: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    RsaSha256,
    EcdsaSha256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    pub algorithm: Algorithm,
    /// DER SubjectPublicKeyInfo.
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Package<'a> {
    pub proofs: Vec<Proof>,
    /// The serialized `SignedData`, signed as is.
    pub signed_header_data: &'a [u8],
    /// The 16 bytes the extension id spells.
    pub crx_id: Vec<u8>,
    pub archive: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotCrx,
    UnsupportedVersion(u32),
    Malformed,
    NoDeveloperProof,
    NotFromWebStore,
    BadSignature,
    WrongId { expected: String, found: String },
    Crypto,
}

impl Error {
    /// What the person is told.
    pub fn describe(&self) -> String {
        match self {
            Error::NotCrx => "lo descargado no es un paquete de extensión (CRX)".to_string(),
            Error::UnsupportedVersion(v) => format!("formato CRX{v} no soportado (sólo CRX3)"),
            Error::Malformed => "el paquete está dañado".to_string(),
            Error::NoDeveloperProof => "el paquete no trae la firma del desarrollador".to_string(),
            Error::NotFromWebStore => "el paquete no está firmado por la Chrome Web Store".to_string(),
            Error::BadSignature => "la firma del paquete no es válida".to_string(),
            Error::WrongId { expected, found } => format!("el paquete es de otra extensión ({found}, se esperaba {expected})"),
            Error::Crypto => "no se pudo verificar la firma en este equipo".to_string(),
        }
    }
}

// ---------- protobuf, just enough for CrxFileHeader ----------

fn varint(data: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// The length-delimited fields of a message, in order; other wire types are
/// skipped (protobuf readers ignore what they don't know).
fn fields(data: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < data.len() {
        let key = varint(data, &mut at)?;
        match key & 7 {
            0 => {
                varint(data, &mut at)?;
            }
            1 => at = at.checked_add(8)?,
            2 => {
                let len = usize::try_from(varint(data, &mut at)?).ok()?;
                let end = at.checked_add(len)?;
                out.push((key >> 3, data.get(at..end)?));
                at = end;
            }
            5 => at = at.checked_add(4)?,
            _ => return None,
        }
    }
    (at == data.len()).then_some(out)
}

fn first(fields: &[(u64, &[u8])], number: u64) -> Option<Vec<u8>> {
    fields.iter().find(|(n, _)| *n == number).map(|(_, v)| v.to_vec())
}

/// Splits a CRX3 file into its proofs, signed data and archive. Nothing is
/// verified here.
pub fn parse(file: &[u8]) -> Result<Package<'_>, Error> {
    if file.len() < 12 || &file[..4] != MAGIC {
        return Err(Error::NotCrx);
    }
    let word = |at: usize| u32::from_le_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]]);
    let version = word(4);
    if version != 3 {
        return Err(Error::UnsupportedVersion(version));
    }
    let header_len = word(8) as usize;
    if header_len > MAX_HEADER || file.len() < 12 + header_len {
        return Err(Error::Malformed);
    }
    let header = &file[12..12 + header_len];
    let archive = &file[12 + header_len..];
    let top = fields(header).ok_or(Error::Malformed)?;
    let mut proofs = Vec::new();
    let mut signed_header_data: &[u8] = &[];
    for (number, value) in &top {
        let algorithm = match number {
            2 => Algorithm::RsaSha256,
            3 => Algorithm::EcdsaSha256,
            10000 => {
                signed_header_data = value;
                continue;
            }
            _ => continue,
        };
        let proof = fields(value).ok_or(Error::Malformed)?;
        let (Some(public_key), Some(signature)) = (first(&proof, 1), first(&proof, 2)) else {
            return Err(Error::Malformed);
        };
        proofs.push(Proof { algorithm, public_key, signature });
    }
    let crx_id = fields(signed_header_data).and_then(|f| first(&f, 1)).ok_or(Error::Malformed)?;
    if crx_id.len() != 16 || proofs.is_empty() {
        return Err(Error::Malformed);
    }
    Ok(Package { proofs, signed_header_data, crx_id, archive })
}

/// The extension id spelled by a hash: its first 16 bytes, each nibble as a
/// letter from 'a' to 'p'.
pub fn id_from_hash(hash: &[u8]) -> String {
    hash.iter().take(16).flat_map(|b| [b >> 4, b & 0x0f]).map(|n| (b'a' + n) as char).collect()
}

/// An id as the store and the engine write it: 32 letters a-p.
pub fn is_extension_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

// ---------- CNG ----------

fn ok(status: NTSTATUS) -> Option<()> {
    (status.0 >= 0).then_some(())
}

struct Hash(BCRYPT_HASH_HANDLE);

impl Drop for Hash {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyHash(self.0);
        }
    }
}

pub fn sha256(parts: &[&[u8]]) -> Option<[u8; 32]> {
    let mut handle = BCRYPT_HASH_HANDLE::default();
    unsafe {
        ok(BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut handle, None, None, 0))?;
        let hash = Hash(handle);
        for part in parts {
            // CNG takes u32 lengths; a part is never near 4 GB here, but feed it in pieces anyway.
            for chunk in part.chunks(1 << 30) {
                ok(BCryptHashData(hash.0, chunk, 0))?;
            }
        }
        let mut out = [0u8; 32];
        ok(BCryptFinishHash(hash.0, &mut out, 0))?;
        Some(out)
    }
}

struct Key(BCRYPT_KEY_HANDLE);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.0);
        }
    }
}

/// Imports a DER SubjectPublicKeyInfo (RSA or EC) as a CNG key.
fn import_key(der: &[u8]) -> Option<Key> {
    unsafe {
        let mut size = 0u32;
        CryptDecodeObjectEx(X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO, der, 0, None, None, &mut size).ok()?;
        // u64 storage: the decoded struct holds pointers and needs their alignment.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        CryptDecodeObjectEx(
            X509_ASN_ENCODING,
            X509_PUBLIC_KEY_INFO,
            der,
            0,
            None,
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
        )
        .ok()?;
        let info = buffer.as_ptr().cast::<CERT_PUBLIC_KEY_INFO>();
        let mut key = BCRYPT_KEY_HANDLE::default();
        CryptImportPublicKeyInfoEx2(X509_ASN_ENCODING, info, CRYPT_IMPORT_PUBLIC_KEY_FLAGS(0), None, &mut key).ok()?;
        Some(Key(key))
    }
}

/// ECDSA signatures travel as DER `SEQUENCE { r INTEGER, s INTEGER }`; CNG
/// wants r and s side by side, each `size` bytes.
pub fn ecdsa_der_to_raw(der: &[u8], size: usize) -> Option<Vec<u8>> {
    fn length(der: &[u8], at: &mut usize) -> Option<usize> {
        let first = *der.get(*at)?;
        *at += 1;
        if first < 0x80 {
            return Some(first as usize);
        }
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 2 {
            return None;
        }
        let mut len = 0usize;
        for _ in 0..count {
            len = (len << 8) | *der.get(*at)? as usize;
            *at += 1;
        }
        Some(len)
    }
    let mut at = 0;
    if *der.first()? != 0x30 {
        return None;
    }
    at += 1;
    let total = length(der, &mut at)?;
    if at + total != der.len() {
        return None;
    }
    let mut raw = Vec::with_capacity(size * 2);
    for _ in 0..2 {
        if *der.get(at)? != 0x02 {
            return None;
        }
        at += 1;
        let len = length(der, &mut at)?;
        let mut int = der.get(at..at + len)?;
        at += len;
        while int.len() > size && int.first() == Some(&0) {
            int = &int[1..];
        }
        if int.len() > size {
            return None;
        }
        raw.extend(std::iter::repeat_n(0, size - int.len()));
        raw.extend_from_slice(int);
    }
    (at == der.len()).then_some(raw)
}

fn verify_proof(proof: &Proof, digest: &[u8; 32]) -> Result<(), Error> {
    let key = import_key(&proof.public_key).ok_or(Error::Crypto)?;
    let status = unsafe {
        match proof.algorithm {
            Algorithm::RsaSha256 => {
                let padding = BCRYPT_PKCS1_PADDING_INFO { pszAlgId: BCRYPT_SHA256_ALGORITHM };
                BCryptVerifySignature(
                    key.0,
                    Some(&padding as *const _ as *const _),
                    digest,
                    &proof.signature,
                    BCRYPT_PAD_PKCS1,
                )
            }
            Algorithm::EcdsaSha256 => {
                let raw = ecdsa_der_to_raw(&proof.signature, 32).ok_or(Error::BadSignature)?;
                BCryptVerifySignature(key.0, None, digest, &raw, BCRYPT_FLAGS(0))
            }
        }
    };
    ok(status).ok_or(Error::BadSignature)
}

/// The digest every proof signs.
pub fn signed_digest(package: &Package) -> Option<[u8; 32]> {
    let len = (package.signed_header_data.len() as u32).to_le_bytes();
    sha256(&[SIGNATURE_CONTEXT, &len, package.signed_header_data, package.archive])
}

#[derive(Debug)]
pub struct Verified<'a> {
    /// The developer's key (DER), which unpacked as the manifest's `key`
    /// keeps the store id.
    pub public_key: Vec<u8>,
    pub archive: &'a [u8],
}

/// Checks a CRX3 the way Chromium does before installing it. `expected_id`
/// is the id the person asked for; `from_store` demands the Web Store proof.
pub fn verify<'a>(file: &'a [u8], expected_id: Option<&str>, from_store: bool) -> Result<Verified<'a>, Error> {
    let package = parse(file)?;
    let digest = signed_digest(&package).ok_or(Error::Crypto)?;
    let mut developer = None;
    let mut publisher = false;
    for proof in &package.proofs {
        let hash = sha256(&[&proof.public_key]).ok_or(Error::Crypto)?;
        verify_proof(proof, &digest)?;
        if hash[..16] == package.crx_id[..] {
            developer = Some(proof.public_key.clone());
        }
        publisher |= hash == PUBLISHER_KEY_HASH;
    }
    let public_key = developer.ok_or(Error::NoDeveloperProof)?;
    if from_store && !publisher {
        return Err(Error::NotFromWebStore);
    }
    let id = id_from_hash(&package.crx_id);
    if let Some(expected) = expected_id {
        if expected != id {
            return Err(Error::WrongId { expected: expected.to_string(), found: id });
        }
    }
    Ok(Verified { public_key, archive: package.archive })
}

/// Standard base64 (the manifest's `key` field).
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

