//! A window that a person cannot see stops drawing its pages: a window of
//! a different program covers all of it, or it is on a different virtual
//! desktop (refer to `cover`).
//!
//! While the window is not the active one, a timer asks if it is out of
//! sight. After two yes answers, the window takes a picture of each page
//! on screen, hides the views and draws the pictures in their place. Thus
//! the small pictures that Windows shows of the window (Alt+Tab, the
//! taskbar) continue to show the page. The engine stops drawing a hidden
//! view, and sound continues: the same as a minimized window.
//!
//! The views come back when the timer sees the window again, and
//! immediately when the window becomes the active one.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use webview2_com::{CapturePreviewCompletedHandler, Microsoft::Web::WebView2::Win32::*};
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    Graphics::Gdi::{HDC, InvalidateRect},
    System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, StructuredStorage::CreateStreamOnHGlobal},
    UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, KillTimer, PostMessageW, SetTimer, WM_APP},
};

use super::{App, memory_ui::Sight, with_app};
use crate::{image, tabs::Live};

/// A worker has the picture of a page ready (`PICTURES`).
pub(super) const WM_APP_COVER: u32 = WM_APP + 0x53;
const TIMER_COVER: usize = 0x4D;
/// How often a window in the background asks if it is out of sight.
const CHECK_MS: u32 = 1000;
/// The same, while it is out of sight: the views must come back quickly.
const CHECK_HIDDEN_MS: u32 = 250;
/// The views are hidden after this time also when a picture did not come.
const PICTURE_LIMIT: Duration = Duration::from_millis(1500);
/// A picture is not wider than this: it is kept in memory, and a person
/// sees it only small.
const PICTURE_WIDTH: u32 = 1920;

