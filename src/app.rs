//! The window and the app state: commands, the tab life cycle (live,
//! suspended, asleep) and persistence. One thread, one window.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment;
use windows::{
    Win32::{
        Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CLEARTYPE_QUALITY, CreateFontIndirectW, DeleteObject, EndPaint, GetMonitorInfoW, HFONT,
            InvalidateRect, LOGFONTW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect, PAINTSTRUCT,
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData},
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        },
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_MENU},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

mod apply_settings;
mod bench_ui;
mod bookmarks_ui;
mod engine_ui;
mod extensions_ui;
mod frame_ui;
mod fullscreen_ui;
mod groups_ui;
mod hovercard_ui;
mod memory_ui;
mod nav_ui;
mod notify_ui;
mod panel_ui;
mod popups_ui;
mod power_ui;
mod private_tabs;
mod spaces_ui;
mod split_ui;
mod tooltip_ui;
mod update_ui;
mod vault_ui;
mod views_ui;

pub(crate) use vault_ui::start_import;

use crate::{
    curtain::Curtain,
    design::{self, Palette},
    gfx,
    history::{Entry, History},
    omnibar::{Omnibar, Signal},
    omnibox,
    panels::PanelKind,
    pip, reader,
    session::Session,
    settings::Settings,
    shield,
    shortcuts::{Command, Shortcuts},
    split::{Side, Splits},
    storage,
    tabbar::{self, Hit, Metrics, TabInfo},
    tabs::{Live, TabList},
    web::{self, Event, WebTab},
};

const TIMER_SLEEP: usize = 1;
const SLEEP_EVERY_MS: u32 = 15_000;
/// Closing a view waits on the engine (tens of milliseconds, hundreds when it
/// is busy): under memory pressure thirty of them in one go froze the window
/// for seconds. One per tick, and the next tick comes sooner.
const DISCARDS_PER_TICK: usize = 1;
/// A lower or a suspend is a shorter call: a few in a tick without a discard.
const STAGES_PER_TICK: usize = 4;
const SLEEP_AGAIN_MS: u32 = 250;
/// A discard waits until no key or click came for this long.
const QUIET_BEFORE_DISCARD: Duration = Duration::from_secs(1);
const TIMER_SAVE: usize = 2;
const TIMER_RETRY: usize = 3;
/// Work done ahead of time while the person is away (see `idle_work`).
const TIMER_IDLE: usize = 4;
/// How long after startup, or after Settings closed, that work is tried.
const IDLE_WORK_AFTER_MS: u32 = 3_000;
/// The person is away when no key or click came for this long.
const AWAY_AFTER_MS: u32 = 1_500;
/// The pause between two steps of the Settings window made ahead.
const PREPARE_STEP_EVERY_MS: u32 = 32;
/// Losing this much history to a crash is fine; rewriting it every 5 s is not.
const HISTORY_SAVE_EVERY: Duration = Duration::from_secs(30);
/// Engine events left for the next pass (see `drain_events`).
const TIMER_EVENTS: usize = 5;
/// After the first paint, the field draws itself once out of sight (refer
/// to `Omnibar::warm`), when nothing moves.
const TIMER_FIELD_WARM: usize = 6;
const FIELD_WARM_AFTER_MS: u32 = 1_500;
const FIELD_WARM_AGAIN_MS: u32 = 300;
/// One pass over the engine events stops after this much work: 200 views
/// made at once queued 200 events, and handling them in one go held the
/// window for seconds.
const EVENTS_BUDGET: Duration = Duration::from_millis(8);
const WM_APP_LAYOUT: u32 = WM_APP + 2;
/// The smallest window, in DIPs: the strip and the window buttons still fit.
const MIN_WINDOW_DIPS: (i32, i32) = (420, 300);
const WM_APP_REPAINT: u32 = WM_APP + 3;
/// Posted once, after the window's first paint (see `request_environment`).
const WM_APP_PAINTED: u32 = WM_APP + 5;
const MENU_PIN: usize = 10;
const MENU_DUPLICATE: usize = 11;
const MENU_KEEP_AWAKE: usize = 12;
const MENU_CLOSE: usize = 13;
const MENU_CLOSE_OTHERS: usize = 14;
const MENU_SHIELD: usize = 15;
const MENU_POPUPS: usize = 16;
const MENU_SPLIT: usize = 40;
const MENU_UNSPLIT: usize = 41;
const MENU_TAB_LAYOUT: usize = 42;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Navigate the current tab.
    Address,
    /// Open what is typed in a new tab (Ctrl+T): no empty tab exists until Enter.
    NewTab,
    /// Same, in a private tab (Ctrl+Shift+N).
    NewPrivateTab,
    /// Find an open tab by name (Ctrl+K).
    Switch,
    Find,
    /// Bookmarks, history or downloads, listed and searched in the field.
    Panel(PanelKind),
    /// Choose what goes beside the current page (Ctrl+Alt+N).
    Split,
    /// Name a tab group: Enter keeps what is typed.
    RenameGroup(u64),
    /// Name a space: Enter keeps what is typed.
    RenameSpace(u64),
    /// Name the bookmark chosen on the bookmarks bar (`bookmarks_ui`).
    RenameBookmark,
}

#[derive(Clone, Debug)]
enum Row {
    Go(String),
    Switch(u64),
    Info,
    Panel(panel_ui::Item),
    /// A pasted Web Store link: install that extension.
    InstallExtension(String),
}

struct App {
    hwnd: HWND,
    settings: Settings,
    shortcuts: Rc<Shortcuts>,
    palette: Palette,
    scale: f32,
    font: HFONT,
    env: Option<ICoreWebView2Environment>,
    env_error: Option<String>,
    env_requested: bool,
    /// The window painted once (see `request_environment`).
    painted: bool,
    /// The field opens once the window painted (no tabs at startup).
    field_after_paint: bool,
    /// The engine was asked for before the first paint.
    env_after_paint: bool,
    tabs: TabList<WebTab>,
    history: History,
    vertical: bool,
    collapsed: bool,
    layout: tabbar::Layout,
    surface: gfx::Surface,
    motion: tabbar::Motion,
    omnibar: Box<Omnibar>,
    mode: Option<Mode>,
    rows: Vec<Row>,
    typed_len: usize,
    hover: Option<Hit>,
    tracking_mouse: bool,
    drag: Option<u64>,
    session_dirty: bool,
    /// The version of the verified update that waits for a restart: the
    /// update chip shows while it is set (refer to `update_ui`).
    update: Option<String>,
    history_saved: Instant,
    panels: panel_ui::Panels,
    curtain: Curtain,
    split: split_ui::SplitUi,
    groups: groups_ui::GroupsUi,
    bench: bench_ui::BenchState,
    vault: vault_ui::Vault,
    notify: notify_ui::Notify,
    extensions: extensions_ui::Extensions,
    popups: popups_ui::Popups,
    frame: frame_ui::FrameUi,
    nav: nav_ui::NavUi,
    /// The strip was moving at the last paint (for the frame gaps).
    motion_was_running: bool,
    /// The time a frame of the strip's motion is drawn for, set by the
    /// frame clock just before it paints.
    frame_now: Option<Instant>,
    /// What the strip shows on screen, when all of it is known (see
    /// `tabbar::changed_area`).
    painted_sheet: Option<tabbar::Sheet>,
    engine: engine_ui::EngineUi,
    memory: memory_ui::MemoryUi,
    /// Where the pages were scrolled when they left the screen (`memory::ScrollMemo`).
    scrolls: crate::memory::ScrollMemo,
    power: power_ui::PowerUi,
    fullscreen: fullscreen_ui::FullscreenUi,
    spaces: spaces_ui::SpacesUi,
    /// The bookmarks bar's copy of the list (`bookmarks_ui`).
    bookmarks: bookmarks_ui::BarUi,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Messages that found the app busy, waiting for `TIMER_RETRY`.
    static RETRY: RefCell<Vec<(HWND, u32, WPARAM, LPARAM)>> = const { RefCell::new(Vec::new()) };
    /// The last request of a script, which stands for the person's input.
    static LAST_SCRIPT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// A script asked for something: as input from the person (see `quiet_for`).
fn note_script() {
    LAST_SCRIPT.with(|l| l.set(Some(Instant::now())));
}

/// Whether no key, click or pointer move came for `quiet`, in this window,
/// in a page or anywhere in the session (a page's input never reaches this
/// thread), and no script request either.
fn quiet_for(quiet: Duration) -> bool {
    quiet_time().is_some_and(|q| q >= quiet)
}

/// How long no key, click, pointer move or script request came (refer to
/// `quiet_for`).
fn quiet_time() -> Option<Duration> {
    let idle = system_idle()?;
    Some(LAST_SCRIPT.with(|l| l.get()).map_or(idle, |at| idle.min(at.elapsed())))
}

/// How long the session had no input.
fn system_idle() -> Option<Duration> {
    use windows::Win32::{System::SystemInformation::GetTickCount, UI::Input::KeyboardAndMouse::*};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe { GetLastInputInfo(&mut info).as_bool() }
        .then(|| Duration::from_millis(u64::from(unsafe { GetTickCount() }.wrapping_sub(info.dwTime))))
}

/// The app is busy — a menu's modal loop runs inside it, and dispatches
/// our messages meanwhile —: the message comes back shortly. Posting it
/// again at once spun the loop at full CPU for as long as the menu was open.
/// Whether `dirty` holds all of `r`.
/// Whether `dirty` holds all of `r` that is inside a `width` × `height` client area.
fn covers(dirty: RECT, r: crate::gfx::RectF, width: i32, height: i32) -> bool {
    let (left, top) = (r.left.max(0.0), r.top.max(0.0));
    let (right, bottom) = (r.right.min(width as f32), r.bottom.min(height as f32));
    right <= left
        || bottom <= top
        || (dirty.left as f32 <= left
            && dirty.top as f32 <= top
            && dirty.right as f32 >= right
            && dirty.bottom as f32 >= bottom)
}

/// `rect` without `hole`: the pieces left around it (at most four, none
/// empty) and the part they share, if any.
fn split_off(rect: RECT, hole: RECT) -> (Vec<RECT>, Option<RECT>) {
    let inner = RECT {
        left: rect.left.max(hole.left),
        top: rect.top.max(hole.top),
        right: rect.right.min(hole.right),
        bottom: rect.bottom.min(hole.bottom),
    };
    if inner.right <= inner.left || inner.bottom <= inner.top {
        return (vec![rect], None);
    }
    let pieces = [
        RECT { bottom: inner.top, ..rect },
        RECT { top: inner.bottom, ..rect },
        RECT { top: inner.top, bottom: inner.bottom, right: inner.left, ..rect },
        RECT { top: inner.top, bottom: inner.bottom, left: inner.right, ..rect },
    ];
    let pieces = pieces.into_iter().filter(|r| r.right > r.left && r.bottom > r.top).collect();
    (pieces, Some(inner))
}

/// The frame clock asks what the strip's next frame draws (see
/// `App::frame_area`); with the app busy, all of it.
fn strip_frame_area(_: HWND, now: Instant) -> Option<RECT> {
    with_app(|app| app.frame_area(now)).flatten()
}

fn retry_later(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    RETRY.with(|pending| {
        let mut pending = pending.borrow_mut();
        if !pending.iter().any(|&(h, m, w, l)| h == hwnd && m == msg && w == wparam && l == lparam) {
            pending.push((hwnd, msg, wparam, lparam));
        }
    });
    unsafe {
        SetTimer(Some(hwnd), TIMER_RETRY, 30, None);
    }
}

fn post_retries(hwnd: HWND) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_RETRY);
    }
    for (target, msg, wparam, lparam) in RETRY.with(|pending| std::mem::take(&mut *pending.borrow_mut())) {
        unsafe {
            let _ = PostMessageW(Some(target), msg, wparam, lparam);
        }
    }
}

/// Runs `f` on the app unless it is already busy (a Win32 call made from
/// inside the app can send a message straight back to the window).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut guard| guard.as_mut().map(f)))
}

/// Whether the app is in the middle of something (refer to `with_app`).
fn app_busy() -> bool {
    APP.with(|cell| cell.try_borrow_mut().is_err())
}

