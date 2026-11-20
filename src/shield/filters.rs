//! A compact network filter engine for standard (Adblock Plus / EasyList)
//! rules. Pure code with no engine or window behind it, so every rule is
//! tested on its own.
//!
//! Memory is the whole point: EasyList + EasyPrivacy are ~110k network rules,
//! and ~85% of them are a bare `||host^`. Those become a sorted `Vec<u64>` of
//! host hashes (8 bytes a rule). The rest keep their pattern in one shared
//! string and sit in sorted `(hash, rule)` vectors keyed by the host they are
//! anchored to, by a token their URL must contain, or by the site they are
//! restricted to. No per-rule allocation survives compilation.
//!
//! Left out on purpose: cosmetic rules (a second phase per the spec), regex
//! rules (`/.../`, a handful, and a regex engine is not worth it) and options
//! that rewrite rather than block (`$csp`, `$removeparam`, `$redirect-rule`…):
//! a rule with an option we don't understand is dropped, never guessed.

use std::collections::{HashMap, HashSet};

/// What a request loads. The bits double as the `$type` mask of a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Script = 1 << 0,
    Image = 1 << 1,
    Stylesheet = 1 << 2,
    Object = 1 << 3,
    Xhr = 1 << 4,
    Subdocument = 1 << 5,
    Ping = 1 << 6,
    Media = 1 << 7,
    Font = 1 << 8,
    Websocket = 1 << 9,
    Other = 1 << 10,
    /// The top-level page. Only `@@…$document` rules look at it.
    Document = 1 << 11,
    /// A new window a page opens. Only `$popup` (or `$all`) rules and the
    /// bare `||host^` ones look at it: a generic path rule like `/ads/*`
    /// would stop links a person meant to follow.
    Popup = 1 << 12,
}

/// Every type a rule without type options applies to (not the page itself).
const DEFAULT_TYPES: u16 = (1 << 11) - 1;
const POPUP: u16 = Kind::Popup as u16;

const EXCEPTION: u8 = 1 << 0;
const IMPORTANT: u8 = 1 << 1;
const THIRD_PARTY: u8 = 1 << 2;
const FIRST_PARTY: u8 = 1 << 3;
/// `||`: the pattern starts at a label boundary of the request host.
const HOST_ANCHOR: u8 = 1 << 4;
/// `|` at the start: the pattern starts at the start of the URL.
const START_ANCHOR: u8 = 1 << 5;
/// `|` at the end: the pattern ends at the end of the URL.
const END_ANCHOR: u8 = 1 << 6;

#[derive(Clone, Copy, Debug)]
struct Rule {
    /// Pattern (lowercase, anchors stripped) at `text[start..start + pattern_len]`,
    /// followed by the `$domain=` list (`dom_len` bytes).
    start: u32,
    pattern_len: u16,
    dom_len: u16,
    types: u16,
    flags: u8,
}

/// Rules of one polarity (blocking or exception), each in the index that
/// finds it fastest.
#[derive(Default)]
struct Index {
    /// Bare `||host^` rules: hashes of the host, sorted.
    hosts: Vec<u64>,
    /// `(hash32 of the anchored host, rule)`, sorted.
    by_host: Vec<(u32, u32)>,
    /// `(hash32 of a token the URL must contain, rule)`, sorted.
    by_token: Vec<(u32, u32)>,
    /// `(hash32 of an included $domain, rule)` for rules with no usable
    /// token: checked only on the sites they name.
    by_site: Vec<(u32, u32)>,
    /// Neither a token nor a site: checked against every request. Few.
    loose: Vec<u32>,
}

impl Index {
    fn finish(&mut self) {
        self.hosts.sort_unstable();
        self.hosts.dedup();
        for v in [&mut self.by_host, &mut self.by_token, &mut self.by_site] {
            v.sort_unstable();
            v.dedup();
            v.shrink_to_fit();
        }
        self.hosts.shrink_to_fit();
        self.loose.shrink_to_fit();
    }