/// The pictures that the workers decoded, by tab.
static PICTURES: Mutex<Vec<(u64, image::Pixels)>> = Mutex::new(Vec::new());

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Stage {
    /// A person can see the window, as far as it knows.
    #[default]
    Seen,
    /// The last answer was that the window is out of sight. A window that
    /// only moves across is not a cover: the next answer tells.
    Maybe,
    /// The pictures of the pages are being taken.
    Taking { until: Instant },
    /// The views are hidden.
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Act {
    Nothing,
    TakePictures,
    Hide,
    Show,
}

/// The stage after an answer of the timer (`out_of_sight`), and what the
/// window does.
pub(super) fn next(stage: Stage, out_of_sight: bool, now: Instant) -> (Stage, Act) {
    match (stage, out_of_sight) {
        (Stage::Seen, false) => (Stage::Seen, Act::Nothing),
        (_, false) => (Stage::Seen, Act::Show),
        (Stage::Seen, true) => (Stage::Maybe, Act::Nothing),
        (Stage::Maybe, true) => (Stage::Taking { until: now + PICTURE_LIMIT }, Act::TakePictures),
        (Stage::Taking { until }, true) if now >= until => (Stage::Hidden, Act::Hide),
        (stage, true) => (stage, Act::Nothing),
    }
}

#[derive(Default)]
pub(super) struct CoverUi {
    stage: Stage,
    /// The pictures of the pages that were on screen, by tab.
    pictures: Vec<(u64, image::Pixels)>,
    /// How many pictures were asked for.
    wanted: usize,
}

unsafe extern "system" fn on_cover_timer(_: HWND, _: u32, _: usize, _: u32) {
    let _doing = crate::diag::doing(&"timer:cover", 0);
    with_app(|app| app.cover_tick());
}

/// A test run measures the pages as a person sees them, also when a
/// different window is on top of the test window. `NEA_BENCH_SIGHT` is for
/// the test of this feature.
fn watched() -> bool {
    std::env::var_os("NEA_BENCH").is_none() || std::env::var_os("NEA_BENCH_SIGHT").is_some()
}

/// Starts the timer of a new window: it can come up in the background,
/// with no message that tells so.
pub(super) fn watch_cover(hwnd: HWND) {
    if watched() {
        unsafe {
            SetTimer(Some(hwnd), TIMER_COVER, CHECK_MS, Some(on_cover_timer));
        }
    }
}

impl App {
    /// `WM_ACTIVATE`. The active window is in sight. A window in the
    /// background asks again and again.
    pub(super) fn window_activated(&mut self, active: bool) {
        if !watched() {
            return;
        }
        if active {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_COVER);
            }
            self.cover_act(Stage::Seen, Act::Show);
        } else {
            self.cover_timer(CHECK_MS);
        }
    }

    fn cover_timer(&self, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_COVER, ms, Some(on_cover_timer));
        }
    }

    fn cover_tick(&mut self) {
        if unsafe { GetForegroundWindow() } == self.hwnd {
            self.window_activated(true);
            return;
        }
        // A minimized window has its own rule (refer to `memory_ui`).
        let minimized = unsafe { IsIconic(self.hwnd) }.as_bool();
        let out_of_sight = !minimized && crate::cover::out_of_sight(self.hwnd, (self.scale * 96.0).round() as u32);
        let before = self.cover.stage;
        let (stage, act) = next(before, out_of_sight, Instant::now());
        self.cover_act(stage, act);
        if (stage == Stage::Seen) != (before == Stage::Seen) {
            self.cover_timer(if stage == Stage::Seen { CHECK_MS } else { CHECK_HIDDEN_MS });
        }
    }

    fn cover_act(&mut self, stage: Stage, act: Act) {
        let before = std::mem::replace(&mut self.cover.stage, stage);
        match act {
            Act::Nothing => {}
            Act::TakePictures => self.take_cover_pictures(),
            Act::Hide => self.sight_changed(Sight::Cover { covered: true }),
            Act::Show => {
                if before == Stage::Seen {
                    return;
                }
                self.sight_changed(Sight::Cover { covered: false });
                self.cover.pictures = Vec::new();
                self.cover.wanted = 0;
                if let Ok(mut ready) = PICTURES.lock() {
                    ready.clear();
                }
            }
        }
    }

    /// Asks the engine for a picture of each page on screen. A worker
    /// decodes it: that takes tens of milliseconds.
    fn take_cover_pictures(&mut self) {
        self.cover.pictures = Vec::new();
        self.cover.wanted = 0;
        let window = self.hwnd.0 as isize;
        for (id, pane) in self.shown_panes() {
            let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live) else { continue };
            if web.suspended || !web.is_shown() {
                continue;
            }
            let Ok(stream) = (unsafe { CreateStreamOnHGlobal(Default::default(), true) }) else { continue };
            let width = ((pane.right - pane.left).max(1) as u32).min(PICTURE_WIDTH);
            let kept = stream.clone();
            let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
                let Some(full) = result.ok().and_then(|_| image::stream_bytes(&kept)) else { return Ok(()) };
                let _ = std::thread::Builder::new().name("cover".into()).spawn(move || {
                    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                    let pixels = image::decode_fit(&full, width);
                    if com {
                        unsafe { CoUninitialize() };
                    }
                    let Some(pixels) = pixels else { return };
                    if let Ok(mut ready) = PICTURES.lock() {
                        ready.push((id, pixels));
                    }
                    unsafe {
                        let _ = PostMessageW(Some(HWND(window as *mut _)), WM_APP_COVER, WPARAM(0), LPARAM(0));
                    }
                });
                Ok(())
            }));
            let asked = unsafe {
                web.webview.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG, &stream, &handler)
            };
            if asked.is_ok() {
                self.cover.wanted += 1;
            }
        }
        if self.cover.wanted == 0 {
            self.cover_act(Stage::Hidden, Act::Hide);
        }
    }

    /// `WM_APP_COVER`: keeps the pictures that are ready. With all of
    /// them, the views are hidden.
    pub(super) fn cover_pictures_ready(&mut self) {
        let ready = PICTURES.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default();
        let Stage::Taking { .. } = self.cover.stage else { return };
        self.cover.pictures.extend(ready);
        if self.cover.pictures.len() >= self.cover.wanted {
            self.cover_act(Stage::Hidden, Act::Hide);
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&self.layout.content.win()), false);
            }
        }
    }

    /// Whether the pictures fill the ground of the pages: a picture for
    /// each page on screen, while the views are hidden.
    pub(super) fn cover_pictured(&self) -> bool {
        self.cover.stage == Stage::Hidden
            && !self.cover.pictures.is_empty()
            && self.shown_panes().iter().all(|(id, _)| self.cover.pictures.iter().any(|(tab, _)| tab == id))
    }

    /// Draws the pictures of the hidden pages in their places.
    pub(super) fn paint_cover(&self, hdc: HDC) {
        if self.cover.stage != Stage::Hidden {
            return;
        }
        for (id, pane) in self.shown_panes() {
            if let Some((_, px)) = self.cover.pictures.iter().find(|(tab, _)| *tab == id) {
                let rect = RECT { left: pane.left, top: pane.top, right: pane.right, bottom: pane.bottom };
                crate::gfx::picture_gdi(hdc, rect, &px.data, px.width, px.height);
            }
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../../tests/unit/app/cover_ui.rs"]
#[rustfmt::skip]
mod tests;
