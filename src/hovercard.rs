//! The card that shows up when the pointer rests on a tab, as in Chrome:
//! the whole title, the site, what state the tab is in and a small picture
//! of the page. A popup of its own that never takes focus, drawn with
//! Direct2D on the page's ground, with the shadow and corners of the rest.
//! Where it goes and how it is laid out is pure (tested); the window only
//! draws what it is given.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, GetMonitorInfoW, PAINTSTRUCT},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

use crate::{
    anim::specs,
    design::{Palette, dwm, size},
    fade,
    gfx::{Align, Font, RectF, Surface},
    image::Pixels,
    tabbar::Rect,
};

/// The card's width before the DPI scale; the picture is this less padding.
pub const WIDTH: i32 = 264;
const PAD: i32 = 12;
/// How far the picture is stored (and drawn) wide, before the scale.
pub const THUMB_WIDTH: i32 = WIDTH - 2 * PAD;

/// What the card says a tab is doing, when it is worth saying.
pub fn status_line(private: bool, asleep: bool, suspended: bool, kept_awake: bool) -> &'static str {
    if private {
        "Privada: no queda en el historial"
    } else if asleep {
        "Dormida: se vuelve a cargar al abrirla"
    } else if suspended {
        "Suspendida: no usa procesador"
    } else if kept_awake {
        "Siempre despierta (usa más memoria)"
    } else {
        ""
    }
}

/// What the card says about one tab.
pub struct Card {
    pub title: String,
    pub host: String,
    /// "Dormida", "Suspendida"…; empty when there is nothing to say.
    pub status: String,
    pub thumb: Option<Pixels>,
}

/// Where each part of the card goes, in the card's own pixels.
#[derive(Debug, PartialEq)]
pub struct CardLayout {
    pub width: i32,
    pub height: i32,
    pub title: Rect,
    pub host: Rect,
    pub status: Option<Rect>,
    pub thumb: Option<Rect>,
}

