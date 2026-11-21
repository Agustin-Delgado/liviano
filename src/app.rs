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
            BeginPaint, CLEARTYPE_QUALITY, CreateFontIndirectW, DeleteObject, EndPaint, HFONT, InvalidateRect,
            LOGFONTW, PAINTSTRUCT, UpdateWindow,
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData},
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        },
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

mod apply_settings;
mod bench_ui;
mod engine_ui;
mod extensions_ui;
mod frame_ui;
mod groups_ui;
mod hovercard_ui;
mod nav_ui;
mod panel_ui;
mod popups_ui;
mod private_tabs;
mod split_ui;
mod tooltip_ui;
mod vault_ui;

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
/// for seconds. A few per tick, and the next tick comes sooner.
const DISCARDS_PER_TICK: usize = 3;
const SLEEP_AGAIN_MS: u32 = 1_000;
const TIMER_SAVE: usize = 2;
const TIMER_RETRY: usize = 3;
/// Work done ahead of time while the person is away (see `idle_work`).
const TIMER_IDLE: usize = 4;
/// How long after startup, or after Settings closed, that work is tried.
const IDLE_WORK_AFTER_MS: u32 = 3_000;
/// The person is away when no key or click came for this long.
const AWAY_AFTER_MS: u32 = 1_500;
/// Losing this much history to a crash is fine; rewriting it every 5 s is not.
const HISTORY_SAVE_EVERY: Duration = Duration::from_secs(30);
const WM_APP_LAYOUT: u32 = WM_APP + 2;
const WM_APP_REPAINT: u32 = WM_APP + 3;
/// Gives the page the keyboard after a switch (see `select`).
const WM_APP_FOCUS: u32 = WM_APP + 4;
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
    history_saved: Instant,
    panels: panel_ui::Panels,
    curtain: Curtain,
    split: split_ui::SplitUi,
    groups: groups_ui::GroupsUi,
    bench: bench_ui::BenchState,
    vault: vault_ui::Vault,
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
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Messages that found the app busy, waiting for `TIMER_RETRY`.
    static RETRY: RefCell<Vec<(HWND, u32, WPARAM, LPARAM)>> = const { RefCell::new(Vec::new()) };
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

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn wide(text: &str) -> HSTRING {
    HSTRING::from(text)
}

