//! What an update signature covers, shared by the app (which verifies) and
//! `examples/sign.rs` (which signs), so the two can never disagree.
//!
//! The signed message is `DOMAIN || version || "\n" || package bytes`: the
//! version is inside the signature, so an old signed package can't be served
//! under a newer version number to downgrade an install.

/// Kept from the browser's former name on purpose: installs that still run
/// the old binary verify their first Nea update with it, so changing it would
/// strand them on the old version.
pub const DOMAIN: &[u8] = b"liviano-update-v1\n";

/// The bytes signed before the package itself.
pub fn signed_prefix(version: &str) -> Vec<u8> {
    let mut prefix = DOMAIN.to_vec();
    prefix.extend_from_slice(version.as_bytes());
    prefix.push(b'\n');
    prefix
}

// The app only reads signatures; writing them is the signing tool's job.
#[cfg_attr(not(test), allow(dead_code))]
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xF) as usize] as char);
    }
    out
}

/// Whitespace around it is ignored (a key file usually ends in a newline).
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    text.as_bytes().chunks(2).map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?)).collect()
}