/// Whether the window may go now: the app wrote its session, or there is
/// no app to write one. A busy app gets the close again shortly.
fn close_now(shut_down: bool, busy: bool) -> bool {
    shut_down || !busy
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

pub fn run() -> windows::core::Result<()> {
    gfx::warm_up();
    storage::watch_session_end();
    let data = storage::data_dir();
    let settings: Settings = storage::read_json(&data.join("settings.json"));
    let session: Session = storage::read_json(&data.join("session.json"));
    let loading = Instant::now();
    let history = History::from_entries(storage::read_json::<Vec<Entry>>(&data.join("history.json")));
    crate::diag::trace(|| {
        format!(
            "startup: history of {} entries read in {:.1} ms",
            history.entries().len(),
            loading.elapsed().as_secs_f64() * 1000.0
        )
    });
    shield::set_disabled(&settings.shield_disabled);
    shield::set_cosmetic(settings.block_trackers);

    crate::diag::trace(|| "startup: settings and session read".to_string());
    let hwnd = create_window(&session)?;
    crate::diag::trace(|| "startup: window created".to_string());
    crate::app_icon::apply(hwnd);
    frame_ui::install(hwnd, session.vertical_tabs, session.sidebar_collapsed);
    web::set_window(hwnd);
    crate::notifications::set_window(hwnd);
    crate::diag::set_window(hwnd);
    crate::updater::notify(hwnd.0 as isize, update_ui::WM_APP_UPDATE);
    crate::updater::start(&settings);
    let bench_on = crate::bench::wanted(settings.bench_enabled);

    let mut omnibar = Box::new(Omnibar::create(hwnd, on_omnibar));
    crate::omnibar::register(&omnibar);
    crate::diag::trace(|| "startup: field created".to_string());

    let palette = design::palette(settings.theme);
    let scale = design::window_scale(hwnd);
    let font = make_font(scale);
    crate::favicon::set_scale(scale);
    omnibar.set_font(font, scale);
    omnibar.set_palette(&palette);
    apply_window_theme(hwnd, &palette);

    let (tabs, restored) = TabList::from_session(&session);
    let splits = Splits::from_saved(&session.splits, &restored);

    let app = App {
        hwnd,
        shortcuts: Rc::new(Shortcuts::new(&settings.shortcuts)),
        settings,
        palette,
        scale,
        font,
        env: None,
        env_error: None,
        env_requested: false,
        painted: false,
        env_after_paint: false,
        field_after_paint: false,
        tabs,
        history,
        vertical: session.vertical_tabs,
        collapsed: session.sidebar_collapsed,
        layout: Default::default(),
        surface: Default::default(),
        motion: {
            crate::anim::refresh_reduced();
            tabbar::Motion::new(!design::animations_enabled())
        },
        omnibar,
        mode: None,
        rows: Vec::new(),
        typed_len: 0,
        hover: None,
        tracking_mouse: false,
        drag: None,
        session_dirty: false,
        update: None,
        history_saved: Instant::now(),
        panels: Default::default(),
        curtain: Curtain::load(&data),
        split: split_ui::SplitUi::new(splits),
        groups: Default::default(),
        bench: Default::default(),
        vault: Default::default(),
        notify: Default::default(),
        extensions: extensions_ui::Extensions::load(),
        popups: Default::default(),
        frame: Default::default(),
        nav: Default::default(),
        motion_was_running: false,
        frame_now: None,
        painted_sheet: None,
        engine: Default::default(),
        memory: Default::default(),
        scrolls: Default::default(),
        power: Default::default(),
        fullscreen: Default::default(),
        spaces: Default::default(),
        bookmarks: bookmarks_ui::BarUi::load(),
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    if bench_on {
        crate::bench::set_enabled(hwnd, true);
    }

    unsafe {
        // A scripted run must not take the foreground (and the keyboard)
        // from whatever the person is doing.
        let show = match (crate::bench::from_env(), session.maximized) {
            (true, _) => SW_SHOWNOACTIVATE,
            (false, true) => SW_SHOWMAXIMIZED,
            (false, false) => SW_SHOW,
        };
        crate::diag::trace(|| "startup: window shown".to_string());
        let _ = ShowWindow(hwnd, show);
        crate::diag::trace(|| "startup: show returned".to_string());
        SetTimer(Some(hwnd), TIMER_SLEEP, SLEEP_EVERY_MS, None);
        SetTimer(Some(hwnd), TIMER_SAVE, 5_000, None);
        SetTimer(Some(hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
    }
    crate::memory::watch_low_memory(hwnd, memory_ui::WM_APP_LOW_MEMORY);
    with_app(|app| {
        app.relayout();
        // With no tabs the field opens at once: after the first paint,
        // with its windows (refer to `first_painted`).
        app.field_after_paint = app.tabs.tabs.is_empty();
    });

    crate::diag::trace(|| "startup: first layout done".to_string());
    message_loop();
    Ok(())
}

thread_local! {
    /// The last chord the message loop ran held Alt (and not Ctrl: AltGr).
    static ALT_CHORD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A native text field (the field, the find bar, Settings): an Edit.
fn is_text_field(hwnd: HWND) -> bool {
    let mut class = [0u16; 16];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    String::from_utf16_lossy(&class[..len]).eq_ignore_ascii_case("edit")
}

/// The main window, for timers set from the message loop.
fn main_window() -> HWND {
    with_app(|app| app.hwnd).unwrap_or_default()
}

fn message_loop() {
    let mut msg = MSG::default();
    unsafe {
        loop {
            crate::diag::waiting(true);
            let more = GetMessageW(&mut msg, None, 0, 0).as_bool();
            crate::diag::waiting(false);
            if !more {
                break;
            }
            let _doing = crate::diag::doing(&"message", msg.message as u64);
            if engine_ui::hold_engine_message(main_window(), &msg) {
                continue;
            }
            if crate::settings_window::pre_translate(&msg) {
                continue;
            }
            if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
                let chord = web::current_chord(msg.wParam.0 as u32);
                // In a text field (the field, find, Settings) a word move
                // is the field's.
                let command = if chord.moves_the_caret() && is_text_field(msg.hwnd) {
                    None
                } else {
                    with_app(|app| app.shortcuts.lookup(chord)).flatten()
                };
                if let Some(command) = command {
                    ALT_CHORD.set(chord.alt && !chord.ctrl);
                    with_app(|app| app.run(command));
                    continue;
                }
                if msg.wParam.0 as u16 != VK_MENU.0 {
                    ALT_CHORD.set(false);
                }
            }
            // The Alt of an Alt chord (Alt+1): its release would be a lone
            // Alt to the window, which opens the window menu.
            if msg.message == WM_SYSKEYUP && msg.wParam.0 as u16 == VK_MENU.0 && ALT_CHORD.replace(false) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Where the window opens: the saved place when it is a sane size and still
/// on a monitor (one that was unplugged leaves it off screen), else the
/// default. The numbers come from session.json, so they can be anything. A
/// window that only touches a monitor (most of it was on one that is gone,
/// or the work area became smaller) is moved into that monitor's work area.
pub(crate) fn window_rect(saved: Option<[i32; 4]>) -> (i32, i32, i32, i32) {
    const DEFAULT: (i32, i32, i32, i32) = (CW_USEDEFAULT, CW_USEDEFAULT, 1280, 860);
    let Some([l, t, r, b]) = saved else { return DEFAULT };
    let (Some(w), Some(h)) = (r.checked_sub(l), b.checked_sub(t)) else { return DEFAULT };
    if !(200..=32_000).contains(&w) || !(200..=32_000).contains(&h) {
        return DEFAULT;
    }
    let rect = RECT { left: l, top: t, right: r, bottom: b };
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) };
    if monitor.is_invalid() {
        return DEFAULT;
    }
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return (l, t, w, h);
    }
    let fit = fit_in_work_area(rect, info.rcWork);
    (fit.left, fit.top, fit.right - fit.left, fit.bottom - fit.top)
}

/// The place a maximized window goes back to, after the scale changed from
/// `from` to `to` while it was maximized: Windows keeps it in pixels of the
/// old scale, so the window came back a size that is not its own (half of
/// it, from 100 % to 200 %). Its corner stays; `None` when nothing changes.
pub(crate) fn restored_at_scale(normal: RECT, from: f32, to: f32) -> Option<RECT> {
    if (from - to).abs() < 0.01 || from <= 0.0 {
        return None;
    }
    Some(bench_ui::scaled_window(normal, from, to))
}

fn scale_restored_place(hwnd: HWND, from: f32, to: f32) {
    let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe {
        if GetWindowPlacement(hwnd, &mut placement).is_err() {
            return;
        }
        let Some(normal) = restored_at_scale(placement.rcNormalPosition, from, to) else { return };
        placement.rcNormalPosition = normal;
        // Still maximized: only the place it goes back to changes.
        placement.showCmd = SW_SHOWMAXIMIZED.0 as u32;
        let _ = SetWindowPlacement(hwnd, &placement);
    }
}

/// How much of the top of a window must be on the work area to drag it
/// back: its height and its width, in pixels.
const GRAB_HEIGHT: i32 = 40;
const GRAB_WIDTH: i32 = 160;

/// `rect`, or `rect` moved (and made smaller if it must) into `work` when
/// its top cannot be reached there to drag it: above the work area, below
/// it, or with less than `GRAB_WIDTH` of it across.
pub(crate) fn fit_in_work_area(rect: RECT, work: RECT) -> RECT {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let across = rect.right.min(work.right) - rect.left.max(work.left);
    let top_inside = rect.top >= work.top && rect.top + GRAB_HEIGHT <= work.bottom;
    let too_large = w > work.right - work.left || h > work.bottom - work.top;
    if top_inside && across >= GRAB_WIDTH.min(w) && !too_large {
        return rect;
    }
    let w = w.min(work.right - work.left);
    let h = h.min(work.bottom - work.top);
    let left = rect.left.clamp(work.left, work.right - w);
    let top = rect.top.clamp(work.top, work.bottom - h);
    RECT { left, top, right: left + w, bottom: top + h }
}

fn create_window(session: &Session) -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("NeaWindow"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(crate::app_icon::ICON_ID as usize as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let (x, y, w, h) = window_rect(session.window);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("NeaWindow"),
            &wide(storage::DISPLAY_NAME),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

fn make_font(scale: f32) -> HFONT {
    make_font_sized(design::size::TEXT, scale)
}

/// A GDI font in the interface's face, for the native controls.
pub(crate) fn make_font_sized(px: f32, scale: f32) -> HFONT {
    make_font_weighted(px, 400, scale)
}

pub(crate) fn make_font_weighted(px: f32, weight: i32, scale: f32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -(px * scale).round() as i32,
        lfWeight: weight,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (slot, ch) in lf.lfFaceName.iter_mut().take(31).zip(gfx::family().encode_utf16()) {
        *slot = ch;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

/// The frame Windows draws around the window, made one piece with the strip:
/// dark mode when the palette is, the title bar in the chrome's own grey with
/// muted ink, and round corners (Windows 11 does that by default; asking is
/// harmless on 10, where the attributes are ignored).
///
/// No Mica: it tints the frame with the wallpaper, and the look this follows
/// is flat neutral greys where the page is the only thing with colour.
fn apply_window_theme(hwnd: HWND, palette: &Palette) {
    use design::dwm;
    dwm::set(hwnd, dwm::DARK_MODE, palette.dark as u32);
    dwm::set(hwnd, dwm::CORNERS, dwm::ROUND);
    dwm::set(hwnd, dwm::CAPTION_COLOR, palette.chrome.colorref().0);
    dwm::set(hwnd, dwm::TEXT_COLOR, palette.ink_muted.colorref().0);
    dwm::set(hwnd, dwm::BORDER_COLOR, palette.line.colorref().0);
}

fn on_omnibar(signal: Signal) {
    let _doing = crate::diag::doing(&"field", 0);
    if let Signal::Submit { .. } = signal {
        crate::diag::trace(|| "submit".to_string());
    }
    with_app(|app| app.on_omnibar(signal));
}

/// What the strip is told about a tab.
fn tab_info(t: &crate::tabs::Tab<WebTab>) -> TabInfo<'_> {
    TabInfo {
        id: t.id,
        label: t.label(),
        initial: t.initial(),
        pinned: t.pinned,
        asleep: matches!(t.live, Live::Asleep),
        loading: t.loading,
        private: t.private,
        icon: if t.private { None } else { crate::favicon::for_url(&t.url) },
        group: t.group,
    }
}

fn x_y(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    crate::diag::tick();
    if msg == crate::diag::WM_APP_PING {
        return LRESULT(0);
    }
    if let Some(result) = extensions_ui::window_message(hwnd, msg, wparam, lparam) {
        return result;
    }
    if let Some(result) = frame_ui::window_message(hwnd, msg, wparam, lparam) {
        return result;
    }
    if let Some(result) = power_ui::window_message(hwnd, msg, wparam, lparam) {
        return result;
    }
    match msg {
        web::WM_APP_EVENTS => {
            if with_app(|app| app.drain_events()).is_none() {
                retry_later(hwnd, web::WM_APP_EVENTS, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        memory_ui::WM_APP_LOW_MEMORY => {
            let _doing = crate::diag::doing(&"low memory", 0);
            with_app(|app| app.low_memory());
            LRESULT(0)
        }
        crate::notifications::WM_APP_TOAST => {
            if with_app(|app| app.toast_outcomes()).is_none() {
                retry_later(hwnd, crate::notifications::WM_APP_TOAST, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        update_ui::WM_APP_UPDATE => {
            if with_app(|app| app.update_changed()).is_none() {
                retry_later(hwnd, update_ui::WM_APP_UPDATE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        crate::bench::WM_APP_BENCH => {
            let _doing = crate::diag::doing(&"script", 0);
            bench_ui::drain();
            LRESULT(0)
        }
        WM_SIZE | WM_APP_LAYOUT => {
            let kind = if msg == WM_SIZE { wparam.0 as u32 } else { SIZE_RESTORED };
            if with_app(|app| {
                if app.window_sized(kind) {
                    app.relayout()
                }
                app.space_name_follow();
            })
            .is_none()
            {
                retry_later(hwnd, WM_APP_LAYOUT, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_MOVE => {
            with_app(|app| {
                app.session_dirty = true;
                app.autofill_follow();
                app.place_omnibar();
                app.space_name_follow();
            });
            LRESULT(0)
        }
        WM_PAINT => {
            let _doing = crate::diag::doing(&"paint", 0);
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if with_app(|app| app.paint(hdc, ps.rcPaint)).is_none() {
                retry_later(hwnd, WM_APP_REPAINT, WPARAM(0), LPARAM(0));
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            static PAINTED: std::sync::Once = std::sync::Once::new();
            PAINTED.call_once(|| {
                crate::diag::trace(|| "first paint".to_string());
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_APP_PAINTED, WPARAM(0), LPARAM(0));
                }
            });
            LRESULT(0)
        }
        WM_APP_PAINTED => {
            if with_app(|app| app.first_painted()).is_none() {
                retry_later(hwnd, WM_APP_PAINTED, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        vault_ui::WM_APP_VAULT => {
            vault_ui::on_deferred(hwnd, wparam.0);
            LRESULT(0)
        }
        crate::favicon::WM_APP_FAVICON => {
            if crate::favicon::take_arrived() {
                with_app(|app| app.invalidate_strip());
            }
            LRESULT(0)
        }
        hovercard_ui::WM_APP_THUMB => {
            with_app(|app| app.thumbs_shrunk());
            LRESULT(0)
        }
        bench_ui::WM_APP_DPI => {
            bench_ui::simulate_dpi(hwnd, wparam.0 as u32);
            LRESULT(0)
        }
        WM_APP_REPAINT => {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = x_y(lparam);
            with_app(|app| app.mouse_move(x, y));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                app.tracking_mouse = false;
                app.set_hover(None);
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = x_y(lparam);
            with_app(|app| app.mouse_down(x, y));
            LRESULT(0)
        }
        // A two-finger swipe on a precision touchpad comes as a horizontal
        // wheel, and a mouse has Shift+wheel: over the strip, both go
        // between spaces.
        WM_MOUSEHWHEEL | WM_MOUSEWHEEL => {
            let horizontal = msg == WM_MOUSEHWHEEL;
            let shift = wparam.0 & 0x0004 != 0; // MK_SHIFT
            if !horizontal && !shift {
                return default();
            }
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            let mut point =
                POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point);
            }
            match with_app(|app| app.space_wheel(point.x, point.y, delta, horizontal)) {
                Some(true) => LRESULT(0),
                _ => default(),
            }
        }
        WM_LBUTTONUP => {
            let (x, y) = x_y(lparam);
            // The keys held at the release, as the message says (a posted
            // message has no keyboard state of its own).
            let ctrl = wparam.0 & 0x0008 != 0; // MK_CONTROL
            with_app(|app| {
                app.bookmark_keys(ctrl);
                app.mouse_up(x, y)
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Our own ReleaseCapture lands here while the app is busy: ignored.
            with_app(|app| app.capture_lost());
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let own = wparam.0 == hwnd.0 as usize && (lparam.0 & 0xFFFF) as u32 == HTCLIENT;
            if own && with_app(|app| app.cursor_on_divider()).unwrap_or(false) {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
                }
                return LRESULT(1);
            }
            default()
        }
        WM_LBUTTONDBLCLK => LRESULT(0),
        WM_XBUTTONDOWN => LRESULT(1),
        WM_XBUTTONUP => {
            let which = ((wparam.0 >> 16) & 0xFFFF) as u16;
            with_app(|app| app.x_button(which));
            LRESULT(1)
        }
        WM_MBUTTONUP => {
            let (x, y) = x_y(lparam);
            with_app(|app| {
                if app.bookmark_middle_click(x, y) {
                    return;
                }
                if let Some(Hit::Tab(id) | Hit::Close(id)) = app.layout.hit(x, y) {
                    app.close_tab(id);
                }
            });
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let (x, y) = x_y(lparam);
            let _doing = crate::diag::doing(&"tab menu", 0);
            with_app(|app| app.context_menu(x, y));
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_SLEEP => {
                    let _doing = crate::diag::doing(&"timer:sleep", 0);
                    with_app(|app| app.sleep_tick());
                }
                TIMER_SAVE => {
                    let _doing = crate::diag::doing(&"timer:save", 0);
                    with_app(|app| app.save(false));
                }
                TIMER_RETRY => post_retries(hwnd),
                TIMER_FIELD_WARM => {
                    let _doing = crate::diag::doing(&"timer:field warm", 0);
                    with_app(|app| app.warm_field());
                }
                TIMER_EVENTS => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_EVENTS);
                    }
                    if with_app(|app| app.drain_events()).is_none() {
                        retry_later(hwnd, web::WM_APP_EVENTS, WPARAM(0), LPARAM(0));
                    }
                }
                TIMER_IDLE => {
                    let _doing = crate::diag::doing(&"timer:idle", 0);
                    with_app(|app| app.idle_work());
                }

                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            // It comes during a menu's or a dialog's own loop too, with the
            // app busy: done then, the change to dark or light was lost
            // until the next one. Its text is not kept, so it comes back bare.
            if with_app(|app| app.refresh_theme()).is_none() {
                retry_later(hwnd, WM_SETTINGCHANGE, WPARAM(0), LPARAM(0));
            }
            default()
        }
        WM_DPICHANGED => {
            // The new scale first: the resize below lays the window out
            // again, and with the old scale that layout was wrong for a frame.
            // A message from anything else than the system can have no DPI.
            let dpi = match (wparam.0 & 0xFFFF) as u32 {
                0 => unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) },
                dpi => dpi,
            };
            let before = with_app(|app| {
                let before = app.scale;
                app.refresh_dpi(dpi);
                before
            });
            if let Some(before) = before
                && unsafe { IsZoomed(hwnd) }.as_bool()
            {
                scale_restored_place(hwnd, before, design::scale_for_dpi(dpi));
            }
            // The system always sends a rect; a message posted by anything
            // else may not have one.
            if let Some(suggested) = unsafe { crate::msg_param::read::<RECT>(lparam) } {
                let (w, h) =
                    (suggested.right.saturating_sub(suggested.left), suggested.bottom.saturating_sub(suggested.top));
                if w > 0 && h > 0 {
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            suggested.left,
                            suggested.top,
                            w,
                            h,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
            crate::app_icon::apply(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if let Some(info) = unsafe { crate::msg_param::write::<MINMAXINFO>(lparam) } {
                let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
                let (x, y) = min_window_size(dpi);
                info.ptMinTrackSize = POINT { x, y };
            }
            LRESULT(0)
        }
        WM_NCACTIVATE => {
            // Keep the title bar looking active while our own popup has focus.
            let omnibar_open = with_app(|app| app.mode.is_some()).unwrap_or(false);
            // The window buttons go quieter in a window in the background.
            with_app(|app| app.set_window_active(wparam.0 != 0 || omnibar_open));
            if omnibar_open && wparam.0 == 0 {
                return unsafe { DefWindowProcW(hwnd, msg, WPARAM(1), lparam) };
            }
            default()
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 != WA_INACTIVE {
                with_app(|app| app.restore_focus());
            } else {
                // Another program took the keyboard: the account list goes.
                with_app(|app| app.autofill_close());
            }
            default()
        }
        WM_CLOSE => {
            let _doing = crate::diag::doing(&"close", 0);
            let shut_down = with_app(|app| app.shutdown()).is_some();
            // A modal loop inside the app (a menu) dispatches the close too:
            // destroyed then, the window went without the session written.
            if !close_now(shut_down, app_busy()) {
                retry_later(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
            crate::diag::flush_now();
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => default(),
    }
}

impl App {
    // ---------- layout and painting ----------

    fn client_size(&self) -> (i32, i32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        (rect.right - rect.left, rect.bottom - rect.top)
    }

    fn tab_infos(&self) -> Vec<TabInfo<'_>> {
        self.tabs.tabs.iter().map(tab_info).collect()
    }

    fn relayout(&mut self) {
        self.tidy_pairs();
        self.lay_out();
        self.show_active();
        self.place_omnibar();
        self.invalidate();
    }

    /// The strip's geometry for the client area of now. A minimized window
    /// keeps the last one: its client area is 0 x 0, and a layout of that
    /// size has no column. Restored, the window then went from no column to
    /// a column, which is the user showing it again, and the column slid
    /// in. `WM_SIZE` lays out again when the window comes back.
    fn lay_out(&mut self) {
        if unsafe { IsIconic(self.hwnd) }.as_bool() {
            return;
        }
        let (width, height) = self.client_size();
        let metrics = Metrics { scale: self.scale };
        self.layout = tabbar::layout_chrome(
            &self.tab_infos(),
            &self.group_infos(),
            width,
            height,
            self.vertical,
            self.collapsed,
            &metrics,
            self.extension_slots(),
            self.zoom_shown(),
            self.update.is_some(),
            self.space_mark_width(),
        );
        self.place_bookmarks();
        if self.fullscreen.on() {
            self.layout = fullscreen_ui::fullscreen_layout(width, height, self.vertical);
        }
    }

    /// The percent of the zoom chip: the zoom of the live page, when it is
    /// not 100 %.
    fn zoom_shown(&self) -> Option<u32> {
        self.tabs.active_tab().and_then(|t| tabbar::zoom_percent(t.zoom))
    }

    /// The engine changed the zoom of a view. The chip of the live page
    /// shows the new level, and flashes as a press does, so that each step
    /// of Ctrl+wheel is seen.
    fn zoom_changed(&mut self, id: u64, factor: f64) {
        let Some(tab) = self.tabs.get_mut(id) else { return };
        if tab.zoom == factor {
            return;
        }
        tab.zoom = factor;
        if self.tabs.active == Some(id) {
            self.relayout_soft();
            self.motion.press(tabbar::HoverKey::Zoom);
        }
    }

    /// Ctrl+0 and a click on the chip: the live page back to 100 %.
    fn reset_zoom(&mut self) {
        let Some(id) = self.tabs.active else { return };
        if let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live) {
            web.set_zoom(1.0);
        }
        // The engine sends no `ZoomChanged` for a change that the app makes.
        self.zoom_changed(id, 1.0);
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn hint(&self) -> Option<&str> {
        if let Some(error) = &self.env_error {
            return Some(error);
        }
        match self.tabs.active_tab() {
            None => Some("Press Ctrl+L to enter an address or search"),
            Some(_) => None,
        }
    }

    /// Draws the chrome, binding Direct2D only to the parts of `dirty` that
    /// show: a frame of the strip's motion renders the strip, and a full
    /// repaint the chrome around the pages, not the whole client area.
    fn paint(&mut self, hdc: windows::Win32::Graphics::Gdi::HDC, dirty: RECT) {
        let (width, height) = self.client_size();
        let loading = self.tabs.tabs.iter().any(|t| t.loading);
        let started = Instant::now();
        let frame = self.frame_now.take();
        let now = frame.unwrap_or(started);
        let fills = self.split_fills();
        self.motion.set_indicator(fills.iter().find(|(_, c)| *c == self.palette.accent).map(|(r, _)| *r));
        let next = self.motion.advance(&self.layout, self.tabs.active, self.hover, loading, now);
        let next = next.or(self.spaces_advance(now));
        let tabs_dx = self.paint_slide();
        let palette = self.palette;
        let mut surface = std::mem::take(&mut self.surface);
        let infos = self.tab_infos();
        let extension_initials = self.extension_initials();
        let pairs = self.split_pairs();
        let tab_groups = self.group_infos();
        let hint = self.hint().map(str::to_owned);
        let paint = tabbar::Paint {
            layout: &self.layout,
            tabs: &infos,
            active: self.tabs.active,
            palette: &palette,
            hint: hint.as_deref(),
            scale: self.scale,
            pairs: &pairs,
            tab_groups: &tab_groups,
            fills: &fills,
            extension_initials: &extension_initials,
            motion: &self.motion,
            maximized: self.is_maximized(),
            pressed: self.frame.pressed,
            window_active: !self.frame.inactive,
            nav: self.nav.state,
            tabs_dx,
        };
        let bar = crate::bookmark_bar::BarPaint {
            layout: &self.layout,
            entries: self.bookmark_entries(),
            palette: &palette,
            motion: &self.motion,
            scale: self.scale,
            held: self.bookmark_held(),
        };
        let dirty = RECT {
            left: dirty.left.max(0),
            top: dirty.top.max(0),
            right: dirty.right.min(width),
            bottom: dirty.bottom.min(height),
        };
        // With no page on it (the first paint, a view still being made),
        // the page's ground is one flat colour: GDI fills it, and Direct2D
        // draws only the chrome. A software target renders each pixel it is
        // bound to, and the ground is most of the window: repaints over a
        // view being made took 10-45 ms.
        let flat = (hint.is_none() && fills.is_empty() && self.motion.rail() >= 1.0).then(|| {
            let c = self.layout.content;
            RECT { left: c.left, top: c.top, right: c.right, bottom: c.bottom }
        });
        if dirty.right > dirty.left && dirty.bottom > dirty.top {
            for rect in gfx::visible_rects(hdc, self.hwnd, dirty) {
                let (chrome, ground) = match flat {
                    Some(content) => split_off(rect, content),
                    None => (vec![rect], None),
                };
                if let Some(ground) = ground {
                    gfx::fill_gdi(hdc, ground, self.palette.field);
                }
                for piece in chrome {
                    let drawn = surface.paint(hdc, piece, |g| {
                        tabbar::paint(g, width, height, &paint);
                        self.paint_spaces(g, &palette, &paint);
                        crate::bookmark_bar::paint(g, &bar);
                    });
                    if !drawn {
                        gfx::fill_gdi(hdc, piece, palette.chrome);
                    }
                }
            }
        }
        self.surface = surface;
        // What is on screen now is known when this paint drew every change
        // (a frame of the motion) or the whole strip; after any other paint
        // the next frame draws the strip whole.
        let sheet = self.strip_sheet();
        let whole = sheet.values().all(|p| covers(dirty, p.bounds, width, height));
        self.painted_sheet = (frame.is_some() || whole).then_some(sheet);
        // Still moving: the frame clock repaints what moves, and stops by
        // itself when nothing asks again.
        if next.is_some() || self.motion_was_running {
            let ring = self.motion.only_spinning(now) && !self.spaces_sliding();
            crate::anim::mark(if ring { "ring" } else { "strip" });
            crate::anim::frame_shown(started);
        }
        if next.is_none() && self.motion_was_running {
            crate::anim::motion_rested();
        }
        self.motion_was_running = next.is_some();
        if let Some(ms) = next {
            crate::anim::report_frame(started.elapsed());
            crate::anim::repaint_area(self.hwnd, strip_frame_area, ms);
        }
    }

    fn strip_sheet(&self) -> tabbar::Sheet {
        let loading: Vec<u64> = self.tabs.tabs.iter().filter(|t| t.loading).map(|t| t.id).collect();
        self.motion.sheet(&self.layout, self.tabs.active, &loading, self.scale)
    }

    /// The part of the window the next frame of the strip's motion draws:
    /// only what changes since the frame on screen. None: all of it.
    fn frame_area(&mut self, now: Instant) -> Option<RECT> {
        let loading = self.tabs.tabs.iter().any(|t| t.loading);
        let fills = self.split_fills();
        self.motion.set_indicator(fills.iter().find(|(_, c)| *c == self.palette.accent).map(|(r, _)| *r));
        let next = self.motion.advance(&self.layout, self.tabs.active, self.hover, loading, now);
        let next = next.or(self.spaces_advance(now));
        self.frame_now = Some(now);
        let old = self.painted_sheet.as_ref()?;
        if self.motion.rail_moving() || self.motion.indicator_moving() || self.spaces_moving() {
            return self.moving_area(&fills);
        }
        match tabbar::changed_area(old, &self.strip_sheet()) {
            Some(area) => {
                let (width, height) = self.client_size();
                Some(RECT {
                    left: (area.left.floor() as i32).max(0),
                    top: (area.top.floor() as i32).max(0),
                    right: (area.right.ceil() as i32).min(width),
                    bottom: (area.bottom.ceil() as i32).min(height),
                })
            }
            None => {
                // Nothing to draw this frame; the motion goes on.
                self.frame_now = None;
                if let Some(ms) = next {
                    crate::anim::repaint_area(self.hwnd, strip_frame_area, ms);
                }
                Some(RECT::default())
            }
        }
    }

    fn invalidate_strip(&self) {
        let strip = self.layout.strip;
        if strip.width() == 0 {
            self.invalidate();
            return;
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), Some(&strip.win()), false);
        }
    }

    /// What a frame of the strip's motion has to repaint: the strip (or the
    /// caption row when it is hidden) and the split's focus line.
    fn moving_area(&self, fills: &[(tabbar::Rect, design::Rgb)]) -> Option<RECT> {
        let strip = self.layout.strip;
        let base = if strip.width() > 0 { strip } else { self.layout.frame.caption };
        if base.width() == 0 {
            return None;
        }
        let mut area = base.win();
        for (r, _) in fills.iter().filter(|(_, c)| *c == self.palette.accent) {
            area.left = area.left.min(r.left);
            area.top = area.top.min(r.top);
            area.right = area.right.max(r.right);
            area.bottom = area.bottom.max(r.bottom);
        }
        Some(area)
    }

    fn update_title(&self) {
        let title = private_tabs::window_title(self.tabs.active_tab().map(|t| (t.label(), t.private)));
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &wide(&title));
        }
    }

    fn refresh_theme(&mut self) {
        self.motion.reduced = !design::animations_enabled();
        crate::anim::refresh_reduced();
        for tab in self.tabs.all() {
            if let Live::Ready(web) = &tab.live {
                web.apply_theme(self.settings.theme);
            }
        }
        let palette = design::palette(self.settings.theme);
        // Windows sends WM_SETTINGCHANGE for many settings (the language, the
        // environment, a policy…): the windows are coloured again only when
        // the colours change, so they do not flash and redraw for nothing.
        if crate::settings_window::palette_key(&palette) == crate::settings_window::palette_key(&self.palette) {
            return;
        }
        self.palette = palette;
        apply_window_theme(self.hwnd, &self.palette);
        self.omnibar.set_palette(&self.palette);
        crate::settings_window::set_palette(&self.palette);
        // They keep the colours they were shown with: they come back with
        // the new ones at the next hover.
        self.hide_card();
        self.hide_tip();
        self.restyle_vault();
        self.restyle_notify();
        self.invalidate();
    }

    /// The window has a new DPI. `WM_DPICHANGED` gives it.
    fn refresh_dpi(&mut self, dpi: u32) {
        self.scale = design::scale_for_dpi(dpi);
        let old = self.font;
        self.font = make_font(self.scale);
        crate::favicon::set_scale(self.scale);
        self.omnibar.set_font(self.font, self.scale);
        // Before the old font is deleted: the offer's controls still use it.
        self.restyle_vault();
        self.restyle_notify();
        unsafe {
            let _ = DeleteObject(old.into());
        }
        self.relayout();
    }

    // ---------- tab life cycle ----------

    /// A view that went out of sight with the keyboard in it would still get
    /// the keys that the person types (the next page takes the keyboard only
    /// when its slide ends). The main window holds the keyboard until then:
    /// its shortcuts continue to work, and no key goes to a hidden page.
    fn keyboard_off_hidden_views(&self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
        unsafe {
            let focus = GetFocus();
            let ours = !focus.is_invalid() && focus != self.hwnd && IsChild(self.hwnd, focus).as_bool();
            if keyboard_in_hidden_view(ours, ours && IsWindowVisible(focus).as_bool()) {
                let _ = SetFocus(Some(self.hwnd));
            }
        }
    }

    fn wake_now(&mut self, id: u64) {
        crate::diag::trace(|| format!("wake tab={id}"));
        let Some(env) = self.env.clone() else {
            self.request_environment();
            return;
        };
        let Some(tab) = self.tabs.get(id) else { return };
        if !matches!(tab.live, Live::Asleep) {
            return;
        }
        let private = tab.private;
        self.scrolls.woke(id);
        if !private && self.take_spare(id) {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.live = Live::Creating;
            }
            return;
        }
        let Some(tab) = self.tabs.get_mut(id) else { return };
        tab.live = Live::Creating;
        let created = if tab.private {
            private_tabs::create_controller(&env, self.hwnd, id)
        } else {
            web::create_controller(&env, self.hwnd, id)
        };
        if created.is_err() {
            tab.live = Live::Asleep;
        }
    }

    /// The engine starts with the first tab that needs a page, not with the
    /// window: with no tabs, no engine process runs at all. And not before
    /// the window's first paint: its start holds the UI thread for 25-30 ms,
    /// and the chrome comes first.
    fn request_environment(&mut self) {
        if self.env_requested || !self.environment_allowed() {
            return;
        }
        if !self.painted {
            self.env_after_paint = true;
            return;
        }
        self.env_requested = true;
        shield::start();
        let extensions = self.extensions_for_environment();
        if let Err(err) = web::create_environment(&storage::engine_dir(), extensions) {
            self.environment_failed(format!("Cannot start WebView2: {err}"));
        }
    }

    /// The window drew itself once: what waited for it starts.
    fn first_painted(&mut self) {
        self.painted = true;
        if std::mem::take(&mut self.field_after_paint) && self.mode.is_none() {
            self.open_omnibar(Mode::NewTab);
        } else {
            self.omnibar.realize();
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_FIELD_WARM, FIELD_WARM_AFTER_MS, None);
        }
        if std::mem::take(&mut self.env_after_paint) {
            self.request_environment();
        }
    }

    /// The field's first drawing, out of sight and before anyone asks for
    /// it: the first opening of the history panel took 15 ms more for it.
    /// Never while something moves or the field is open.
    fn warm_field(&mut self) {
        let busy = self.mode.is_some() || self.something_moves(Instant::now()) || crate::anim::stats().running;
        unsafe {
            if busy {
                SetTimer(Some(self.hwnd), TIMER_FIELD_WARM, FIELD_WARM_AGAIN_MS, None);
                return;
            }
            let _ = KillTimer(Some(self.hwnd), TIMER_FIELD_WARM);
        }
        self.omnibar.warm();
    }

    fn select(&mut self, id: u64) {
        if self.tabs.active == Some(id) {
            return;
        }
        if let Some(leaving) = self.tabs.active {
            self.capture_thumb(leaving);
            if let Some(Live::Ready(web)) = self.tabs.get(leaving).map(|t| &t.live) {
                web.read_scroll(leaving);
            }
            self.fullscreen_tab_leaves(leaving);
        }
        // A tab of a folded group (Ctrl+K, Ctrl+1…8) unfolds it.
        let unfolds = self.tabs.is_hidden(id);
        if self.tabs.select(id) {
            // The zoom chip shows the level of the new live page.
            if unfolds || self.layout.zoom.map(|z| z.percent) != self.zoom_shown() {
                self.relayout_soft();
            }
            self.show_active();
            self.warm_up_if_asleep();
            self.session_dirty = true;
            self.invalidate();
            // Moving the keyboard into the page waits on the engine's process
            // (90-170 ms measured for a page that just woke): it follows once
            // the live card has slid over.
            self.focus_when_settled(id);
        }
    }

    fn focus_page(&self) {
        let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        let front = keyboard_is_ours(foreground, &[self.hwnd, self.omnibar.popup]) || crate::bench::acts_in_front();
        if self.mode.is_some() || !front {
            return;
        }
        self.focus_page_now();
    }

    /// Gives the keyboard to the active page, or to the window while that
    /// page is being made. Only for a window that is in front.
    fn focus_page_now(&self) {
        if let Some(Live::Ready(web)) = self.tabs.active_tab().map(|t| &t.live) {
            let _doing = crate::diag::doing(&"view:focus", 0);
            web.focus();
        } else {
            // Also while the page is still being created: the keyboard leaves
            // the page shown before (beside it, in split view) instead of
            // staying there; the new page takes it once it is ready.
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
            }
        }
    }

    fn restore_focus(&self) {
        if self.mode.is_some() {
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.omnibar.edit));
            }
        } else {
            self.focus_page_now();
        }
    }

    fn open_tab(&mut self, url: &str, at: Option<usize>) {
        let id = self.tabs.insert(url, "", false, at);
        self.tabs.select(id);
        self.relayout();
        self.focus_page();
        self.session_dirty = true;
    }

    fn close_tab(&mut self, id: u64) {
        self.close_tabs(&[id]);
    }

    /// Closes the tabs, then lays out, shows the views and moves the
    /// keyboard once, for the tab that is active at the end. One at a time,
    /// each tab that became active on the way took the keyboard (a call to
    /// the engine's process) and woke its view: 4 s for 300 tabs.
    pub(super) fn close_tabs(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        for &id in ids {
            self.close_one(id);
        }
        self.relayout();
        self.focus_page();
        self.session_dirty = true;
    }

    fn close_one(&mut self, id: u64) {
        self.fullscreen_tab_leaves(id);
        self.forget_thumb(id);
        self.scrolls.forget(id);
        self.curtain.closed(id);
        self.notify_tab_closed(id);
        let heir = self.unpair_closing(id);
        self.opening_closed(id);
        if let Some(tab) = self.tabs.close(id)
            && let Live::Ready(web) = tab.live
        {
            if tab.private { self.close_private_view(web) } else { web.close() }
        }
        if let Some(partner) = heir {
            self.tabs.select(partner);
        }
    }

    fn navigate_active(&mut self, url: &str) {
        let Some(id) = self.tabs.active else {
            self.open_tab(url, None);
            return;
        };
        let Some(tab) = self.tabs.get_mut(id) else { return };
        tab.url = url.to_string();
        tab.title.clear();
        if let Live::Ready(web) = &tab.live {
            web.navigate(url);
        }
        self.show_active();
        self.focus_page();
        self.invalidate();
        self.session_dirty = true;
    }

    fn discard(&mut self, id: u64) {
        // Asleep, a private tab would lose its page and its cookies anyway.
        if self.tabs.get(id).is_some_and(|t| t.private) {
            self.close_tab(id);
            return;
        }
        self.fullscreen_tab_leaves(id);
        // Loaded again later, the page is no longer the window a page opened.
        self.popups.openings.forget_page(id);
        if let Some(tab) = self.tabs.get_mut(id) {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
            tab.loading = false;
        }
    }

    fn keeps_awake(&self, url: &str) -> bool {
        self.settings.keep_awake.contains(omnibox::display_host(url))
    }

    /// Work that would make a click wait, done ahead while the person is
    /// away: the Settings window is made hidden, so opening it only shows
    /// it. Tried again later while the person is busy or something moves.
    fn idle_work(&mut self) {
        let away = system_idle().is_some_and(|idle| idle >= Duration::from_millis(u64::from(AWAY_AFTER_MS)));
        let busy = crate::anim::stats().running || self.mode.is_some();
        // The work starts while the person is away. Its steps are a few
        // milliseconds each, so once started they also go on while the
        // person uses the window (not while something moves or the field
        // is open).
        let started = crate::settings_window::building();
        unsafe {
            if !(away || started) || busy {
                SetTimer(Some(self.hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
                return;
            }
            let _ = KillTimer(Some(self.hwnd), TIMER_IDLE);
        }
        let done = crate::settings_window::prepare(
            self.hwnd,
            &self.settings,
            &self.palette,
            apply_settings::on_settings_changed,
        );
        if !done {
            // The next step soon: the thread is free between two of them.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_IDLE, PREPARE_STEP_EVERY_MS, None);
            }
        }
    }

    /// Settings closed: its window is made again ahead of the next time.
    pub(super) fn settings_closed(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
        }
    }

    fn sleep_tick(&mut self) {
        let pressure = memory_load() >= self.settings.memory_pressure_percent;
        self.sleep_pass(pressure);
        self.idle_trim();
    }

    /// Tabs out of sight go to their next stage (lowered, suspended, then
    /// destroyed down to their metadata; refer to `memory`). Pinned and
    /// private tabs may sleep but are never destroyed; kept-awake sites are
    /// left alone; a tab playing audio is never destroyed.
    ///
    /// Each stage is a call to the engine: one tick makes one discard or a
    /// few lighter calls, the longest idle tabs first, and the next tick
    /// comes sooner while work is left.
    fn sleep_pass(&mut self, pressure: bool) {
        let now = Instant::now();
        let policy = crate::memory::Policy::new(self.settings.suspend_after_secs, self.settings.discard_after_secs);
        let minimized = self.memory.minimized();
        let mut to_discard = Vec::new();
        let mut stages = Vec::new();
        // Live views out of sight that may still be discarded: what the cap counts.
        let mut capped = Vec::new();
        // A page that waits for its sign-in pop-up stays live too.
        // So does a site that may notify: a sleeping page sends nothing.
        let keep_awake: Vec<bool> = self
            .tabs
            .all()
            .map(|t| self.keeps_awake(&t.url) || self.waits_for_popup(t.id) || self.notifies(&t.url, t.private))
            .collect();
        // Both pages of the pair on screen are in use: their idle time starts
        // when they leave the screen, or when the window is minimized. The
        // tabs of the other spaces are background tabs like any other.
        let visible = self.visible_tabs();
        for (tab, awake) in self.tabs.all_mut().zip(keep_awake) {
            let shown = visible.contains(&tab.id);
            if shown && !minimized {
                tab.last_active = now;
                continue;
            }
            let Live::Ready(web) = &tab.live else { continue };
            let facts = crate::memory::Facts {
                idle: now.duration_since(tab.last_active),
                minimized_on_screen: shown,
                keep_awake: awake,
                pinned: tab.pinned,
                private: tab.private,
                playing_audio: false,
                lowered: web.lowered,
                suspended: web.suspended,
            };
            let step = crate::memory::step(&facts, &policy, pressure);
            // A page on screen in a minimized window comes back first: it
            // keeps what draws it (refer to `memory::park_step`).
            let park = !pressure && !shown;
            match step {
                crate::memory::Step::Discard => to_discard.push((tab.last_active, tab.id, Some(facts))),
                crate::memory::Step::Keep => {}
                step => stages.push((tab.last_active, tab.id, step, park)),
            }
            if step != crate::memory::Step::Discard && !tab.pinned && !tab.private && !awake && !shown {
                capped.push((tab.last_active, tab.id));
            }
        }
        to_discard.extend(
            crate::memory::over_cap(&capped, self.settings.live_tabs_out_of_sight, now)
                .into_iter()
                .map(|(since, id)| (since, id, None)),
        );
        // A call to the engine holds the window: never while the strip or
        // the field moves, and a discard (hundreds of ms when the engine is
        // busy) only once the person paused, unless memory is low.
        let work = !to_discard.is_empty() || !stages.is_empty();
        let busy = !to_discard.is_empty() && !pressure && !quiet_for(QUIET_BEFORE_DISCARD);
        if work && (busy || self.something_moves(now)) {
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_SLEEP, SLEEP_AGAIN_MS, None);
            }
            return;
        }
        if pressure {
            self.trim_under_pressure();
        }
        to_discard.sort_by_key(|(since, ..)| *since);
        stages.sort_by_key(|(since, ..)| *since);
        let mut discarded = 0;
        let started = Instant::now();
        let mut pending = to_discard.into_iter();
        for (_, id, facts) in pending.by_ref() {
            let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live) else { continue };
            if web.is_playing_audio() {
                // A page that plays sound is never cut off; one due for
                // discard goes to the stage it may still take.
                if let Some(mut facts) = facts {
                    facts.playing_audio = true;
                    let step = crate::memory::step(&facts, &policy, pressure);
                    if step != crate::memory::Step::Keep {
                        stages.insert(0, (now, id, step, !pressure));
                    }
                }
                continue;
            }
            {
                let _doing = crate::diag::doing(&"view:close", id);
                self.discard(id);
            }
            discarded += 1;
            if discarded == DISCARDS_PER_TICK {
                break;
            }
        }
        let mut more = pending.next().is_some();
        if discarded > 0 {
            self.invalidate();
            more |= !stages.is_empty();
        } else {
            more |= stages.len() > STAGES_PER_TICK;
            for (_, id, step, park) in stages.into_iter().take(STAGES_PER_TICK) {
                if let Some(Live::Ready(web)) = self.tabs.get_mut(id).map(|t| &mut t.live) {
                    match step {
                        crate::memory::Step::Suspend => {
                            let parked = web.parked_for(now);
                            match crate::memory::park_step(park, parked) {
                                crate::memory::Park::Start => {
                                    let _doing = crate::diag::doing(&"view:park", id);
                                    web.park(now);
                                    more = true;
                                }
                                crate::memory::Park::Wait => more = true,
                                crate::memory::Park::Done => web.suspend(),
                            }
                        }
                        crate::memory::Step::Lower => web.lower(),
                        crate::memory::Step::Keep | crate::memory::Step::Discard => {}
                    }
                }
            }
        }
        let next = if more { sleep_again_ms(started.elapsed()) } else { SLEEP_EVERY_MS };
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SLEEP, next, None);
        }
    }

    // ---------- engine events ----------

    fn drain_events(&mut self) {
        let started = Instant::now();
        while let Some(event) = web::pop() {
            {
                let _doing = crate::diag::doing(event.name(), event.tab());
                self.bench.observe(&event);
                self.handle(event);
            }
            if yield_events(started.elapsed(), web::queued()) {
                // A timer comes after the paint and the input that wait.
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_EVENTS, USER_TIMER_MINIMUM, None);
                }
                break;
            }
        }
        self.bench_poll();
    }

    fn handle(&mut self, event: Event) {
        if event.generation().is_some_and(|g| g != web::generation()) {
            self.stale_engine_event(event);
            return;
        }
        match event {
            Event::EnvironmentReady(_, result) => self.environment_ready(result),
            Event::BrowserExited(_, failed) => self.browser_exited(failed),
            Event::ControllerReady(engine_ui::SPARE, _, result) => self.spare_ready(result),
            Event::ControllerReady(id, _, result) => self.view_ready(id, result),
            Event::TitleChanged(id) => {
                let records = self.tabs.records(id);
                if let Some(tab) = self.tabs.get_mut(id)
                    && let Live::Ready(web) = &tab.live
                {
                    tab.title = web.title();
                    if records {
                        self.history.set_title(&tab.url, &tab.title);
                    }
                }
                self.after_tab_change(id);
            }
            Event::SourceChanged(id) => {
                if let Some(tab) = self.tabs.get_mut(id)
                    && let Live::Ready(web) = &tab.live
                {
                    let source = web.source();
                    if !source.is_empty() {
                        tab.url = source;
                    }
                }
                self.after_tab_change(id);
            }
            Event::NavigationStarting(id) => {
                self.curtain.navigated(id);
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = true;
                }
                self.invalidate();
            }
            Event::NavigationCompleted(id, ok) => {
                self.page_arrived();
                let records = self.tabs.records(id);
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = false;
                    if ok && records {
                        self.history.record_visit(&tab.url, &tab.title, unix_now());
                    }
                }
                if ok && self.visible_tabs().contains(&id) {
                    self.capture_thumb(id);
                }
                if let Some(Live::Ready(web)) = self.tabs.get(id).map(|t| &t.live)
                    && let Some((x, y)) = self.scrolls.arrived(id, &web.source(), ok)
                {
                    web.restore_scroll(x, y);
                }
                self.invalidate();
            }
            Event::NewWindow(from, url, user_initiated, request) => self.new_window(from, url, user_initiated, request),
            Event::CloseRequested(id) => self.page_closed_itself(id),
            Event::ProcessFailed(id, _, kind) => self.process_failed(id, kind),
            Event::PageClicked(id) => self.page_focused(id),
            Event::HistoryChanged(id) => {
                if self.tabs.active == Some(id) {
                    self.refresh_nav();
                }
            }
            Event::NavHistory(id, back, json) => self.on_nav_history(id, back, &json),
            Event::FindChanged(id) => {
                if self.mode == Some(Mode::Find) && self.tabs.active == Some(id) {
                    self.show_find_status();
                }
            }
            Event::WebMessage(id, source, json) => {
                if let Some(request) = crate::webstore::store_request(&source, &json) {
                    self.store_page_request(id, request);
                    return;
                }
                // A private tab hides nothing for good: curtain.json outlives it.
                if !self.tabs.get(id).is_some_and(|t| t.private) {
                    let live = live_webs(&self.tabs);
                    self.curtain.on_message(id, &source, &json, live);
                }
            }
            Event::Command(command) => self.run(command),
            Event::FullScreen(id) => self.page_fullscreen(id),
            Event::ZoomChanged(id, factor) => self.zoom_changed(id, factor),
            Event::Download(download) => self.on_download(download),
            Event::Vault(id, message) => self.on_vault_message(id, message),
            Event::Scrolled(id, url, x, y) => {
                if self.tabs.get(id).is_some() {
                    self.scrolls.left(id, &url, x, y);
                }
            }
            Event::Extension(event) => self.on_extension_event(event),
            Event::Notify(id, message) => self.on_notify(id, message),
        }
    }

    fn after_tab_change(&mut self, id: u64) {
        if self.tabs.active == Some(id) {
            self.update_title();
        }
        self.session_dirty = true;
        self.relayout_soft();
    }

    /// Repaint the strip without touching the views (labels changed).
    fn relayout_soft(&mut self) {
        self.lay_out();
        self.invalidate();
    }

    fn controller_ready(
        &mut self,
        id: u64,
        result: windows::core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller>,
    ) {
        let started = Instant::now();
        let _traced = Traced(started, id);
        // A failure, or a view nobody waits for any more (the tab closed or
        // slept while it was made), ends here.
        let Some(controller) = self.view_arrived(id, result) else { return };
        let Some(tab) = self.tabs.get_mut(id) else {
            unsafe {
                let _ = controller.Close();
            }
            return;
        };
        let mut main_profile = None;
        match WebTab::new(id, controller, self.shortcuts.clone(), self.settings.theme) {
            Ok(mut web) => {
                // A page may ask to notify from any tab; the decisions the
                // engine kept are read from the main profile.
                let _ = crate::notify_watch::watch(&web.webview, id);
                if !tab.private {
                    main_profile = Some(web.webview.clone());
                }
                // A private tab keeps no passwords: no capture, no account menu.
                web.passwords = !tab.private;
                // A private download is not listed in the panel: downloads.json outlives it.
                if !tab.private {
                    let _ = crate::download_watch::watch(&web.webview, id);
                }
                self.curtain.install(&web, id);
                crate::webstore::watch(&web.webview);
                web.apply_privacy(self.settings.block_trackers);
                if web.passwords {
                    let _ = crate::vault_watch::watch(&web.webview, id);
                }
                if !tab.private {
                    crate::favicon::watch(&web.webview, self.hwnd);
                }
                // A new view starts at 100 %: it gets the level of the view before it.
                if tab.zoom != 1.0 {
                    web.set_zoom(tab.zoom);
                }
                let url = if tab.url.is_empty() { "about:blank".to_string() } else { tab.url.clone() };
                // A tab that a page opened gets its page from the engine.
                if !self.answer_opening(id, &web.webview) {
                    web.navigate(&url);
                }
                let Some(tab) = self.tabs.get_mut(id) else {
                    web.close();
                    return;
                };
                tab.live = Live::Ready(web);
            }
            Err(_) => {
                tab.live = Live::Asleep;
                if tab.private {
                    self.close_tab(id);
                    return;
                }
            }
        }
        if let Some(view) = main_profile {
            self.notify_import(&view);
        }
        self.extensions_view_ready();
        self.show_active();
        // Moving the keyboard waits on the engine's process: only for the
        // page that takes it, not for each view made out of sight.
        if self.tabs.active == Some(id) {
            self.focus_page();
        }
        self.invalidate();
    }

    // ---------- commands ----------

    fn run(&mut self, command: Command) {
        let _doing = crate::diag::doing(&"command", 0);
        crate::diag::trace(|| format!("command {}", command.name()));
        let started = Instant::now();
        self.run_command(command);
        // For measurements (NEA_TRACE=1): how long the command held the window.
        crate::diag::trace(|| {
            format!("command {} done in {:.3} ms", command.name(), started.elapsed().as_secs_f64() * 1000.0)
        });
    }

    fn run_command(&mut self, command: Command) {
        match command {
            Command::FocusAddress => {
                let mode = if self.tabs.active.is_some() { Mode::Address } else { Mode::NewTab };
                self.open_omnibar(mode);
            }
            Command::NewTab => self.open_omnibar(Mode::NewTab),
            Command::Fullscreen => self.toggle_fullscreen(),
            Command::ResetZoom => self.reset_zoom(),
            Command::NewPrivateTab => self.open_omnibar(Mode::NewPrivateTab),
            Command::SwitchTab => self.open_omnibar(Mode::Switch),
            Command::Find => {
                if self.tabs.active.is_some() {
                    self.open_omnibar(Mode::Find);
                }
            }
            Command::CloseTab => {
                if let Some(id) = self.tabs.active {
                    self.close_omnibar();
                    self.close_tab(id);
                }
            }
            Command::ReopenTab => {
                if let Some(closed) = self.tabs.pop_closed() {
                    let id = self.tabs.insert(&closed.url, &closed.title, false, Some(closed.index));
                    self.tabs.select(id);
                    self.relayout();
                    self.focus_page();
                }
            }
            Command::Back => self.with_active_web(|w| w.back()),
            Command::Forward => self.with_active_web(|w| w.forward()),
            Command::Reload => self.with_active_web(|w| w.reload()),
            Command::HardReload => self.with_active_web(|w| w.hard_reload()),
            Command::Downloads => self.open_panel(PanelKind::Downloads),
            Command::Bookmarks => self.open_panel(PanelKind::Bookmarks),
            Command::History => self.open_panel(PanelKind::History),
            Command::ToggleBookmark => self.toggle_bookmark(),
            Command::ToggleBookmarksBar => self.toggle_bookmarks_bar(),
            Command::Settings => self.open_settings(),
            Command::Passwords => self.open_panel(PanelKind::Passwords),
            Command::PrevTab => {
                if let Some(id) = self.tabs.neighbour(-1) {
                    self.select(id);
                }
            }
            Command::NextTab => {
                if let Some(id) = self.tabs.neighbour(1) {
                    self.select(id);
                }
            }
            Command::GoToTab(n) => {
                if let Some(id) = self.tabs.by_number(n) {
                    self.select(id);
                }
            }
            Command::ToggleTabLayout => self.set_tab_layout(!self.vertical, false),
            Command::ToggleSidebar => self.set_tab_layout(self.vertical, !self.collapsed),
            Command::DuplicateTab => {
                if let Some(tab) = self.tabs.active_tab() {
                    let (url, private) = (tab.url.clone(), tab.private);
                    let at = self.tabs.after_active();
                    if private { self.open_private_tab(&url, at) } else { self.open_tab(&url, at) }
                }
            }
            Command::CopyAddress => {
                if let Some(tab) = self.tabs.active_tab() {
                    set_clipboard(self.hwnd, &tab.url);
                }
            }
            Command::PasteAndGo => {
                if let Some(text) = get_clipboard(self.hwnd)
                    && let Some(url) = omnibox::resolve(&text, &self.settings.search_url)
                {
                    self.close_omnibar();
                    self.navigate_active(&url);
                }
            }
            Command::PinTab => {
                if let Some(id) = self.tabs.active {
                    self.toggle_pin_with_pair(id);
                    self.session_dirty = true;
                    self.relayout_soft();
                }
            }
            Command::KeepAwake => {
                // settings.json would keep the host of a private page.
                if let Some(tab) = self.tabs.active_tab().filter(|t| !t.private) {
                    let host = omnibox::display_host(&tab.url).to_string();
                    if !host.is_empty() && !self.settings.keep_awake.remove(&host) {
                        self.settings.keep_awake.insert(host);
                    }
                    self.save_settings();
                    crate::settings_window::sync(&self.settings);
                }
            }
            Command::ReaderMode => self.with_active_web(|w| reader::toggle(w)),
            Command::FloatingVideo => self.with_active_web(|w| pip::toggle(w)),
            Command::HideElement => {
                if let Some(tab) = self.tabs.active_tab().filter(|t| !t.private)
                    && let Live::Ready(web) = &tab.live
                {
                    self.curtain.toggle_picker(tab.id, web);
                }
            }
            Command::ShowHidden => {
                self.save_before_modal();
                let frame = self.omnibar_frame();
                let (x, y, width) = (frame.left, frame.top, frame.width);
                if let Some(tab) = self.tabs.active_tab()
                    && let Live::Ready(web) = &tab.live
                {
                    self.curtain.show_hidden(self.hwnd, (x + width / 2, y), web, live_webs(&self.tabs));
                }
                self.focus_page();
            }
            Command::ToggleShield => {
                // settings.json would keep the host of a private page.
                if let Some(tab) = self.tabs.active_tab().filter(|t| !t.private) {
                    let host = shield::site_key(&tab.url).to_string();
                    if !host.is_empty() && !self.settings.shield_disabled.remove(&host) {
                        self.settings.shield_disabled.insert(host);
                    }
                    shield::set_disabled(&self.settings.shield_disabled);
                    self.save_settings();
                    // An open Settings window holds a copy: its next change would undo this.
                    crate::settings_window::sync(&self.settings);
                    // Bypassing the cache: what loaded while blocking was off
                    // sits there and is served without passing the filters.
                    self.with_active_web(|w| w.hard_reload());
                }
            }
            Command::TogglePopups => self.toggle_popups(),
            Command::SplitPage => self.split_command(),
            Command::Unsplit => {
                if let Some(id) = self.tabs.active {
                    self.unsplit(id);
                }
            }
            Command::FocusLeftPage => self.focus_side(Side::Left),
            Command::FocusRightPage => self.focus_side(Side::Right),
            Command::GroupTab | Command::UngroupTab | Command::ToggleGroup => self.group_command(command),
            Command::GoToSpace(_) | Command::NextSpace | Command::PrevSpace | Command::NewSpace => {
                self.space_command(command)
            }
        }
    }

    fn with_active_web(&mut self, f: impl FnOnce(&mut WebTab)) {
        if let Some(id) = self.tabs.active
            && let Some(tab) = self.tabs.get_mut(id)
            && let Live::Ready(web) = &mut tab.live
        {
            f(web);
        }
    }

    // ---------- mouse ----------

    fn set_hover(&mut self, hover: Option<Hit>) {
        if self.hover != hover {
            self.hover = hover;
            self.card_follow_hover();
            // A click on a sleeping tab is likely next: its view gets made meanwhile.
            if let Some(Hit::Tab(id)) = hover
                && self.tabs.get(id).is_some_and(|t| matches!(t.live, Live::Asleep) && !t.private)
            {
                self.warm_up();
            }
            self.tip_follow_hover();
            self.invalidate_hover_area();
        }
    }

    /// A modal loop (a popup menu) took the mouse, and no move or leave
    /// came here meanwhile: the hover is set again from where the pointer
    /// is, and the leave is tracked anew.
    pub(super) fn rehover(&mut self) {
        let mut point = POINT::default();
        let inside = unsafe {
            GetCursorPos(&mut point).is_ok()
                && windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut point).as_bool()
        };
        self.tracking_mouse = false;
        let hover = if inside { self.layout.hit(point.x, point.y) } else { None };
        self.set_hover(hover);
    }

    /// Everything a hover can light up lives in the strip, the caption row
    /// and the window's buttons: only those repaint, never the page area.
    fn invalidate_hover_area(&self) {
        let l = &self.layout;
        let [close, min, max] = l.frame.buttons;
        let [back, forward, reload] = l.nav;
        let areas = [l.strip, l.frame.caption, l.settings, min, max, close, back, forward, reload];
        if areas.iter().all(|r| r.width() == 0) {
            self.invalidate();
            return;
        }
        for r in areas.iter().filter(|r| r.width() > 0) {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), Some(&r.win()), false);
            }
        }
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
        }
        if self.divider_drag(x) {
            return;
        }
        if self.group_mouse_move(x, y) || self.bookmark_mouse_move(x, y) {
            self.set_hover(self.layout.hit(x, y));
            return;
        }
        if let Some(dragged) = self.drag {
            self.drag_tab(dragged, x, y);
        }
        self.set_hover(self.layout.hit(x, y));
    }

    fn mouse_down(&mut self, x: i32, y: i32) {
        self.hide_card();
        self.hide_tip();
        if self.divider_mouse_down(x, y) {
            return;
        }
        match self.layout.hit(x, y) {
            Some(Hit::Tab(id)) => {
                self.split.drag_origin = self.tabs.active;
                self.close_omnibar();
                self.select(id);
                self.drag = Some(id);
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Some(Hit::Close(id)) => self.close_tab(id),
            Some(Hit::Group(group)) => self.group_mouse_down(group, x, y),
            Some(Hit::SpaceMark) => self.space_mouse_down(x, y),
            Some(Hit::NewTab) => {
                self.motion.press(tabbar::HoverKey::NewTab);
                self.open_omnibar(Mode::NewTab);
            }
            Some(Hit::Extensions) => {
                self.motion.press(tabbar::HoverKey::Extensions);
                self.invalidate();
                self.close_omnibar();
                self.extensions_menu();
            }
            Some(Hit::Settings) => {
                self.motion.press(tabbar::HoverKey::Settings);
                self.invalidate();
                self.close_omnibar();
                self.open_settings();
            }
            Some(Hit::Zoom) => {
                self.close_omnibar();
                self.reset_zoom();
            }
            Some(Hit::Update) => {
                self.motion.press(tabbar::HoverKey::Update);
                self.close_omnibar();
                self.restart_to_update();
            }
            Some(Hit::PinnedExtension(nth)) => {
                self.close_omnibar();
                self.open_pinned_extension(nth);
            }
            Some(Hit::Window(button)) => self.press_window_button(button),
            Some(Hit::Nav(button)) => self.nav_mouse_down(button),
            Some(Hit::Bookmark(index)) => self.bookmark_mouse_down(index, x, y),
            Some(Hit::BookmarksMore) => self.bookmarks_more_menu(),
            None => {
                if self.mode.is_some() {
                    self.close_omnibar();
                }
            }
        }
    }

    fn context_menu(&mut self, x: i32, y: i32) {
        if let Some(Hit::Nav(button)) = self.layout.hit(x, y) {
            self.ask_history(button);
            return;
        }
        if let Some(Hit::Group(group)) = self.layout.hit(x, y) {
            self.group_menu(x, y, group);
            return;
        }
        if let Some(Hit::SpaceMark) = self.layout.hit(x, y) {
            self.space_menu(x, y, self.tabs.space);
            return;
        }
        if let Some(Hit::Bookmark(index)) = self.layout.hit(x, y) {
            self.bookmark_menu(x, y, index);
            return;
        }
        let Some(Hit::Tab(id) | Hit::Close(id)) = self.layout.hit(x, y) else {
            if self.layout.strip.contains(x, y) && self.layout.hit(x, y).is_none() {
                self.layout_menu(x, y);
            }
            return;
        };
        let Some(tab) = self.tabs.get(id) else { return };
        let pinned = tab.pinned;
        let awake = self.keeps_awake(&tab.url);
        let site = if tab.private { "" } else { shield::site_key(&tab.url) };
        let shielded = (!site.is_empty()).then(|| !self.settings.shield_disabled.contains(site));
        // Offered in private tabs too: there the choice is not saved.
        let popups = crate::curtain::host_of(&tab.url).map(|_| self.popups_allowed_for(&tab.url, tab.private));
        let paired = self.split.splits.partner(id).is_some();
        let can_split = self.settings.split_view && !paired && self.tabs.active.is_some_and(|a| a != id);
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING, MENU_PIN, &wide(if pinned { "Unpin tab" } else { "Pin tab" }));
            let _ = AppendMenuW(menu, MF_STRING, MENU_DUPLICATE, w!("Duplicate tab"));
            let _ =
                AppendMenuW(menu, MF_STRING | check(awake), MENU_KEEP_AWAKE, w!("Keep site awake (uses more memory)"));
            if let Some(on) = shielded {
                let _ = AppendMenuW(menu, MF_STRING | check(on), MENU_SHIELD, w!("Block ads on this site"));
            }
            if let Some(on) = popups {
                let _ = AppendMenuW(menu, MF_STRING | check(on), MENU_POPUPS, w!("Allow pop-ups on this site"));
            }
            if paired {
                let _ = AppendMenuW(menu, MF_STRING, MENU_UNSPLIT, w!("Close split view"));
            } else if can_split {
                let _ = AppendMenuW(menu, MF_STRING, MENU_SPLIT, w!("Open next to the current page"));
            }
            self.append_group_items(menu, id);
            self.append_space_items(menu, id);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_TAB_LAYOUT, &wide(frame_ui::layout_label(self.vertical)));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("Close tab"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE_OTHERS, w!("Close other tabs"));
            let mut point = POINT { x, y };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut point);
            // Its time is the person reading the menu.
            self.save_before_modal();
            let modal = crate::diag::modal();
            let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, self.hwnd, None)
                .0 as usize;
            drop(modal);
            let _ = DestroyMenu(menu);
            if chosen == MENU_TAB_LAYOUT {
                self.run(Command::ToggleTabLayout);
                return;
            }
            if self.group_menu_choice(id, chosen) || self.space_menu_choice(id, chosen) {
                return;
            }
            if chosen != 0 && self.tabs.active != Some(id) && chosen != MENU_CLOSE && chosen != MENU_SPLIT {
                self.select(id);
            }
            match chosen {
                MENU_SPLIT => self.split_with(id),
                MENU_UNSPLIT => self.unsplit(id),
                MENU_PIN => {
                    self.toggle_pin_with_pair(id);
                    self.session_dirty = true;
                    self.relayout_soft();
                }
                MENU_DUPLICATE => self.run(Command::DuplicateTab),
                MENU_KEEP_AWAKE => self.run(Command::KeepAwake),
                MENU_SHIELD => self.run(Command::ToggleShield),
                MENU_POPUPS => self.run(Command::TogglePopups),
                MENU_CLOSE => self.close_tab(id),
                MENU_CLOSE_OTHERS => {
                    let others: Vec<u64> =
                        self.tabs.tabs.iter().filter(|t| t.id != id && !t.pinned).map(|t| t.id).collect();
                    self.close_tabs(&others);
                }
                _ => {}
            }
        }
    }

    // ---------- the single field ----------

    /// The column of the focused page the field may float in (screen px).
    fn omnibar_frame(&self) -> crate::omnibar::Frame {
        let content = self.focused_pane();
        let px = |v: f32| (v * self.scale).round() as i32;
        let width = px(640.0).min(content.width() - px(32.0)).max(px(240.0));
        let left = content.left + (content.width() - width) / 2;
        let gap = px(crate::omnibar::EDGE_GAP);
        let mut top = POINT { x: left, y: content.top + gap };
        let mut bottom = POINT { x: left, y: content.bottom - gap };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut top);
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut bottom);
        }
        crate::omnibar::Frame { left: top.x, top: top.y, width, bottom: bottom.y }
    }

    fn place_omnibar(&self) {
        self.place_vault_bar();
        if self.mode.is_some() {
            self.omnibar.place(self.omnibar_frame());
        }
    }

    fn open_omnibar(&mut self, mode: Mode) {
        self.autofill_close();
        self.hide_card();
        self.forget_group_target();
        if self.mode == Some(Mode::Find) {
            self.with_active_web(|w| w.find_stop());
        }
        let (cue, text, select_all) = match mode {
            Mode::Address => {
                ("Search or enter address", self.tabs.active_tab().map(|t| t.url.clone()).unwrap_or_default(), true)
            }
            Mode::NewTab => ("New tab: search or enter address", String::new(), false),
            Mode::NewPrivateTab => ("New private tab: search or enter address", String::new(), false),
            Mode::Switch => ("Switch to tab", String::new(), false),
            Mode::Find => ("Find in page", String::new(), false),
            Mode::Panel(kind) => (kind.cue(), String::new(), false),
            Mode::Split => {
                ("Split view: search, address or open tab (empty Enter: the same page)", String::new(), false)
            }
            Mode::RenameGroup(group) => {
                let (cue, name) = self.group_cue(group);
                (cue, name, true)
            }
            Mode::RenameSpace(space) => {
                let (cue, name) = self.space_cue(space);
                (cue, name, true)
            }
            Mode::RenameBookmark => {
                let (cue, name) = self.bookmark_rename_cue();
                (cue, name, true)
            }
        };
        self.mode = Some(mode);
        // With no engine and no tab (the window just opened, empty), the
        // first keystroke starts it instead: a window left open untouched
        // never loads it at all.
        let engine_in_use = self.env.is_some() || self.tabs.count() > 0;
        if matches!(mode, Mode::NewTab | Mode::Split) && engine_in_use {
            self.warm_up();
        }
        self.load_panel_data(mode);
        self.rows.clear();
        self.typed_len = text.encode_utf16().count();
        let frame = self.omnibar_frame();
        match mode {
            Mode::Panel(_) => self.omnibar.set_list_mode(panel_ui::PANEL_VISIBLE_ROWS, true),
            _ => self.omnibar.set_list_mode(usize::MAX, false),
        }
        let position = crate::omnibar::position_for(self.settings.field_position, mode == Mode::Find);
        // A list that shows at once opens with its rows: the field is
        // placed, shaped and drawn once, not empty and then again.
        let rows = match mode {
            Mode::Switch | Mode::Panel(_) | Mode::Split => self.rows_for(mode, &text, false).unwrap_or_default(),
            _ => Vec::new(),
        };
        let labels: Vec<String> = rows.iter().map(|(label, _)| label.clone()).collect();
        self.rows = rows.into_iter().map(|(_, row)| row).collect();
        self.omnibar.open(frame, position, cue, &text, select_all, &labels);
        self.invalidate();
    }

    fn close_omnibar(&mut self) {
        self.forget_group_target();
        if self.mode.take() == Some(Mode::Find) {
            self.with_active_web(|w| w.find_stop());
        }
        self.panels.release();
        self.omnibar.close();
        self.focus_page();
    }

    fn on_omnibar(&mut self, signal: Signal) {
        let Some(mode) = self.mode else { return };
        match signal {
            Signal::Cancel => self.close_omnibar(),
            Signal::Changed => {
                let text_len = self.omnibar.text().encode_utf16().count();
                let typing_forward = text_len > self.typed_len;
                self.typed_len = text_len;
                match mode {
                    Mode::Find => {
                        let term = self.omnibar.text();
                        let env = self.env.clone();
                        let id = self.tabs.active;
                        if let (Some(env), Some(id)) = (env, id) {
                            self.with_active_web(|w| {
                                w.find_start(id, &env, &term);
                            });
                        }
                        if term.is_empty() {
                            self.set_rows(Vec::new());
                        }
                    }
                    _ => {
                        if matches!(mode, Mode::NewTab | Mode::Split) {
                            // Still typing: keep the spare view for the Enter.
                            self.warm_up();
                        }
                        self.refresh_rows(typing_forward)
                    }
                }
            }
            Signal::Submit { row, shift } => self.submit(mode, row, shift),
            Signal::Delete { row } => {
                if let Mode::Panel(kind) = mode {
                    self.panel_delete(kind, row);
                }
            }
        }
    }

    fn set_rows(&mut self, rows: Vec<(String, Row)>) {
        let _doing = crate::diag::doing(&"field:rows", rows.len() as u64);
        let labels: Vec<String> = rows.iter().map(|(label, _)| label.clone()).collect();
        self.rows = rows.into_iter().map(|(_, row)| row).collect();
        {
            let _doing = crate::diag::doing(&"field:list", 0);
            self.omnibar.set_rows(&labels);
        }
        let _doing = crate::diag::doing(&"field:place", 0);
        self.omnibar.place(self.omnibar_frame());
    }

    fn refresh_rows(&mut self, typing_forward: bool) {
        let Some(mode) = self.mode else { return };
        let text = self.omnibar.text();
        if let Some(rows) = self.rows_for(mode, &text, typing_forward) {
            self.set_rows(rows);
        }
    }

    /// The rows of the field in `mode` for `text`; None for a mode without a list.
    fn rows_for(&mut self, mode: Mode, text: &str, typing_forward: bool) -> Option<Vec<(String, Row)>> {
        let rows = match mode {
            // Every space's tabs, the active space's first; another space's
            // row says which space it is in.
            Mode::Switch => self
                .tabs
                .all()
                .filter(|t| fuzzy_match(text, t.label()) || fuzzy_match(text, &t.url))
                .take(12)
                .map(|t| {
                    let host = omnibox::display_host(&t.url);
                    let label = match self.tabs.space_by_id(t.space).filter(|s| s.id != self.tabs.space) {
                        Some(space) => format!("{}    {host} · {}", t.label(), space.name),
                        None => format!("{}    {host}", t.label()),
                    };
                    (label, Row::Switch(t.id))
                })
                .collect(),
            Mode::Address | Mode::NewTab | Mode::NewPrivateTab => {
                let (start, end) = self.omnibar.selection();
                let caret_at_end = start == end && end == text.encode_utf16().count();
                let _doing = crate::diag::doing(&"field:history", 0);
                if typing_forward
                    && caret_at_end
                    && let Some(completion) = self.history.inline_completion(text)
                {
                    self.omnibar.complete_inline(text, &completion);
                }
                let history = self
                    .history
                    .suggest(text, 8, unix_now())
                    .into_iter()
                    .map(|e| {
                        let title = if e.title.is_empty() { omnibox::strip_for_match(&e.url) } else { &e.title };
                        (format!("{title}    {}", omnibox::strip_for_match(&e.url)), Row::Go(e.url.clone()))
                    })
                    .collect();
                let mut rows = self.with_bookmark_suggestions(text, history);
                if let Some(row) = self.extension_row(text) {
                    rows.insert(0, row);
                }
                rows
            }
            Mode::Panel(kind) => {
                let _doing = crate::diag::doing(&"field:panel", 0);
                self.panel_rows(kind, text)
            }
            Mode::Split => self.split_rows(text),
            Mode::Find | Mode::RenameGroup(_) | Mode::RenameSpace(_) | Mode::RenameBookmark => return None,
        };
        Some(rows)
    }

    fn show_find_status(&mut self) {
        let status = self.tabs.active_tab().and_then(|t| match &t.live {
            Live::Ready(web) => web.find_status(),
            _ => None,
        });
        let label = match status {
            Some((_, 0)) | None => "No matches".to_string(),
            Some((index, count)) => format!("{} of {count}    Enter: next · Shift+Enter: previous", index.max(1)),
        };
        self.set_rows(vec![(label, Row::Info)]);
    }

    fn submit(&mut self, mode: Mode, row: Option<usize>, shift: bool) {
        let chosen = row.and_then(|r| self.rows.get(r)).cloned();
        if let Mode::Panel(kind) = mode {
            self.panel_submit(kind, row, shift);
            return;
        }
        if mode == Mode::Split {
            self.split_submit(row);
            return;
        }
        if let Mode::RenameGroup(group) = mode {
            self.rename_submit(group);
            return;
        }
        if let Mode::RenameSpace(space) = mode {
            self.space_rename_submit(space);
            return;
        }
        if mode == Mode::RenameBookmark {
            self.bookmark_rename_submit();
            return;
        }
        if mode != Mode::Find {
            self.page_typed();
        }
        match (mode, chosen) {
            (Mode::Find, _) => {
                let id = self.tabs.active;
                if let Some(id) = id {
                    self.with_active_web(|w| w.find_step(id, !shift));
                }
            }
            (_, Some(Row::Switch(id))) => {
                self.close_omnibar();
                self.select_anywhere(id);
            }
            (Mode::Switch, _) => {
                if let Some(Row::Switch(id)) = self.rows.first().cloned() {
                    self.close_omnibar();
                    self.select_anywhere(id);
                }
            }
            (_, Some(Row::Go(url))) => self.go(mode, &url),
            (_, Some(Row::InstallExtension(id))) => {
                self.close_omnibar();
                self.install_from_store(&id, None);
            }
            (_, Some(Row::Info | Row::Panel(_))) => {}
            (_, None) => {
                if let Some(url) = omnibox::resolve(&self.omnibar.text(), &self.settings.search_url) {
                    self.go(mode, &url);
                }
            }
        }
    }

    fn go(&mut self, mode: Mode, url: &str) {
        let group = self.take_group_target();
        self.close_omnibar();
        if let (Mode::NewTab, Some(group)) = (mode, group) {
            self.open_tab_in_group(url, group);
        } else if mode == Mode::NewTab {
            let at = self.tabs.after_active();
            self.open_tab(url, at);
        } else if mode == Mode::NewPrivateTab {
            let at = self.tabs.after_active();
            self.open_private_tab(url, at);
        } else {
            self.navigate_active(url);
        }
    }

    // ---------- persistence ----------

    fn save_settings(&self) {
        storage::write_json_later(storage::data_dir().join("settings.json"), self.settings.clone());
    }

    /// The session on every tick it changed; the history, megabytes of it,
    /// at most every `HISTORY_SAVE_EVERY`. Both are written off this thread.
    fn save(&mut self, force: bool) {
        let dir = storage::data_dir();
        if self.session_dirty || force {
            storage::write_json_later(dir.join("session.json"), self.session());
            self.session_dirty = false;
        }
        let due = self.history_saved.elapsed() >= HISTORY_SAVE_EVERY;
        if self.history.dirty && (due || force) {
            storage::write_json_later(dir.join("history.json"), self.history.entries().to_vec());
            self.history.dirty = false;
            self.history_saved = Instant::now();
        }
    }

    /// Before a modal loop (a menu, a dialog): the session goes to disk now.
    /// Windows can end the session while the loop runs, and the app, busy
    /// inside it, can then write nothing new.
    fn save_before_modal(&mut self) {
        self.save(false);
    }

    fn session(&self) -> Session {
        let mut placement =
            WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe {
            let _ = GetWindowPlacement(self.hwnd, &mut placement);
        }
        // In full screen, the place the window goes back to.
        if let Some(normal) = self.fullscreen.normal_placement() {
            placement = normal;
        }
        let r = placement.rcNormalPosition;
        let (tabs, active) = private_tabs::saved_tabs(&self.tabs);
        let (spaces, active_space) = private_tabs::saved_spaces(&self.tabs);
        Session {
            tabs,
            active,
            vertical_tabs: self.vertical,
            sidebar_collapsed: self.collapsed,
            window: Some([r.left, r.top, r.right, r.bottom]),
            maximized: placement.showCmd == SW_SHOWMAXIMIZED.0 as u32,
            splits: self.saved_splits(),
            groups: private_tabs::saved_groups(&self.tabs),
            spaces,
            active_space,
        }
    }

    fn shutdown(&mut self) {
        self.notify_shutdown();
        self.save(true);
        self.release_spare();
        storage::flush_writes(Duration::from_secs(10));
        for tab in self.tabs.all_mut() {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
        }
    }
}