pub fn run() -> windows::core::Result<()> {
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

    let hwnd = create_window(&session)?;
    crate::app_icon::apply(hwnd);
    frame_ui::install(hwnd, session.vertical_tabs, session.sidebar_collapsed);
    web::set_window(hwnd);
    crate::diag::set_window(hwnd);
    crate::updater::start(&settings);
    let bench_on = crate::bench::wanted(settings.bench_enabled);

    let mut omnibar = Box::new(Omnibar::create(hwnd, on_omnibar)?);
    crate::omnibar::register(&omnibar);

    let palette = design::palette(settings.theme);
    let scale = design::window_scale(hwnd);
    let font = make_font(scale);
    crate::favicon::set_scale(scale);
    omnibar.set_font(font, scale);
    omnibar.set_palette(&palette);
    apply_window_theme(hwnd, &palette);

    let mut tabs = TabList::default();
    let restored: Vec<(u64, Option<usize>)> = session
        .tabs
        .iter()
        .map(|saved| (tabs.insert(&saved.url, &saved.title, saved.pinned, None), saved.group))
        .collect();
    // Selected after the groups are back: a live tab in a folded group unfolds it.
    tabs.restore_groups(&session.groups, &restored);
    let restored: Vec<u64> = restored.into_iter().map(|(id, _)| id).collect();
    if let Some(&id) = restored.get(session.active.min(restored.len().saturating_sub(1))) {
        tabs.select(id);
    }
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
        history_saved: Instant::now(),
        panels: Default::default(),
        curtain: Curtain::load(&data),
        split: split_ui::SplitUi::new(splits),
        groups: Default::default(),
        bench: Default::default(),
        vault: Default::default(),
        extensions: extensions_ui::Extensions::load(),
        popups: Default::default(),
        frame: Default::default(),
        nav: Default::default(),
        motion_was_running: false,
        frame_now: None,
        painted_sheet: None,
        engine: Default::default(),
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
        let _ = ShowWindow(hwnd, show);
        SetTimer(Some(hwnd), TIMER_SLEEP, SLEEP_EVERY_MS, None);
        SetTimer(Some(hwnd), TIMER_SAVE, 5_000, None);
        SetTimer(Some(hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
    }
    with_app(|app| {
        app.relayout();
        if app.tabs.tabs.is_empty() {
            app.open_omnibar(Mode::NewTab);
        }
    });

    message_loop();
    Ok(())
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
            if crate::settings_window::pre_translate(&msg) {
                continue;
            }
            if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
                let chord = web::current_chord(msg.wParam.0 as u32);
                let command = with_app(|app| app.shortcuts.lookup(chord)).flatten();
                if let Some(command) = command {
                    with_app(|app| app.run(command));
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
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
        let (x, y, w, h) = match session.window {
            Some([l, t, r, b]) if r - l > 200 && b - t > 200 => (l, t, r - l, b - t),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1280, 860),
        };
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
    match msg {
        web::WM_APP_EVENTS => {
            if with_app(|app| app.drain_events()).is_none() {
                retry_later(hwnd, web::WM_APP_EVENTS, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        crate::bench::WM_APP_BENCH => {
            let _doing = crate::diag::doing(&"script", 0);
            bench_ui::drain();
            LRESULT(0)
        }
        WM_SIZE | WM_APP_LAYOUT => {
            if with_app(|app| app.relayout()).is_none() {
                retry_later(hwnd, WM_APP_LAYOUT, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_MOVE => {
            with_app(|app| {
                app.session_dirty = true;
                app.place_omnibar();
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
        WM_APP_FOCUS => {
            // Only if the switch it was posted for still holds.
            with_app(|app| {
                if app.tabs.active == Some(wparam.0 as u64) {
                    app.focus_page();
                }
            });
            LRESULT(0)
        }
        hovercard_ui::WM_APP_THUMB => {
            with_app(|app| app.thumbs_shrunk());
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
        WM_LBUTTONUP => {
            let (x, y) = x_y(lparam);
            with_app(|app| app.mouse_up(x, y));
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
                TIMER_IDLE => {
                    let _doing = crate::diag::doing(&"timer:idle", 0);
                    with_app(|app| app.idle_work());
                }

                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            with_app(|app| app.refresh_theme());
            default()
        }
        WM_DPICHANGED => {
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            crate::app_icon::apply(hwnd);
            with_app(|app| app.refresh_dpi());
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: 420, y: 300 };
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
            }
            default()
        }
        WM_CLOSE => {
            let _doing = crate::diag::doing(&"close", 0);
            with_app(|app| app.shutdown());
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
        self.tabs
            .tabs
            .iter()
            .map(|t| TabInfo {
                id: t.id,
                label: t.label(),
                initial: t.initial(),
                pinned: t.pinned,
                asleep: matches!(t.live, Live::Asleep),
                loading: t.loading,
                private: t.private,
                icon: if t.private { None } else { crate::favicon::for_url(&t.url) },
                group: t.group,
            })
            .collect()
    }

    fn relayout(&mut self) {
        self.tidy_pairs();
        let (width, height) = self.client_size();
        let metrics = Metrics { scale: self.scale };
        self.layout = tabbar::layout_with(
            &self.tab_infos(),
            &self.group_infos(),
            width,
            height,
            self.vertical,
            self.collapsed,
            &metrics,
            self.extension_slots(),
        );
        self.show_active();
        self.place_omnibar();
        self.invalidate();
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
            palette: &self.palette,
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
        };
        let dirty = RECT {
            left: dirty.left.max(0),
            top: dirty.top.max(0),
            right: dirty.right.min(width),
            bottom: dirty.bottom.min(height),
        };
        if dirty.right > dirty.left && dirty.bottom > dirty.top {
            for rect in gfx::visible_rects(hdc, self.hwnd, dirty) {
                if !surface.paint(hdc, rect, |g| tabbar::paint(g, width, height, &paint)) {
                    gfx::fill_gdi(hdc, rect, self.palette.chrome);
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
            crate::anim::mark("strip");
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
        self.frame_now = Some(now);
        let old = self.painted_sheet.as_ref()?;
        if self.motion.rail_moving() || self.motion.indicator_moving() {
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
        self.palette = design::palette(self.settings.theme);
        apply_window_theme(self.hwnd, &self.palette);
        self.omnibar.set_palette(&self.palette);
        crate::settings_window::set_palette(&self.palette);
        for tab in &self.tabs.tabs {
            if let Live::Ready(web) = &tab.live {
                web.apply_theme(self.settings.theme);
            }
        }
        self.invalidate();
    }

    fn refresh_dpi(&mut self) {
        self.scale = design::window_scale(self.hwnd);
        let old = self.font;
        self.font = make_font(self.scale);
        crate::favicon::set_scale(self.scale);
        self.omnibar.set_font(self.font, self.scale);
        unsafe {
            let _ = DeleteObject(old.into());
        }
        self.relayout();
    }

    // ---------- tab life cycle ----------

    /// Shows the active tab's view (and its partner's, when split), creating
    /// them if asleep, and hides the rest.
    fn show_active(&mut self) {
        let shown = self.shown_panes();
        let mut wake = Vec::new();
        for tab in &mut self.tabs.tabs {
            let bounds = shown.iter().find(|(id, _)| *id == tab.id).map(|(_, r)| *r);
            match (&mut tab.live, bounds) {
                (Live::Ready(web), Some(bounds)) => {
                    let _doing = crate::diag::doing(&"view:show", tab.id);
                    web.show(bounds)
                }
                (Live::Ready(web), None) => {
                    let _doing = crate::diag::doing(&"view:hide", tab.id);
                    web.hide()
                }
                (Live::Asleep, Some(_)) => wake.push(tab.id),
                _ => {}
            }
        }
        for id in wake {
            self.wake(id);
        }
        self.update_title();
        self.place_vault_bar();
        self.refresh_nav();
    }

    fn wake(&mut self, id: u64) {
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
    /// window: with no tabs, no engine process runs at all.
    fn request_environment(&mut self) {
        if self.env_requested {
            return;
        }
        self.env_requested = true;
        shield::start();
        let extensions = self.extensions_for_environment();
        if let Err(err) = web::create_environment(&storage::engine_dir(), extensions) {
            self.env_error = Some(format!("Cannot start WebView2: {err}"));
            self.invalidate();
        }
    }

    fn select(&mut self, id: u64) {
        if self.tabs.active == Some(id) {
            return;
        }
        if let Some(leaving) = self.tabs.active {
            self.capture_thumb(leaving);
        }
        // A tab of a folded group (Ctrl+K, Ctrl+1…8) unfolds it.
        let unfolds = self.tabs.is_hidden(id);
        if self.tabs.select(id) {
            if unfolds {
                self.relayout_soft();
            }
            self.show_active();
            self.warm_up_if_asleep();
            self.session_dirty = true;
            self.invalidate();
            // Moving the keyboard into the page waits on the engine's process
            // (90-170 ms measured for a page that just woke): the strip shows
            // the switch first, and the keyboard follows in a message of its own.
            unsafe {
                let _ = UpdateWindow(self.hwnd);
                let _ = PostMessageW(Some(self.hwnd), WM_APP_FOCUS, WPARAM(id as usize), LPARAM(0));
            }
        }
    }

    fn focus_page(&self) {
        if self.mode.is_some() || !crate::omnibar::owns_foreground() {
            return;
        }
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
            self.focus_page();
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
        self.forget_thumb(id);
        let heir = self.unpair_closing(id);
        if let Some(tab) = self.tabs.close(id)
            && let Live::Ready(web) = tab.live
        {
            if tab.private { self.close_private_view(web) } else { web.close() }
        }
        if let Some(partner) = heir {
            self.tabs.select(partner);
        }
        self.relayout();
        self.focus_page();
        self.session_dirty = true;
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

    /// Background tabs: suspended after a while, then destroyed down to their
    /// metadata. Pinned and private tabs may be suspended but never destroyed;
    /// kept-awake sites are left alone; a tab playing audio is never destroyed.
    /// Work that would make a click wait, done ahead while the person is
    /// away: the Settings window is made hidden, so opening it only shows
    /// it. Tried again later while the person is busy or something moves.
    fn idle_work(&mut self) {
        let away = unsafe {
            let mut info = windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO {
                cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            windows::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo(&mut info).as_bool()
                && windows::Win32::System::SystemInformation::GetTickCount().wrapping_sub(info.dwTime) >= AWAY_AFTER_MS
        };
        let busy = crate::anim::stats().running || self.mode.is_some();
        unsafe {
            if !away || busy {
                SetTimer(Some(self.hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
                return;
            }
            let _ = KillTimer(Some(self.hwnd), TIMER_IDLE);
        }
        if !crate::settings_window::prepared() {
            crate::settings_window::prepare(
                self.hwnd,
                &self.settings,
                &self.palette,
                apply_settings::on_settings_changed,
            );
        }
    }

    /// Settings closed: its window is made again ahead of the next time.
    pub(super) fn settings_closed(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_IDLE, IDLE_WORK_AFTER_MS, None);
        }
    }

    fn sleep_tick(&mut self) {
        let now = Instant::now();
        let suspend_after = Duration::from_secs(self.settings.suspend_after_secs);
        let discard_after = Duration::from_secs(self.settings.discard_after_secs);
        let pressure = memory_load() >= self.settings.memory_pressure_percent;
        let mut to_discard = Vec::new();
        let keep_awake: Vec<bool> = self.tabs.tabs.iter().map(|t| self.keeps_awake(&t.url)).collect();
        // Both pages of the pair on screen are in use: their idle time starts
        // when they leave the screen.
        let visible = self.visible_tabs();
        for (tab, awake) in self.tabs.tabs.iter_mut().zip(keep_awake) {
            if visible.contains(&tab.id) {
                tab.last_active = now;
                continue;
            }
            if awake {
                continue;
            }
            let Live::Ready(web) = &mut tab.live else { continue };
            let idle = now.duration_since(tab.last_active);
            if !tab.pinned && !tab.private && (pressure || idle >= discard_after) && !web.is_playing_audio() {
                to_discard.push((tab.last_active, tab.id));
            } else if !web.suspended && idle >= suspend_after {
                web.suspend();
            }
        }
        let more = take_first_discards(&mut to_discard, DISCARDS_PER_TICK);
        if !to_discard.is_empty() {
            for (_, id) in to_discard {
                self.discard(id);
            }
            self.invalidate();
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SLEEP, if more { SLEEP_AGAIN_MS } else { SLEEP_EVERY_MS }, None);
        }
    }

    // ---------- engine events ----------

    fn drain_events(&mut self) {
        while let Some(event) = web::pop() {
            let _doing = crate::diag::doing(event.name(), event.tab());
            self.bench.observe(&event);
            self.handle(event);
        }
        self.bench_poll();
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::EnvironmentReady(Ok(env)) => {
                self.env = Some(env);
                self.show_active();
                self.warm_environment_ready();
            }
            Event::EnvironmentReady(Err(err)) => {
                self.env_error = Some(format!("Cannot start WebView2 ({err}). Install the WebView2 Runtime."));
                self.invalidate();
            }
            Event::ControllerReady(engine_ui::SPARE, result) => self.spare_ready(result),
            Event::ControllerReady(id, result) => self.controller_ready(id, result),
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
                self.invalidate();
            }
            Event::NewWindow(from, url, user_initiated) => self.new_window(from, url, user_initiated),
            Event::ProcessFailed(id, kind) => self.process_failed(id, kind),
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
                // A private tab hides nothing for good: curtain.json outlives it.
                if !self.tabs.get(id).is_some_and(|t| t.private) {
                    let live = live_webs(&self.tabs);
                    self.curtain.on_message(id, &source, &json, live);
                }
            }
            Event::Command(command) => self.run(command),
            Event::Download(download) => self.on_download(download),
            Event::Vault(id, message) => self.on_vault_message(id, message),
            Event::Extension(event) => self.on_extension_event(event),
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
        let (width, height) = self.client_size();
        let metrics = Metrics { scale: self.scale };
        self.layout = tabbar::layout_with(
            &self.tab_infos(),
            &self.group_infos(),
            width,
            height,
            self.vertical,
            self.collapsed,
            &metrics,
            self.extension_slots(),
        );
        self.invalidate();
    }

    fn controller_ready(
        &mut self,
        id: u64,
        result: windows::core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller>,
    ) {
        let started = Instant::now();
        let _traced = Traced(started, id);
        let Ok(controller) = result else {
            if let Some(tab) = self.tabs.get_mut(id) {
                tab.live = Live::Asleep;
                if tab.private {
                    self.close_tab(id);
                }
            }
            return;
        };
        let Some(tab) = self.tabs.get_mut(id) else {
            // The tab was closed while its view was being created.
            unsafe {
                let _ = controller.Close();
            }
            return;
        };
        match WebTab::new(id, controller, self.shortcuts.clone(), self.settings.theme) {
            Ok(mut web) => {
                // A private tab keeps no passwords: no capture, no account menu.
                web.passwords = !tab.private;
                // A private download is not listed in the panel: downloads.json outlives it.
                if !tab.private {
                    let _ = crate::download_watch::watch(&web.webview, id);
                }
                self.curtain.install(&web, id);
                web.apply_privacy(self.settings.block_trackers);
                if web.passwords {
                    let _ = crate::vault_watch::watch(&web.webview, id);
                }
                if !tab.private {
                    crate::favicon::watch(&web.webview, self.hwnd);
                }
                let url = if tab.url.is_empty() { "about:blank".to_string() } else { tab.url.clone() };
                web.navigate(&url);
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
        self.extensions_view_ready();
        self.show_active();
        self.focus_page();
        self.invalidate();
    }

    // ---------- commands ----------

    fn run(&mut self, command: Command) {
        let _doing = crate::diag::doing(&"command", 0);
        crate::diag::trace(|| format!("command {}", command.name()));
        match command {
            Command::FocusAddress => {
                let mode = if self.tabs.active.is_some() { Mode::Address } else { Mode::NewTab };
                self.open_omnibar(mode);
            }
            Command::NewTab => self.open_omnibar(Mode::NewTab),
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
        if self.group_mouse_move(x, y) {
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
            Some(Hit::NewTab) => {
                self.motion.press(tabbar::HoverKey::NewTab);
                self.open_omnibar(Mode::NewTab);
            }
            Some(Hit::Extensions) => {
                self.motion.press(tabbar::HoverKey::Extensions);
                self.close_omnibar();
                self.extensions_menu();
            }
            Some(Hit::Settings) => {
                self.motion.press(tabbar::HoverKey::Settings);
                self.invalidate();
                self.close_omnibar();
                self.open_settings();
            }
            Some(Hit::PinnedExtension(nth)) => {
                self.close_omnibar();
                self.open_pinned_extension(nth);
            }
            Some(Hit::Window(button)) => self.press_window_button(button),
            Some(Hit::Nav(button)) => self.nav_mouse_down(button),
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
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_TAB_LAYOUT, &wide(frame_ui::layout_label(self.vertical)));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("Close tab"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE_OTHERS, w!("Close other tabs"));
            let mut point = POINT { x, y };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut point);
            // Its time is the person reading the menu.
            let modal = crate::diag::modal();
            let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, self.hwnd, None)
                .0 as usize;
            drop(modal);
            let _ = DestroyMenu(menu);
            if chosen == MENU_TAB_LAYOUT {
                self.run(Command::ToggleTabLayout);
                return;
            }
            if self.group_menu_choice(id, chosen) {
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
                    for other in others {
                        self.close_tab(other);
                    }
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
        };
        self.mode = Some(mode);
        // With no engine and no tab (the window just opened, empty), the
        // first keystroke starts it instead: a window left open untouched
        // never loads it at all.
        let engine_in_use = self.env.is_some() || !self.tabs.tabs.is_empty();
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
        self.omnibar.open(frame, position, cue, &text, select_all);
        if matches!(mode, Mode::Switch | Mode::Panel(_) | Mode::Split) {
            self.refresh_rows(false);
        }
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
        let rows = match mode {
            Mode::Switch => self
                .tabs
                .tabs
                .iter()
                .filter(|t| fuzzy_match(&text, t.label()) || fuzzy_match(&text, &t.url))
                .take(12)
                .map(|t| (format!("{}    {}", t.label(), omnibox::display_host(&t.url)), Row::Switch(t.id)))
                .collect(),
            Mode::Address | Mode::NewTab | Mode::NewPrivateTab => {
                let (start, end) = self.omnibar.selection();
                let caret_at_end = start == end && end == text.encode_utf16().count();
                let _doing = crate::diag::doing(&"field:history", 0);
                if typing_forward
                    && caret_at_end
                    && let Some(completion) = self.history.inline_completion(&text)
                {
                    self.omnibar.complete_inline(&text, &completion);
                }
                let history = self
                    .history
                    .suggest(&text, 8, unix_now())
                    .into_iter()
                    .map(|e| {
                        let title = if e.title.is_empty() { omnibox::strip_for_match(&e.url) } else { &e.title };
                        (format!("{title}    {}", omnibox::strip_for_match(&e.url)), Row::Go(e.url.clone()))
                    })
                    .collect();
                let mut rows = self.with_bookmark_suggestions(&text, history);
                if let Some(row) = self.extension_row(&text) {
                    rows.insert(0, row);
                }
                rows
            }
            Mode::Panel(kind) => {
                let _doing = crate::diag::doing(&"field:panel", 0);
                self.panel_rows(kind, &text)
            }
            Mode::Split => self.split_rows(&text),
            Mode::Find | Mode::RenameGroup(_) => return,
        };
        self.set_rows(rows);
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
        match (mode, chosen) {
            (Mode::Find, _) => {
                let id = self.tabs.active;
                if let Some(id) = id {
                    self.with_active_web(|w| w.find_step(id, !shift));
                }
            }
            (_, Some(Row::Switch(id))) => {
                self.close_omnibar();
                self.select(id);
            }
            (Mode::Switch, _) => {
                if let Some(Row::Switch(id)) = self.rows.first().cloned() {
                    self.close_omnibar();
                    self.select(id);
                }
            }
            (_, Some(Row::Go(url))) => self.go(mode, &url),
            (_, Some(Row::InstallExtension(id))) => {
                self.close_omnibar();
                self.install_from_store(&id);
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

    fn session(&self) -> Session {
        let mut placement =
            WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe {
            let _ = GetWindowPlacement(self.hwnd, &mut placement);
        }
        let r = placement.rcNormalPosition;
        let (tabs, active) = private_tabs::saved_tabs(&self.tabs);
        Session {
            tabs,
            active,
            vertical_tabs: self.vertical,
            sidebar_collapsed: self.collapsed,
            window: Some([r.left, r.top, r.right, r.bottom]),
            maximized: placement.showCmd == SW_SHOWMAXIMIZED.0 as u32,
            splits: self.saved_splits(),
            groups: private_tabs::saved_groups(&self.tabs),
        }
    }

    fn shutdown(&mut self) {
        self.save(true);
        self.release_spare();
        storage::flush_writes(Duration::from_secs(10));
        for tab in &mut self.tabs.tabs {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
        }
    }
}

/// Keeps the `limit` longest idle of `candidates` (idle since, tab); whether
/// some were left for the next tick.
fn take_first_discards(candidates: &mut Vec<(Instant, u64)>, limit: usize) -> bool {
    candidates.sort_by_key(|(since, _)| *since);
    let more = candidates.len() > limit;
    candidates.truncate(limit);
    more
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
    tabs.tabs.iter().filter_map(|t| match &t.live {
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
