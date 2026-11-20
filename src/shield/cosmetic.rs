//! Element hiding (`##selector`) for one kind of list only: cookie banners.
//! Pure code: what a page gets is a list of selectors chosen by its host.
//!
//! Syntax kept: `##sel` (every site), `a.com,b.com##sel` (those sites and
//! their subdomains), `~a.com##sel` (every site but those), and the
//! exceptions `#@#sel` and `a.com#@#sel`. Left out: the extended syntaxes
//! (`#?#`, `#$#`, `#%#`, `##+js`, `##^`), procedural pseudo-classes, and
//! anything that would hide the whole page. A dropped rule hides nothing,
//! never something else.
//!
//! Memory: the selectors and domain lists live in one string; each rule is a
//! 12-byte span into it, and the site-specific ones are found through a
//! sorted `(hash, rule)` vector by each domain they name.
//!
//! Almost every generic rule is a lone `#id` or `.class` (14,431 of the
//! 15,274 in the cookie list): a page gets those only for the ids and
//! classes it really has, the way uBlock does it. A sheet with all of them
//! cost every tab ~10 MB in its renderer (measured on example.com).

use std::collections::HashSet;

use super::filters::{domain_allows, hash32, suffixes};

#[derive(Clone, Copy, Debug)]
struct Rule {
    start: u32,
    selector_len: u16,
    /// The domain list follows the selector in `text`.
    domains_len: u16,
}

#[derive(Default)]
pub struct Cosmetic {
    text: String,
    rules: Vec<Rule>,
    /// No domain list and no id or class to look them up by: every page
    /// gets them.
    generic: Vec<u32>,
    /// `(hash32 of the leading "#id" or ".class", rule)` of the other
    /// generic ones, sorted.
    by_token: Vec<(u32, u32)>,
    /// Only `~` entries: every page except those.
    almost_generic: Vec<u32>,
    /// `(hash32 of an included domain, rule)`, sorted.
    by_domain: Vec<(u32, u32)>,
    /// Exceptions limited to some domains, indexed the same way.
    exceptions: Vec<(u32, u32)>,
}

/// Selectors longer than this are dropped: no banner needs one.
const MAX_SELECTOR: usize = 1024;

/// Pseudo-classes of other blockers that no engine parses: a rule with one
/// is not CSS.
const PROCEDURAL: &[&str] = &[
    ":-abp-", ":has-text(", ":contains(", ":xpath(", ":upward(", ":remove(", ":style(", ":matches-", ":min-text-length(",
    ":watch-attr(", ":others(", ":if(", ":if-not(", ":nth-ancestor(", ":remove-attr(", ":remove-class(",
];

const TOO_BROAD: &[&str] = &["*", "html", "body", ":root", "head", "html > body"];

/// A selector we can paste into `selector{display:none}` as it is.
fn usable(selector: &str) -> bool {
    !selector.is_empty()
        && selector.len() <= MAX_SELECTOR
        && !selector.starts_with('+')
        && !selector.starts_with('^')
        && !selector.contains(['{', '}'])
        && !selector.contains("/*")
        && !selector.chars().any(char::is_control)
        && !PROCEDURAL.iter().any(|p| selector.contains(p))
        && !TOO_BROAD.contains(&selector.to_ascii_lowercase().as_str())
}

