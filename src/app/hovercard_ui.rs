//! The tab's hover card (`hovercard.rs`) wired to the window. The pointer
//! resting on a tab for a moment opens it; moving to another tab moves it
//! at once; a click, a drag, the field opening or the pointer leaving the
//! strip closes it.
//!
//! The picture is the page as it was last seen: taken (`CapturePreview`)
//! when the tab is left and when a page shown on screen finishes loading,
//! shrunk to the card's width and kept as a JPEG of a few kilobytes, a
//! few megabytes for every tab together, and dropped with its tab. A tab
//! asleep keeps the last one. The tab on screen gets no picture: its page
//! is right there.

use std::{cell::RefCell, sync::Mutex};

use webview2_com::{
    CapturePreviewCompletedHandler, Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG,
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, WPARAM},
    Graphics::Gdi::ClientToScreen,
    System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize, StructuredStorage::CreateStreamOnHGlobal},
    UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, WM_APP},
};

use super::{App, with_app};
use crate::{
    hovercard::{self, Card, HoverCard, Thumbs},
    image, omnibox,
    tabbar::{Hit, Rect},
    tabs::Live,
};

const TIMER_CARD: usize = 0x48;
/// How long the pointer rests on a tab before its card opens.
const DELAY_MS: u32 = 400;
/// Bytes every tab's picture may take together.
const THUMBS_CAP: usize = 3 * 1024 * 1024;
const JPEG_QUALITY: f32 = 0.72;
/// Posted when a picture shrunk away from the UI thread is ready.
pub(super) const WM_APP_THUMB: u32 = WM_APP + 0x52;

/// Pictures shrunk by a worker, waiting for the UI thread: tab, JPEG.
static SHRUNK: Mutex<Vec<(u64, Vec<u8>)>> = Mutex::new(Vec::new());

#[derive(Default)]
struct CardState {
    window: Option<HoverCard>,
    /// The tab the pointer rests on while the delay runs.
    waiting: Option<u64>,
    /// The tab whose card is up.
    shown: Option<u64>,
}

thread_local! {
    static STATE: RefCell<CardState> = RefCell::new(CardState::default());
    static THUMBS: RefCell<Thumbs> = RefCell::new(Thumbs::new(THUMBS_CAP));
}

unsafe extern "system" fn on_delay(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| app.card_delay_over());
}

