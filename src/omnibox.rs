//! What the single field does with what was typed: navigate or search.
//! Pure functions, so the rule can be tested without a window.

/// Turns the typed text into the URL to load. Nothing leaves the machine
/// before this is called (i.e. before Enter).
pub fn resolve(input: &str, search_url: &str) -> Option<String> {
    let text = input.trim();
    if text.is_empty() {
        return None;
    }
    if has_scheme(text) {
        return Some(text.to_string());
    }
    if looks_like_address(text) {
        let scheme = if is_local(text) { "http://" } else { "https://" };
        return Some(format!("{scheme}{text}"));
    }
    Some(search_url.replace("{q}", &encode_query(text)))
}

fn has_scheme(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["http://", "https://", "file://", "about:", "data:", "view-source:", "edge://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

fn is_local(text: &str) -> bool {
    let host = host_part(text);
    host == "localhost" || host.ends_with(".localhost") || host.ends_with(".test") || is_ipv4(host)
}

/// The host of something typed without a scheme ("example.com:8080/a" -> "example.com").
fn host_part(text: &str) -> &str {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = &text[..end];
    match authority.rfind(':') {
        Some(colon) if authority[colon + 1..].chars().all(|c| c.is_ascii_digit()) => &authority[..colon],
        _ => authority,
    }
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

fn looks_like_address(text: &str) -> bool {
    if text.contains(char::is_whitespace) {
        return false;
    }
    let host = host_part(text);
    if host == "localhost" || is_ipv4(host) {
        return true;
    }
    let Some((name, tld)) = host.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Percent-encodes a search query (spaces as '+', as search forms send them).
pub fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The host of a full URL, without "www.", used for tab labels and per-site settings.
pub fn display_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    authority.strip_prefix("www.").unwrap_or(authority)
}

/// A URL as it is compared against typed text: no scheme, no "www.".
pub fn strip_for_match(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_prefix("www.").unwrap_or(rest)
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/omnibox.rs"]
#[rustfmt::skip]
mod tests;
