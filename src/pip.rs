//! Floating video (RF-12): the engine's own Picture-in-Picture window, which
//! stays above other apps, over the largest visible video (a playing one
//! first). Validated on WebView2: `requestPictureInPicture()` from a plain
//! `ExecuteScript` is refused for lack of a user gesture, and the app's own
//! shortcut never reaches the page as one; evaluated through DevTools with
//! `userGesture` it opens. Pressing again brings the video back.

use crate::{inject, web::WebTab};

const GLUE: &str = include_str!("../assets/pip.js");

pub fn script() -> String {
    inject::fill(GLUE, &[("TOAST", inject::TOAST)])
}

pub fn toggle(web: &WebTab) {
    inject::execute_with_gesture(&web.webview, &script());
}