    fn bytes(&self) -> usize {
        self.hosts.capacity() * 8
            + (self.by_host.capacity() + self.by_token.capacity() + self.by_site.capacity()) * 8
            + self.loose.capacity() * 4
    }
}

#[derive(Default)]
pub struct Filters {
    rules: Vec<Rule>,
    text: String,
    block: Index,
    /// `$important` blocks: exceptions don't apply to them.
    important: Index,
    allow: Index,
    /// Rules waiting for their token while compiling.
    untokened: Vec<u32>,
}

/// One request as the engine sees it.
struct Request<'a> {
    url: &'a str,
    host: &'a str,
    /// Offset of `host` inside `url`.
    host_at: usize,
    page_host: &'a str,
    kind: Kind,
    third_party: bool,
}

impl Filters {
    /// Compiles the network rules of one or more lists in standard syntax.
    pub fn compile<'a>(lists: impl IntoIterator<Item = &'a str> + Clone) -> Self {
        // `$badfilter` cancels a rule written identically elsewhere.
        let mut cancelled = HashSet::new();
        for list in lists.clone() {
            for line in list.lines() {
                if let Some(original) = without_badfilter(line.trim()) {
                    cancelled.insert(original);
                }
            }
        }
        let mut filters = Filters::default();
        for list in lists {
            for line in list.lines() {
                let line = line.trim();
                if !cancelled.is_empty() && cancelled.contains(line) {
                    continue;
                }
                filters.add(line);
            }
        }
        filters.index_tokens();
        filters.block.finish();
        filters.important.finish();
        filters.allow.finish();
        filters.rules.shrink_to_fit();
        filters.text.shrink_to_fit();
        filters
    }

    /// Heap bytes held by the compiled engine.
    pub fn heap_bytes(&self) -> usize {
        self.rules.capacity() * std::mem::size_of::<Rule>()
            + self.text.capacity()
            + self.block.bytes()
            + self.important.bytes()
            + self.allow.bytes()
    }

    /// Number of network rules kept.
    pub fn len(&self) -> usize {
        self.rules.len() + self.block.hosts.len() + self.important.hosts.len() + self.allow.hosts.len()
    }

    fn add(&mut self, line: &str) {
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') || is_cosmetic(line) {
            return;
        }
        let (exception, line) = match line.strip_prefix("@@") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        // Regex rules are skipped (see the module comment).
        if line.len() > 1 && line.starts_with('/') && (line.ends_with('/') || line.contains("/$")) {
            return;
        }
        let (pattern, options) = match line.rfind('$') {
            Some(at) if looks_like_options(&line[at + 1..]) => (&line[..at], Some(&line[at + 1..])),
            _ => (line, None),
        };
        let Some(opts) = parse_options(options.unwrap_or("")) else { return };
        let mut types = opts.types;
        if exception {
            // A page-wide exception: the only one that looks at `Document`.
            if opts.document {
                types |= Kind::Document as u16;
            }
        } else if types == 0 {
            // Only the page itself: nothing to block here.
            return;
        }
        if types == 0 {
            return;
        }

        let mut flags = opts.flags;
        if exception {
            flags |= EXCEPTION;
        }
        let mut pattern = pattern;
        if let Some(rest) = pattern.strip_prefix("||") {
            flags |= HOST_ANCHOR;
            pattern = rest;
        } else if let Some(rest) = pattern.strip_prefix('|') {
            flags |= START_ANCHOR;
            pattern = rest;
        }
        if let Some(rest) = pattern.strip_suffix('|') {
            flags |= END_ANCHOR;
            pattern = rest;
        }
        // Leading/trailing wildcards say nothing.
        if flags & (HOST_ANCHOR | START_ANCHOR) == 0 {
            pattern = pattern.trim_start_matches('*');
        }
        if flags & END_ANCHOR == 0 {
            pattern = pattern.trim_end_matches('*');
        }
        if pattern.len() > u16::MAX as usize || opts.domains.len() > u16::MAX as usize {
            return;
        }
        let pattern = pattern.to_ascii_lowercase();
        let index = match (exception, flags & IMPORTANT != 0) {
            (true, _) => &mut self.allow,
            (false, true) => &mut self.important,
            (false, false) => &mut self.block,
        };

        // The common case: `||host^`, nothing else.
        let bare = flags & !(EXCEPTION | IMPORTANT) == HOST_ANCHOR && types == DEFAULT_TYPES && opts.domains.is_empty();
        if bare {
            if let Some(host) = pattern.strip_suffix('^').filter(|h| is_host(h)) {
                index.hosts.push(hash64(host.as_bytes()));
                return;
            }
        }

        let id = self.rules.len() as u32;
        let start = self.text.len() as u32;
        self.text.push_str(&pattern);
        self.text.push_str(&opts.domains.to_ascii_lowercase());
        self.rules.push(Rule { start, pattern_len: pattern.len() as u16, dom_len: opts.domains.len() as u16, types, flags });

        if flags & HOST_ANCHOR != 0 {
            let end = pattern.find(['^', '/', ':', '?', '*', '|']).unwrap_or(pattern.len());
            let host = &pattern[..end];
            let terminated = matches!(pattern.as_bytes().get(end), Some(b'^' | b'/' | b':' | b'?'));
            if terminated && is_host(host) {
                index.by_host.push((hash32(host.as_bytes()), id));
                return;
            }
        }
        if whole_tokens(&pattern, flags).next().is_some() {
            // Its token is chosen once every rule is known (`index_tokens`).
            self.untokened.push(id);
            return;
        }
        let included: Vec<&str> =
            opts.domains.split('|').filter(|d| !d.is_empty() && !d.starts_with('~') && !d.contains('*')).collect();
        if !included.is_empty() && included.len() == opts.domains.split('|').filter(|d| !d.starts_with('~')).count() {
            for domain in included {
                index.by_site.push((hash32(domain.to_ascii_lowercase().as_bytes()), id));
            }
            return;
        }
        index.loose.push(id);
    }

    fn index_tokens(&mut self) {
        let pending = std::mem::take(&mut self.untokened);
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for &id in &pending {
            let (pattern, flags) = self.pattern_of(id);
            for token in whole_tokens(pattern, flags) {
                *counts.entry(hash32(token.as_bytes())).or_default() += 1;
            }
        }
        for id in pending {
            let (pattern, flags) = self.pattern_of(id);
            let Some(token) = best_token(pattern, flags, &counts) else { continue };
            let key = hash32(token.as_bytes());
            let index = match (flags & EXCEPTION != 0, flags & IMPORTANT != 0) {
                (true, _) => &mut self.allow,
                (false, true) => &mut self.important,
                (false, false) => &mut self.block,
            };
            index.by_token.push((key, id));
        }
    }

    fn pattern_of(&self, id: u32) -> (&str, u8) {
        let rule = self.rules[id as usize];
        (&self.text[rule.start as usize..rule.start as usize + rule.pattern_len as usize], rule.flags)
    }

    /// Whether a request should be stopped. `page_url` is the top-level page
    /// it is made for (its first party).
    pub fn should_block(&self, url: &str, page_url: &str, kind: Kind) -> bool {
        let url = lower(url);
        let page = lower(page_url);
        let Some((host_at, host)) = host_of(&url) else { return false };
        let page_host = host_of(&page).map_or("", |(_, h)| h);
        let third_party = page_host.is_empty() || site(host) != site(page_host);
        let request = Request { url: &url, host, host_at, page_host, kind, third_party };

        if self.find(&self.important, &request) {
            return true;
        }
        if !self.find(&self.block, &request) || self.find(&self.allow, &request) {
            return false;
        }
        // `@@||site^$document` turns blocking off for the whole page.
        if let Some((page_at, _)) = host_of(&page) {
            let document =
                Request { url: &page, host: page_host, host_at: page_at, page_host, kind: Kind::Document, third_party: false };
            if self.find(&self.allow, &document) {
                return false;
            }
        }
        true
    }

    /// Whether any rule of `index` matches.
    fn find(&self, index: &Index, request: &Request) -> bool {
        let hit = |rule: u32| self.matches(self.rules[rule as usize], request);
        // Bare host rules have the default types, which leave out the page
        // itself; a popup to a bare ad or tracker host is blocked too.
        let bare_applies = request.kind != Kind::Document;
        if bare_applies && suffixes(request.host).any(|(_, s)| index.hosts.binary_search(&hash64(s.as_bytes())).is_ok()) {
            return true;
        }
        if suffixes(request.host).any(|(_, s)| lookup(&index.by_host, hash32(s.as_bytes())).any(hit)) {
            return true;
        }
        if tokens(request.url).any(|t| lookup(&index.by_token, hash32(t.as_bytes())).any(hit)) {
            return true;
        }
        if suffixes(request.page_host).any(|(_, s)| lookup(&index.by_site, hash32(s.as_bytes())).any(hit)) {
            return true;
        }
        index.loose.iter().any(|&rule| hit(rule))
    }

    fn matches(&self, rule: Rule, request: &Request) -> bool {
        if rule.types & request.kind as u16 == 0 {
            return false;
        }
        if rule.flags & THIRD_PARTY != 0 && !request.third_party {
            return false;
        }
        if rule.flags & FIRST_PARTY != 0 && request.third_party {
            return false;
        }
        let start = rule.start as usize;
        let pattern = &self.text.as_bytes()[start..start + rule.pattern_len as usize];
        let domains = &self.text[start + rule.pattern_len as usize..start + rule.pattern_len as usize + rule.dom_len as usize];
        let url = request.url.as_bytes();
        let end = rule.flags & END_ANCHOR != 0;
        let matched = if rule.flags & HOST_ANCHOR != 0 {
            suffixes(request.host).any(|(at, _)| glob(pattern, &url[request.host_at + at..], true, end))
        } else {
            glob(pattern, url, rule.flags & START_ANCHOR != 0, end)
        };
        // Domain lists run to hundreds of entries: read them last.
        matched && (domains.is_empty() || domain_allows(domains, request.page_host))
    }
}