/// The smallest size of the window in pixels at `dpi`: the same size in
/// DIPs at each scale. A size in pixels was half as large at 200 %.
fn min_window_size(dpi: u32) -> (i32, i32) {
    let scale = design::dpi_scale(dpi.max(96));
    let px = |dips: i32| (dips as f32 * scale).round() as i32;
    (px(MIN_WINDOW_DIPS.0), px(MIN_WINDOW_DIPS.1))
}

/// The keyboard is in a child window of the main window (`in_window`) that
/// is not on screen: a hidden view, or a window inside one.
fn keyboard_in_hidden_view(in_window: bool, visible: bool) -> bool {
    in_window && !visible
}

/// Whether the app may move the keyboard into the main window: only when
/// that window or the field over it (`windows`) is in front. `SetFocus`
/// also activates the window that gets the focus, so a page that became
/// ready while Settings or another program was in front pulled the main
/// window over it.
fn keyboard_is_ours(foreground: HWND, windows: &[HWND]) -> bool {
    !foreground.is_invalid() && windows.contains(&foreground)
}

/// When the next sleep tick comes while work is left, after one that took
/// `took`: the window is free at least three quarters of the time.
fn sleep_again_ms(took: Duration) -> u32 {
    let took = took.as_millis().min(10_000) as u32;
    SLEEP_AGAIN_MS.max(took * 3)
}