impl App {
    /// Follows the strip's hover: waits on a new tab, moves an open card.
    pub(super) fn card_follow_hover(&mut self) {
        let over = match self.hover {
            Some(Hit::Tab(id) | Hit::Close(id)) if self.drag.is_none() && self.mode.is_none() => Some(id),
            _ => None,
        };
        let Some(id) = over else {
            self.hide_card();
            return;
        };
        let (waiting, shown) = STATE.with(|s| {
            let s = s.borrow();
            (s.waiting, s.shown)
        });
        if shown == Some(id) || waiting == Some(id) {
            return;
        }
        if shown.is_some() {
            self.show_card(id);
            return;
        }
        STATE.with(|s| s.borrow_mut().waiting = Some(id));
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_CARD, DELAY_MS, Some(on_delay));
        }
    }

    fn card_delay_over(&mut self) {
        let waiting = STATE.with(|s| s.borrow_mut().waiting.take());
        let still = matches!(self.hover, Some(Hit::Tab(id) | Hit::Close(id)) if Some(id) == waiting);
        if let (true, Some(id)) = (still && self.drag.is_none() && self.mode.is_none(), waiting) {
            self.show_card(id);
        }
    }

    pub(super) fn hide_card(&self) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.waiting.take().is_some() {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_CARD);
                }
            }
            if s.shown.take().is_some()
                && let Some(window) = &s.window
            {
                window.hide();
            }
        });
    }

    fn show_card(&mut self, id: u64) {
        let (Some(item), Some(tab)) = (self.layout.items.iter().find(|i| i.id == id), self.tabs.get(id)) else {
            self.hide_card();
            return;
        };
        let suspended = matches!(&tab.live, Live::Ready(web) if web.suspended);
        let status = hovercard::status_line(
            tab.private,
            matches!(tab.live, Live::Asleep),
            suspended,
            self.keeps_awake(&tab.url),
        );
        let on_screen = self.visible_tabs().contains(&id);
        let width = (hovercard::THUMB_WIDTH as f32 * self.scale).round() as u32;
        let thumb = (self.settings.hover_preview && !on_screen)
            .then(|| THUMBS.with(|t| t.borrow().get(id).and_then(|jpeg| image::decode_fit(jpeg, width))))
            .flatten();
        let card = Card {
            title: tab.label().to_string(),
            host: omnibox::display_host(&tab.url).to_string(),
            status: status.to_string(),
            thumb,
        };
        let rect = self.screen_rect(item.rect);
        let (hwnd, vertical, palette, scale) = (self.hwnd, self.vertical, self.palette, self.scale);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.window.is_none() {
                s.window = HoverCard::create(hwnd).ok();
            }
            if let Some(window) = &s.window {
                window.show(card, rect, vertical, &palette, scale);
                s.shown = Some(id);
            }
        });
    }

    pub(super) fn screen_rect(&self, r: Rect) -> Rect {
        let mut a = POINT { x: r.left, y: r.top };
        let mut b = POINT { x: r.right, y: r.bottom };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut a);
            let _ = ClientToScreen(self.hwnd, &mut b);
        }
        Rect::new(a.x, a.y, b.x, b.y)
    }

    /// Takes the picture of a tab's page as it is now, for its card.
    pub(super) fn capture_thumb(&self, id: u64) {
        if !self.settings.hover_preview {
            THUMBS.with(|t| t.borrow_mut().clear());
            return;
        }
        let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live) else { return };
        if web.suspended {
            return;
        }
        let Ok(stream) = (unsafe { CreateStreamOnHGlobal(Default::default(), true) }) else { return };
        let width = (hovercard::THUMB_WIDTH as f32 * self.scale).round() as u32;
        let kept = stream.clone();
        let window = self.hwnd.0 as isize;
        let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
            let Some(full) = result.ok().and_then(|_| image::stream_bytes(&kept)) else { return Ok(()) };
            // Decoding the whole page and encoding it again takes tens of
            // milliseconds: never on the UI thread, which may be drawing.
            let _ = std::thread::Builder::new().name("thumb".into()).spawn(move || {
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                let jpeg = image::shrink_to_jpeg(&full, width, JPEG_QUALITY);
                if com {
                    unsafe { CoUninitialize() };
                }
                let Some(jpeg) = jpeg else { return };
                if let Ok(mut shrunk) = SHRUNK.lock() {
                    shrunk.push((id, jpeg));
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(window as *mut _)), WM_APP_THUMB, WPARAM(0), LPARAM(0));
                }
            });
            Ok(())
        }));
        unsafe {
            let _ = web.webview.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG, &stream, &handler);
        }
    }

    /// Keeps the pictures a worker shrunk (`WM_APP_THUMB`), for tabs still open.
    pub(super) fn thumbs_shrunk(&self) {
        let shrunk = SHRUNK.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default();
        for (id, jpeg) in shrunk {
            if self.tabs.get(id).is_some() {
                THUMBS.with(|t| t.borrow_mut().put(id, jpeg));
            }
        }
    }

    /// For scripts: the card up, and what the pictures weigh.
    pub(super) fn card_state(&self) -> serde_json::Value {
        let shown = STATE.with(|s| s.borrow().shown);
        let (pictures, bytes) = THUMBS.with(|t| {
            let t = t.borrow();
            (t.len(), t.bytes())
        });
        serde_json::json!({ "shown": shown, "pictures": pictures, "picture_bytes": bytes })
    }

    /// A closed tab's picture goes with it.
    pub(super) fn forget_thumb(&self, id: u64) {
        THUMBS.with(|t| t.borrow_mut().forget(id));
        if STATE.with(|s| s.borrow().shown == Some(id)) {
            self.hide_card();
        }
    }
}