fn lookup(index: &[(u32, u32)], key: u32) -> impl Iterator<Item = u32> + '_ {
    let from = index.partition_point(|&(k, _)| k < key);
    index[from..].iter().take_while(move |&&(k, _)| k == key).map(|&(_, rule)| rule)
}

/// The host and each parent domain: "a.b.com" -> "a.b.com", "b.com", "com",
/// with the offset of each inside the host.
pub(super) fn suffixes(host: &str) -> impl Iterator<Item = (usize, &str)> {
    std::iter::once(0)
        .chain(host.match_indices('.').map(|(i, _)| i + 1))
        .filter(move |&i| i < host.len())
        .map(move |i| (i, &host[i..]))
}

/// Maximal runs of `[a-z0-9%]`, the units rules are indexed by.
fn tokens(url: &str) -> impl Iterator<Item = &str> {
    url.split(|c: char| !(c.is_ascii_alphanumeric() || c == '%')).filter(|t| t.len() >= 2)
}

/// Tokens nearly every URL has: a rule indexed by one is checked all the time.
const COMMON_TOKENS: &[&str] = &["http", "https", "www", "com", "js", "net", "org", "html", "php", "png", "jpg", "gif"];

/// The tokens a matching URL is guaranteed to contain whole.
fn whole_tokens(pattern: &str, flags: u8) -> impl Iterator<Item = &str> {
    let bytes = pattern.as_bytes();
    let is_token = |c: u8| c.is_ascii_alphanumeric() || c == b'%';
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if !is_token(bytes[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && is_token(bytes[i]) {
                i += 1;
            }
            // A run cut by the pattern's edge or a wildcard may be part of a
            // longer token in the URL.
            let left_ok = if start == 0 { flags & (HOST_ANCHOR | START_ANCHOR) != 0 } else { bytes[start - 1] != b'*' };
            let right_ok = if i == bytes.len() { flags & END_ANCHOR != 0 } else { bytes[i] != b'*' };
            if left_ok && right_ok && i - start >= 2 {
                return Some(&pattern[start..i]);
            }
        }
        None
    })
}

