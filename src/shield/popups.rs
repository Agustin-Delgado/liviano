//! Which new windows a page may open. Pure code: the app asks with what the
//! engine said about the request and what the person allowed.

/// What the engine reports about one `window.open` / `target=_blank`.
pub struct Request<'a> {
    /// The top-level page of the tab that asked (its first party).
    pub opener: &'a str,
    pub target: &'a str,
    /// The engine saw a click or a key behind it.
    pub user_initiated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Open,
    /// Nobody clicked: an ad or a page that opens windows by itself.
    NotByUser,
    /// A click, but to an address the lists know as an ad (a popunder
    /// riding on the first click anywhere on the page).
    AdTarget,
}

/// `allowed`: the person allowed popups on the opener's site.
/// `shielded`: ad blocking is on there (its per-site exception relaxes the
/// list check too, not the click check). `listed`: the target matches a
/// popup rule of the lists, asked only when it matters.
pub fn decide(request: &Request, allowed: bool, shielded: bool, listed: impl FnOnce() -> bool) -> Verdict {
    // Extension pages and the app's own pages open what they need: only a
    // web page is second-guessed.
    if !is_web(request.opener) || allowed {
        return Verdict::Open;
    }
    if !request.user_initiated {
        return Verdict::NotByUser;
    }
    if shielded && is_web(request.target) && listed() {
        return Verdict::AdTarget;
    }
    Verdict::Open
}

fn is_web(url: &str) -> bool {
    let lower = url.trim_start().get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

