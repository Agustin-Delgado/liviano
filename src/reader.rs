//! Reader mode (RF-10): Mozilla Readability, vendored in `assets/readability`
//! (Apache 2.0), reduces the page to its article under a sober stylesheet that
//! follows the light/dark scheme the app gives the engine. The same command
//! puts the original page back. The script is built only when asked for: the
//! extractor costs nothing until the first use, and nothing after it.

use crate::{inject, web::WebTab};

const READABILITY: &str = include_str!("../assets/readability/Readability.js");
const GLUE: &str = include_str!("../assets/reader.js");
const STYLE: &str = include_str!("../assets/reader.css");

pub fn script() -> String {
    inject::fill(
        GLUE,
        &[("CSS", &inject::js_string(STYLE)), ("TOAST", inject::TOAST), ("READABILITY", READABILITY)],
    )
}

/// Turns reader mode on, or off when the page is already in it.
pub fn toggle(web: &WebTab) {
    inject::execute(&web.webview, &script());
}