/// Of a rule's tokens, the one fewest other rules share (`counts`), so a
/// URL token wakes up as few rules as possible.
fn best_token<'p>(pattern: &'p str, flags: u8, counts: &HashMap<u32, u32>) -> Option<&'p str> {
    let cost = |t: &str| {
        let shared = counts.get(&hash32(t.as_bytes())).copied().unwrap_or(0);
        (COMMON_TOKENS.contains(&t), shared, usize::MAX - t.len())
    };
    whole_tokens(pattern, flags).min_by_key(|t| cost(t))
}

/// ABP separator: anything but a letter, a digit or `_ - . %`.
fn is_separator(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

/// Matches `seg` (no `*`) at `text[at..]`; returns where it ended.
fn segment_at(seg: &[u8], text: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    for (k, &p) in seg.iter().enumerate() {
        match text.get(i) {
            Some(&c) if p == b'^' && is_separator(c) => i += 1,
            Some(&c) if p != b'^' && c == p => i += 1,
            // `^` also matches the end of the address, but only as the last thing.
            None if p == b'^' && seg[k + 1..].iter().all(|&q| q == b'^') => return Some(i),
            _ => return None,
        }
    }
    Some(i)
}

/// Wildcard match with `*` and `^`.
fn glob(pattern: &[u8], text: &[u8], anchor_start: bool, anchor_end: bool) -> bool {
    let mut segments = pattern.split(|&c| c == b'*').peekable();
    let mut pos = 0;
    let mut first = true;
    while let Some(seg) = segments.next() {
        let fixed = first && anchor_start;
        let must_end = anchor_end && segments.peek().is_none();
        first = false;
        if seg.is_empty() {
            if must_end {
                return true;
            }
            continue;
        }
        let ends_ok = |e: &usize| !must_end || *e == text.len();
        let found = if fixed {
            segment_at(seg, text, pos).filter(ends_ok)
        } else {
            (pos..=text.len()).find_map(|s| segment_at(seg, text, s).filter(ends_ok))
        };
        match found {
            Some(end) => pos = end,
            None => return false,
        }
    }
    true
}

/// `domain=a.com|~b.a.com`: the page must be one of the plain entries (if
/// any) and none of the `~` ones. Subdomains count; the most specific wins.
pub(super) fn domain_allows(domains: &str, page_host: &str) -> bool {
    let mut include_seen = false;
    let mut best: Option<(usize, bool)> = None;
    for entry in domains.split('|').filter(|e| !e.is_empty()) {
        let (negated, domain) = match entry.strip_prefix('~') {
            Some(d) => (true, d),
            None => (false, entry),
        };
        include_seen |= !negated;
        let hit = page_host == domain || page_host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'));
        if hit && best.is_none_or(|(len, _)| domain.len() > len) {
            best = Some((domain.len(), !negated));
        }
    }
    match best {
        Some((_, allowed)) => allowed,
        None => !include_seen,
    }
}

struct Options<'a> {
    types: u16,
    flags: u8,
    document: bool,
    domains: &'a str,
}