fn px(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

/// The card's parts top to bottom: title, site, state (when there is one),
/// then the picture at its own proportions.
pub fn card_layout(scale: f32, has_status: bool, thumb: Option<(u32, u32)>) -> CardLayout {
    let width = px(WIDTH, scale);
    let pad = px(PAD, scale);
    let inner = width - 2 * pad;
    let mut y = px(10, scale);
    let title = Rect::new(pad, y, width - pad, y + px(20, scale));
    y = title.bottom;
    let host = Rect::new(pad, y, width - pad, y + px(18, scale));
    y = host.bottom;
    let status = has_status.then(|| {
        let r = Rect::new(pad, y, width - pad, y + px(18, scale));
        y = r.bottom;
        r
    });
    let thumb = thumb.filter(|(w, h)| *w > 0 && *h > 0).map(|(w, h)| {
        y += px(8, scale);
        let height = ((h as i64 * inner as i64) / w as i64).clamp(1, px(180, scale) as i64) as i32;
        let r = Rect::new(pad, y, pad + inner, y + height);
        y = r.bottom;
        r
    });
    CardLayout { width, height: y + px(10, scale), title, host, status, thumb }
}

/// The card's top-left corner on screen for a tab at `tab` (screen
/// pixels): under it with the tabs on top, beside it with them at the
/// side, always inside the monitor's work area.
pub fn place(tab: Rect, size: (i32, i32), vertical: bool, work: Rect, gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let (mut x, mut y) = if vertical { (tab.right + gap, tab.top) } else { (tab.left, tab.bottom + gap) };
    x = x.min(work.right - w).max(work.left);
    y = y.min(work.bottom - h).max(work.top);
    (x, y)
}

/// The last picture of each tab's page, kept as a small JPEG until a card
/// shows it. Bounded in bytes: past the cap the oldest pictures go.
pub struct Thumbs {
    cap: usize,
    total: usize,
    jpegs: HashMap<u64, Vec<u8>>,
    order: VecDeque<u64>,
}

impl Thumbs {
    pub fn new(cap: usize) -> Self {
        Self { cap, total: 0, jpegs: HashMap::new(), order: VecDeque::new() }
    }

    pub fn put(&mut self, id: u64, jpeg: Vec<u8>) {
        self.forget(id);
        if jpeg.len() > self.cap {
            return;
        }
        self.total += jpeg.len();
        self.jpegs.insert(id, jpeg);
        self.order.push_back(id);
        while self.total > self.cap {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(gone) = self.jpegs.remove(&old) {
                self.total -= gone.len();
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<&[u8]> {
        self.jpegs.get(&id).map(Vec::as_slice)
    }

    pub fn forget(&mut self, id: u64) {
        if let Some(gone) = self.jpegs.remove(&id) {
            self.total -= gone.len();
            self.order.retain(|o| *o != id);
        }
    }

    pub fn clear(&mut self) {
        self.jpegs.clear();
        self.order.clear();
        self.total = 0;
    }

    pub fn bytes(&self) -> usize {
        self.total
    }

    pub fn len(&self) -> usize {
        self.jpegs.len()
    }
}

struct Shown {
    card: Card,
    palette: Palette,
    scale: f32,
}

thread_local! {
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    static SURFACE: RefCell<Surface> = RefCell::new(Surface::default());
}

pub struct HoverCard {
    pub hwnd: HWND,
}

impl HoverCard {
    pub fn create(owner: HWND) -> windows::core::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(card_proc),
                hInstance: instance,
                lpszClassName: w!("LivianoHoverCard"),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassW(&class);
            // Never activated and never in the way: clicks go through it to
            // nothing (it hides as soon as the pointer leaves the tab).
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                w!("LivianoHoverCard"),
                None,
                WS_POPUP,
                0,
                0,
                10,
                10,
                Some(owner),
                None,
                Some(instance),
                None,
            )?;
            dwm::set(hwnd, dwm::CORNERS, dwm::ROUND);
            fade::prepare(hwnd, 255);
            Ok(Self { hwnd })
        }
    }

    /// Shows `card` for a tab at `tab` (screen pixels), without activating
    /// anything; moves it there at once if it is already up.
    pub fn show(&self, card: Card, tab: Rect, vertical: bool, palette: &Palette, scale: f32) {
        let layout = card_layout(scale, !card.status.is_empty(), card.thumb.as_ref().map(|t| (t.width, t.height)));
        let size = (layout.width, layout.height);
        let work = work_area(tab);
        let (x, y) = place(tab, size, vertical, work, px(6, scale));
        dwm::set(self.hwnd, dwm::BORDER_COLOR, palette.line.colorref().0);
        dwm::set(self.hwnd, dwm::DARK_MODE, palette.dark as u32);
        SHOWN.with(|s| *s.borrow_mut() = Some(Shown { card, palette: *palette, scale }));
        // It opens as a tooltip does (fade, 100 ms); moving to another tab
        // while it is up only moves it.
        fade::fade_in(self.hwnd, specs::TOOLTIP_IN, 255, 0);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), x, y, size.0, size.1, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Fades it out (80 ms), then lets go of what it showed (the decoded
    /// picture).
    pub fn hide(&self) {
        fade::fade_out(self.hwnd, specs::TOOLTIP_OUT, 0, Some(forget));
    }
}

fn forget(_: HWND) {
    SHOWN.with(|s| *s.borrow_mut() = None);
}

fn work_area(near: Rect) -> Rect {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: near.left, y: near.top }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let r = info.rcWork;
            return Rect::new(r.left, r.top, r.right, r.bottom);
        }
    }
    Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut rect);
        SHOWN.with(|s| {
            let shown = s.borrow();
            let Some(shown) = shown.as_ref() else { return };
            let (pal, scale, card) = (&shown.palette, shown.scale, &shown.card);
            let layout = card_layout(scale, !card.status.is_empty(), card.thumb.as_ref().map(|t| (t.width, t.height)));
            SURFACE.with(|surface| {
                surface.borrow_mut().paint(hdc, rect, |g| {
                    g.fill(rect.into(), pal.field);
                    let text = |s: f32, weight| Font::new(s * scale, weight, Align::Leading);
                    g.text(&card.title, layout.title.into(), text(size::TEXT, size::MEDIUM), pal.ink);
                    g.text(&card.host, layout.host.into(), text(size::TEXT_SMALL, size::REGULAR), pal.ink_muted);
                    if let Some(r) = layout.status {
                        g.text(&card.status, r.into(), text(size::TEXT_SMALL, size::REGULAR), pal.ink_asleep);
                    }
                    if let (Some(r), Some(thumb)) = (layout.thumb, &card.thumb) {
                        let r: RectF = r.into();
                        g.image(&thumb.data, thumb.width, thumb.height, r, 1.0);
                        g.outline(r, size::RADIUS * scale * 0.5, 1.0, pal.line);
                    }
                })
            });
        });
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn card_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