/// Whether a pass over the engine events stops here, after `spent`, and
/// leaves the rest for the next one.
fn yield_events(spent: Duration, more: bool) -> bool {
    more && spent >= EVENTS_BUDGET
}

/// Traces how long wiring a new view took (on drop, so every return counts).
struct Traced(Instant, u64);

impl Drop for Traced {
    fn drop(&mut self) {
        let (took, id) = (self.0.elapsed(), self.1);
        crate::diag::trace(|| format!("view wired tab={id} in {:.1} ms", took.as_secs_f64() * 1000.0));
    }
}

/// The tabs that have a web view right now.
fn live_webs(tabs: &TabList<WebTab>) -> impl Iterator<Item = (u64, &WebTab)> {
    tabs.all().filter_map(|t| match &t.live {
        Live::Ready(web) => Some((t.id, web)),
        _ => None,
    })
}

/// Every character of the query appears in order in the text (case-insensitive).
fn fuzzy_match(query: &str, text: &str) -> bool {
    let mut chars = text.chars().flat_map(char::to_lowercase);
    query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()).all(|q| chars.any(|c| c == q))
}

fn memory_load() -> u32 {
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe { if GlobalMemoryStatusEx(&mut status).is_ok() { status.dwMemoryLoad } else { 0 } }
}

const CF_UNICODETEXT: u32 = 13;

fn set_clipboard(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let target = GlobalLock(memory) as *mut u16;
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                let _ = GlobalUnlock(memory);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn get_clipboard(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|handle| {
            let memory = HGLOBAL(handle.0);
            let source = GlobalLock(memory) as *const u16;
            if source.is_null() {
                return None;
            }
            let mut len = 0;
            while *source.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(source, len));
            let _ = GlobalUnlock(memory);
            Some(text)
        });
        let _ = CloseClipboard();
        text
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/app.rs"]
#[rustfmt::skip]
mod tests;