fn looks_like_options(text: &str) -> bool {
    !text.is_empty() && !text.contains('/') && text.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '~' || c == '_')
}

/// `None` when the rule uses an option we don't implement: such a rule is
/// dropped rather than applied with a different meaning.
fn parse_options(text: &str) -> Option<Options<'_>> {
    let mut positive = 0u16;
    let mut negative = 0u16;
    let mut flags = 0u8;
    let mut document = false;
    let mut domains = "";
    for option in text.split(',').filter(|o| !o.is_empty()) {
        let (negated, name) = match option.strip_prefix('~') {
            Some(n) => (true, n),
            None => (false, option),
        };
        let (name, value) = name.split_once('=').unwrap_or((name, ""));
        let kind = match name.to_ascii_lowercase().as_str() {
            "script" => Kind::Script as u16,
            "image" => Kind::Image as u16,
            "stylesheet" | "css" => Kind::Stylesheet as u16,
            "object" | "object-subrequest" => Kind::Object as u16,
            "xmlhttprequest" | "xhr" => Kind::Xhr as u16,
            "subdocument" | "frame" => Kind::Subdocument as u16,
            "ping" | "beacon" => Kind::Ping as u16,
            "media" => Kind::Media as u16,
            "font" => Kind::Font as u16,
            "websocket" => Kind::Websocket as u16,
            "other" => Kind::Other as u16,
            "popup" => POPUP,
            "all" => DEFAULT_TYPES | POPUP,
            "document" | "doc" => {
                if !negated {
                    document = true;
                }
                0
            }
            "third-party" | "3p" => {
                flags |= if negated { FIRST_PARTY } else { THIRD_PARTY };
                continue;
            }
            "first-party" | "1p" => {
                flags |= if negated { THIRD_PARTY } else { FIRST_PARTY };
                continue;
            }
            "domain" | "from" => {
                domains = value;
                continue;
            }
            "important" => {
                flags |= IMPORTANT;
                continue;
            }
            // Still a block; the replacement resource is not ours to serve.
            "match-case" | "redirect" | "empty" | "mp4" => continue,
            _ => return None,
        };
        if negated {
            negative |= kind;
        } else {
            positive |= kind;
        }
    }
    let base = if positive == 0 && !document { DEFAULT_TYPES } else { positive };
    let types = base & !negative;
    Some(Options { types, flags, document, domains })
}