/// The `#id` or `.class` a selector can only match under: the first one of
/// its first compound (`div.cookie-bar > a` needs a `.cookie-bar`). None
/// when it has none, or when it is not plain enough to tell (a list, an
/// escape, a non-ASCII name).
fn leading_token(selector: &str) -> Option<&str> {
    if selector.contains([',', '\\']) || !selector.is_ascii() {
        return None;
    }
    let bytes = selector.as_bytes();
    let mut at = bytes.iter().take_while(|b| b.is_ascii_alphanumeric()).count();
    if at > 0 && !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    if !matches!(bytes.get(at), Some(b'#' | b'.')) {
        return None;
    }
    let start = at;
    at += 1;
    let name = bytes[at..].iter().take_while(|b| is_name_byte(**b)).count();
    if name == 0 || bytes[at].is_ascii_digit() {
        return None;
    }
    Some(&selector[start..at + name])
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// The token a page reports for one of its ids (`i` + id) or classes
/// (`c` + class), as `leading_token` writes it.
fn token_key(reported: &str) -> Option<String> {
    let prefix = match reported.as_bytes().first()? {
        b'i' => '#',
        b'c' => '.',
        _ => return None,
    };
    let name = &reported[1..];
    if name.is_empty() || !name.bytes().all(is_name_byte) {
        return None;
    }
    Some(format!("{prefix}{name}"))
}

/// `domains##selector` or `domains#@#selector`: (exception, domains, selector).
fn split_rule(line: &str) -> Option<(bool, &str, &str)> {
    let at = line.find('#')?;
    let rest = &line[at..];
    let (exception, selector) = if let Some(s) = rest.strip_prefix("#@#") {
        (true, s)
    } else if let Some(s) = rest.strip_prefix("##") {
        (false, s)
    } else {
        return None;
    };
    let domains = &line[..at];
    // A network rule with a `#` in its pattern has a `$` or `/` before it.
    if domains.contains(['/', '$', '|', '^', ' ']) {
        return None;
    }
    Some((exception, domains, selector.trim()))
}

impl Cosmetic {
    pub fn compile<'a>(lists: impl IntoIterator<Item = &'a str> + Clone) -> Self {
        // Generic exceptions cancel a selector everywhere, whatever site it
        // was written for: known before anything is kept.
        let mut cancelled: HashSet<&str> = HashSet::new();
        for list in lists.clone() {
            for line in list.lines() {
                if let Some((true, "", selector)) = split_rule(line.trim()) {
                    cancelled.insert(selector);
                }
            }
        }
        let mut seen_generic: HashSet<&str> = HashSet::new();
        let mut c = Cosmetic::default();
        for list in lists {
            for line in list.lines() {
                let line = line.trim();
                if line.starts_with('!') {
                    continue;
                }
                let Some((exception, domains, selector)) = split_rule(line) else { continue };
                if !usable(selector) || cancelled.contains(selector) || domains.len() > u16::MAX as usize {
                    continue;
                }
                if domains.is_empty() {
                    if exception || !seen_generic.insert(selector) {
                        continue;
                    }
                }
                let id = c.push(selector, domains);
                let included: Vec<&str> = domains.split(',').map(str::trim).filter(|d| !d.is_empty() && !d.starts_with('~')).collect();
                match (exception, domains.is_empty(), included.is_empty()) {
                    (false, true, _) => match leading_token(selector) {
                        Some(token) => c.by_token.push((hash32(token.as_bytes()), id)),
                        None => c.generic.push(id),
                    },
                    (false, false, true) => c.almost_generic.push(id),
                    (true, _, true) => {} // `~a.com#@#x`: an exception for nowhere
                    (exception, false, false) => {
                        let index = if exception { &mut c.exceptions } else { &mut c.by_domain };
                        for domain in included {
                            index.push((hash32(domain.to_ascii_lowercase().as_bytes()), id));
                        }
                    }
                    (true, true, false) => unreachable!("an empty list has no included domain"),
                }
            }
        }
        c.by_token.sort_unstable();
        c.by_token.shrink_to_fit();
        c.by_domain.sort_unstable();
        c.by_domain.dedup();
        c.exceptions.sort_unstable();
        c.exceptions.dedup();
        for v in [&mut c.generic, &mut c.almost_generic] {
            v.shrink_to_fit();
        }
        c.by_domain.shrink_to_fit();
        c.exceptions.shrink_to_fit();
        c.rules.shrink_to_fit();
        c.text.shrink_to_fit();
        c
    }

    fn push(&mut self, selector: &str, domains: &str) -> u32 {
        let id = self.rules.len() as u32;
        let start = self.text.len() as u32;
        self.text.push_str(selector);
        // The `$domain=` matcher of the network rules reads `|` lists.
        let domains = domains.to_ascii_lowercase().replace(',', "|");
        self.text.push_str(&domains);
        self.rules.push(Rule { start, selector_len: selector.len() as u16, domains_len: domains.len() as u16 });
        id
    }

    fn selector(&self, id: u32) -> &str {
        let r = self.rules[id as usize];
        &self.text[r.start as usize..r.start as usize + r.selector_len as usize]
    }

    fn domains(&self, id: u32) -> &str {
        let r = self.rules[id as usize];
        let from = r.start as usize + r.selector_len as usize;
        &self.text[from..from + r.domains_len as usize]
    }

    fn lookup<'s>(&'s self, index: &'s [(u32, u32)], host: &'s str) -> impl Iterator<Item = u32> + 's {
        suffixes(host).flat_map(move |(_, suffix)| {
            let key = hash32(suffix.as_bytes());
            let from = index.partition_point(|&(k, _)| k < key);
            index[from..].iter().take_while(move |&&(k, _)| k == key).map(|&(_, id)| id)
        })
    }

    fn excepted(&self, host: &str) -> HashSet<&str> {
        self.lookup(&self.exceptions, host)
            .filter(|&id| domain_allows(self.domains(id), host))
            .map(|id| self.selector(id))
            .collect()
    }

    /// The selectors a page on `host` (lowercase, no port) gets before it
    /// has anything: the generic ones no id or class picks and the host's
    /// own, minus its exceptions.
    pub fn selectors_for(&self, host: &str) -> Vec<&str> {
        let excepted = self.excepted(host);
        let specific = self.lookup(&self.by_domain, host).filter(|&id| domain_allows(self.domains(id), host));
        let almost = self.almost_generic.iter().copied().filter(|&id| domain_allows(self.domains(id), host));
        let mut seen = HashSet::new();
        self.generic
            .iter()
            .copied()
            .chain(almost)
            .chain(specific)
            .map(|id| self.selector(id))
            .filter(|s| !excepted.contains(s) && seen.insert(*s))
            .collect()
    }

    /// The generic selectors picked by the ids and classes a page on `host`
    /// reported (`i` + id, `c` + class), minus its exceptions.
    pub fn selectors_for_tokens<'t>(&self, host: &str, reported: impl IntoIterator<Item = &'t str>) -> Vec<&str> {
        let excepted = self.excepted(host);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for key in reported.into_iter().filter_map(token_key) {
            let hash = hash32(key.as_bytes());
            let from = self.by_token.partition_point(|&(k, _)| k < hash);
            for &(_, id) in self.by_token[from..].iter().take_while(|&&(k, _)| k == hash) {
                let selector = self.selector(id);
                // Hashes can collide: the selector has to start where the
                // token says.
                if leading_token(selector) == Some(key.as_str()) && !excepted.contains(selector) && seen.insert(selector) {
                    out.push(selector);
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn heap_bytes(&self) -> usize {
        self.text.capacity()
            + self.rules.capacity() * std::mem::size_of::<Rule>()
            + (self.generic.capacity() + self.almost_generic.capacity()) * 4
            + (self.by_token.capacity() + self.by_domain.capacity() + self.exceptions.capacity()) * 8
    }
}

