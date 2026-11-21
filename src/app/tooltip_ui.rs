//! Tooltips for the strip's buttons that have no words (the gear, +, the
//! puzzle piece), with the design system's timing: the pointer rests 600 ms before one
//! opens, it closes 100 ms after the pointer leaves, and for 300 ms after
//! one closed the next opens at once, so running the pointer along the
//! buttons reads each one without waiting again.

use std::{cell::RefCell, time::Instant};

use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::ClientToScreen,
    UI::WindowsAndMessaging::{GetClientRect, KillTimer, SetTimer},
};

use super::{App, with_app};
use crate::{
    anim::{TOOLTIP_CLOSE_MS, TOOLTIP_DELAY_MS, TOOLTIP_SKIP_MS},
    shortcuts::Command,
    tabbar::{Hit, NavButton, Rect},
    tooltip::Tooltip,
};

const TIMER_OPEN: usize = 0x49;
const TIMER_CLOSE: usize = 0x4A;

#[derive(Default)]
struct TipState {
    window: Option<Tooltip>,
    /// What the pointer rests on while the delay runs, and what is up.
    waiting: Option<Hit>,
    shown: Option<Hit>,
    closed_at: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<TipState> = RefCell::new(TipState::default());
}

unsafe extern "system" fn on_open(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| {
        if let Some(hit) = STATE.with(|s| s.borrow_mut().waiting.take())
            && app.hover == Some(hit)
        {
            app.show_tip(hit);
        }
    });
}

unsafe extern "system" fn on_close(hwnd: HWND, _: u32, id: usize, _: u32) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
    with_app(|app| {
        if !app.hover.is_some_and(|h| app.tip_text(h).is_some()) {
            app.hide_tip();
        }
    });
}

impl App {
    /// What the tooltip over `hit` says, if it has one.
    fn tip_text(&self, hit: Hit) -> Option<String> {
        let with_chord = |label: &str, command: Command| {
            let chord = self.chord_hint(command);
            if chord.is_empty() { label.to_string() } else { format!("{label} ({chord})") }
        };
        match hit {
            Hit::Settings => Some(with_chord("Settings", Command::Settings)),
            Hit::NewTab if !self.layout.vertical => Some(with_chord("New tab", Command::NewTab)),
            Hit::Extensions => Some("Extensions".to_string()),
            Hit::Group(group) => self.group_tip(group),
            Hit::Nav(NavButton::Back) => Some(with_chord("Back", Command::Back)),
            Hit::Nav(NavButton::Forward) => Some(with_chord("Forward", Command::Forward)),
            Hit::Nav(NavButton::Reload) => Some(with_chord("Reload", Command::Reload)),
            _ => None,
        }
    }

    fn tip_anchor(&self, hit: Hit) -> Option<Rect> {
        match hit {
            Hit::Settings => Some(self.layout.settings),
            Hit::NewTab => Some(self.layout.new_tab),
            Hit::Extensions => Some(self.layout.extensions),
            Hit::Group(group) => self.group_tip_anchor(group),
            Hit::Nav(button) => Some(self.nav_rect(button)),
            _ => None,
        }
    }

    /// Follows the strip's hover: waits, swaps or lets go.
    pub(super) fn tip_follow_hover(&mut self) {
        let target = self.hover.filter(|h| self.drag.is_none() && self.tip_text(*h).is_some());
        let (waiting, shown, closed_at) = STATE.with(|s| {
            let s = s.borrow();
            (s.waiting, s.shown, s.closed_at)
        });
        let Some(hit) = target else {
            if waiting.is_some() {
                STATE.with(|s| s.borrow_mut().waiting = None);
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_OPEN);
                }
            }
            if shown.is_some() {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_CLOSE, TOOLTIP_CLOSE_MS as u32, Some(on_close));
                }
            }
            return;
        };
        if shown == Some(hit) || waiting == Some(hit) {
            return;
        }
        let skip = closed_at.is_some_and(|t| t.elapsed().as_millis() < TOOLTIP_SKIP_MS as u128);
        if shown.is_some() || skip {
            self.show_tip(hit);
            return;
        }
        STATE.with(|s| s.borrow_mut().waiting = Some(hit));
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_OPEN, TOOLTIP_DELAY_MS as u32, Some(on_open));
        }
    }

    fn show_tip(&mut self, hit: Hit) {
        let (Some(text), Some(anchor)) = (self.tip_text(hit), self.tip_anchor(hit)) else { return };
        let anchor = self.screen_rect(anchor);
        let mut client = RECT::default();
        let mut top_left = POINT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
            let _ = ClientToScreen(self.hwnd, &mut top_left);
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOSE);
        }
        let inside = Rect::new(top_left.x, top_left.y, top_left.x + client.right, top_left.y + client.bottom);
        let (palette, scale, owner) = (self.palette, self.scale, self.hwnd);
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.window.is_none() {
                s.window = Tooltip::create(owner).ok();
            }
            if let Some(window) = &s.window {
                window.show(&text, anchor, inside, &palette, scale);
                s.shown = Some(hit);
            }
        });
    }

    /// Lets the tooltip go (a press, the field opening, the pointer gone).
    pub(super) fn hide_tip(&self) {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.waiting.take().is_some() {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_OPEN);
                }
            }
            if s.shown.take().is_some() {
                s.closed_at = Some(Instant::now());
                if let Some(window) = &s.window {
                    window.hide();
                }
            }
        });
    }

    pub(super) fn tip_state(&self) -> serde_json::Value {
        let (up, level) = crate::tooltip::state();
        let shown = STATE.with(|s| s.borrow().shown);
        let text = shown.and_then(|h| self.tip_text(h));
        serde_json::json!({ "shown": up, "level": level, "text": text })
    }
}

/// The frame clock and the fades, for the script control.
pub(super) fn anim_state() -> serde_json::Value {
    let stats = crate::anim::stats();
    serde_json::json!({
        "running": stats.running,
        "ticks": stats.ticks,
        "worst_frame_us": stats.worst_frame_us,
        "frames_over_16ms": stats.frames_over_16ms,
        "worst_gap_us": stats.worst_gap_us,
        "gaps": stats.gaps,
        "total_gap_us": stats.total_gap_us,
        "last_motion_worst_gap_us": stats.last_motion_worst_gap_us,
        "frames": stats.frames,
        "total_frame_us": stats.total_frame_us,
        "fading": crate::fade::snapshot().len(),
    })
}