fn is_cosmetic(line: &str) -> bool {
    line.match_indices('#').any(|(i, _)| matches!(line.as_bytes().get(i + 1), Some(b'#' | b'@' | b'?' | b'$' | b'%')))
}

/// For a `$badfilter` line, the rule it cancels.
fn without_badfilter(line: &str) -> Option<String> {
    let at = line.rfind('$')?;
    let options: Vec<&str> = line[at + 1..].split(',').collect();
    if !options.contains(&"badfilter") {
        return None;
    }
    let rest: Vec<&str> = options.into_iter().filter(|o| *o != "badfilter").collect();
    Some(if rest.is_empty() { line[..at].to_string() } else { format!("{}${}", &line[..at], rest.join(",")) })
}

fn is_host(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
}

fn lower(text: &str) -> std::borrow::Cow<'_, str> {
    if text.bytes().any(|c| c.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(text.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// The host of an http(s)/ws(s) URL and its offset; `None` for anything else
/// (data:, blob:, about:…), which is never blocked.
pub(super) fn host_of(url: &str) -> Option<(usize, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https" | "ws" | "wss") {
        return None;
    }
    let start = scheme.len() + 3;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let (at, authority) = match authority.rfind('@') {
        Some(i) => (start + i + 1, &authority[i + 1..]),
        None => (start, authority),
    };
    let host = match authority.rfind(':') {
        Some(i) if !authority.starts_with('[') || authority[..i].ends_with(']') => &authority[..i],
        _ => authority,
    };
    (!host.is_empty()).then_some((at, host))
}

/// Second-level labels under which a country's sites register ("com.ar", "co.uk").
const SECOND_LEVELS: &[&str] = &["com", "net", "org", "gov", "gob", "edu", "co", "ac", "mil", "nom", "or", "ne", "go", "ltd", "plc", "sch", "info", "biz"];

/// The registrable part of a host ("www.lanacion.com.ar" -> "lanacion.com.ar"),
/// what first party means. A small rule instead of the 200 KB public suffix list.
pub fn site(host: &str) -> &str {
    if host.bytes().all(|c| c.is_ascii_digit() || c == '.' as u8) || host.starts_with('[') {
        return host;
    }
    let labels: Vec<usize> = host.match_indices('.').map(|(i, _)| i).collect();
    let n = labels.len();
    if n < 2 {
        return host;
    }
    let tld = &host[labels[n - 1] + 1..];
    let second = &host[labels[n - 2] + 1..labels[n - 1]];
    let keep_three = tld.len() == 2 && SECOND_LEVELS.contains(&second);
    match (keep_three, n) {
        (true, 2) => host,
        (true, _) => &host[labels[n - 3] + 1..],
        (false, _) => &host[labels[n - 2] + 1..],
    }
}

fn hash64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

pub(super) fn hash32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

