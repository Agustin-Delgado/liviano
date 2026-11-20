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
        Graphics::{
            Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute},
            Gdi::{
                BeginPaint, CLEARTYPE_QUALITY, CreateFontIndirectW, DeleteObject, EndPaint, HFONT, InvalidateRect,
                LOGFONTW, PAINTSTRUCT,
            },
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData},
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
        },
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
            },
            Controls::WM_MOUSELEAVE,
            WindowsAndMessaging::*,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

mod apply_settings;
mod bench_ui;
mod extensions_ui;
mod panel_ui;
mod private_tabs;
mod split_ui;
mod vault_ui;

use crate::{
    curtain::Curtain,
    design::{self, Palette},
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
const TIMER_SAVE: usize = 2;
const TIMER_RETRY: usize = 3;
const WM_APP_LAYOUT: u32 = WM_APP + 2;
const WM_APP_REPAINT: u32 = WM_APP + 3;
const MENU_PIN: usize = 10;
const MENU_DUPLICATE: usize = 11;
const MENU_KEEP_AWAKE: usize = 12;
const MENU_CLOSE: usize = 13;
const MENU_CLOSE_OTHERS: usize = 14;
const MENU_SHIELD: usize = 15;
const MENU_SPLIT: usize = 40;
const MENU_UNSPLIT: usize = 41;

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
    omnibar: Box<Omnibar>,
    mode: Option<Mode>,
    rows: Vec<Row>,
    typed_len: usize,
    hover: Option<Hit>,
    tracking_mouse: bool,
    drag: Option<u64>,
    session_dirty: bool,
    panels: panel_ui::Panels,
    curtain: Curtain,
    split: split_ui::SplitUi,
    bench: bench_ui::BenchState,
    vault: vault_ui::Vault,
    extensions: extensions_ui::Extensions,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Messages that found the app busy, waiting for `TIMER_RETRY`.
    static RETRY: RefCell<Vec<(HWND, u32, WPARAM, LPARAM)>> = const { RefCell::new(Vec::new()) };
}

/// The app is busy — a menu's modal loop runs inside it, and dispatches
/// our messages meanwhile —: the message comes back shortly. Posting it
/// again at once spun the loop at full CPU for as long as the menu was open.
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
    let history = History::from_entries(storage::read_json::<Vec<Entry>>(&data.join("history.json")));
    shield::set_disabled(&settings.shield_disabled);

    let hwnd = create_window(&session)?;
    web::set_window(hwnd);
    crate::updater::start(&settings);
    let bench_on = crate::bench::wanted(settings.bench_enabled);

    let mut omnibar = Box::new(Omnibar::create(hwnd, on_omnibar)?);
    crate::omnibar::register(&omnibar);

    let palette = design::palette(settings.theme);
    let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
    let font = make_font(scale);
    omnibar.set_font(font, scale);
    omnibar.set_palette(&palette);
    apply_window_theme(hwnd, &palette);

    let mut tabs = TabList::default();
    for saved in &session.tabs {
        tabs.insert(&saved.url, &saved.title, saved.pinned, None);
    }
    if let Some(tab) = tabs.tabs.get(session.active.min(tabs.tabs.len().saturating_sub(1))) {
        let id = tab.id;
        tabs.select(id);
    }
    let restored: Vec<u64> = tabs.tabs.iter().map(|t| t.id).collect();
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
        omnibar,
        mode: None,
        rows: Vec::new(),
        typed_len: 0,
        hover: None,
        tracking_mouse: false,
        drag: None,
        session_dirty: false,
        panels: Default::default(),
        curtain: Curtain::load(&data),
        split: split_ui::SplitUi::new(splits),
        bench: Default::default(),
        vault: Default::default(),
        extensions: extensions_ui::Extensions::load(),
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
        SetTimer(Some(hwnd), TIMER_SLEEP, 15_000, None);
        SetTimer(Some(hwnd), TIMER_SAVE, 5_000, None);
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
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
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
            lpszClassName: w!("LivianoWindow"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(instance), PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let (x, y, w, h) = match session.window {
            Some([l, t, r, b]) if r - l > 200 && b - t > 200 => (l, t, r - l, b - t),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1280, 860),
        };
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("LivianoWindow"),
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
    let mut lf = LOGFONTW {
        lfHeight: -(13.0 * scale).round() as i32,
        lfWeight: 400,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (slot, ch) in lf.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *slot = ch;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

fn apply_window_theme(hwnd: HWND, palette: &Palette) {
    // DWMWA_USE_IMMERSIVE_DARK_MODE
    let dark: i32 = palette.dark as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(20),
            &dark as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

fn on_omnibar(signal: Signal) {
    with_app(|app| app.on_omnibar(signal));
}

fn x_y(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let default = || unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    if let Some(result) = extensions_ui::window_message(hwnd, msg, wparam, lparam) {
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
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            if with_app(|app| app.paint(hdc)).is_none() {
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
            with_app(|app| app.context_menu(x, y));
            LRESULT(0)
        }
        WM_TIMER => {
            match wparam.0 {
                TIMER_SLEEP => {
                    with_app(|app| app.sleep_tick());
                }
                TIMER_SAVE => {
                    with_app(|app| app.save(false));
                }
                TIMER_RETRY => post_retries(hwnd),
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
            with_app(|app| app.shutdown());
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
            })
            .collect()
    }

    fn relayout(&mut self) {
        self.tidy_pairs();
        let (width, height) = self.client_size();
        let metrics = Metrics { scale: self.scale };
        self.layout = tabbar::layout_with(
            &self.tab_infos(),
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
            None => Some("Ctrl+L para escribir una dirección o buscar"),
            Some(_) => None,
        }
    }

    fn paint(&mut self, hdc: windows::Win32::Graphics::Gdi::HDC) {
        let (width, height) = self.client_size();
        let infos = self.tab_infos();
        let extension_initials = self.extension_initials();
        tabbar::paint(
            hdc,
            width,
            height,
            &tabbar::Paint {
                layout: &self.layout,
                tabs: &infos,
                active: self.tabs.active,
                hover: self.hover,
                palette: &self.palette,
                font: self.font,
                hint: self.hint(),
                scale: self.scale,
                groups: &self.split_groups(),
                fills: &self.split_fills(),
                extension_initials: &extension_initials,
            },
        );
    }

    fn update_title(&self) {
        let title = private_tabs::window_title(self.tabs.active_tab().map(|t| (t.label(), t.private)));
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &wide(&title));
        }
    }

    fn refresh_theme(&mut self) {
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
        self.scale = unsafe { GetDpiForWindow(self.hwnd) } as f32 / 96.0;
        let old = self.font;
        self.font = make_font(self.scale);
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
                (Live::Ready(web), Some(bounds)) => web.show(bounds),
                (Live::Ready(web), None) => web.hide(),
                (Live::Asleep, Some(_)) => wake.push(tab.id),
                _ => {}
            }
        }
        for id in wake {
            self.wake(id);
        }
        self.update_title();
        self.place_vault_bar();
    }

    fn wake(&mut self, id: u64) {
        let Some(env) = self.env.clone() else {
            self.request_environment();
            return;
        };
        let Some(tab) = self.tabs.get_mut(id) else { return };
        if !matches!(tab.live, Live::Asleep) {
            return;
        }
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
            self.env_error = Some(format!("No se pudo iniciar WebView2: {err}"));
            self.invalidate();
        }
    }

    fn select(&mut self, id: u64) {
        if self.tabs.active == Some(id) {
            return;
        }
        if self.tabs.select(id) {
            self.show_active();
            self.focus_page();
            self.session_dirty = true;
            self.invalidate();
        }
    }

    fn focus_page(&self) {
        if self.mode.is_some() || !crate::omnibar::owns_foreground() {
            return;
        }
        if let Some(Live::Ready(web)) = self.tabs.active_tab().map(|t| &t.live) {
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
        let heir = self.unpair_closing(id);
        if let Some(tab) = self.tabs.close(id) {
            if let Live::Ready(web) = tab.live {
                if tab.private { self.close_private_view(web) } else { web.close() }
            }
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
                to_discard.push(tab.id);
            } else if !web.suspended && idle >= suspend_after {
                web.suspend();
            }
        }
        if !to_discard.is_empty() {
            for id in to_discard {
                self.discard(id);
            }
            self.invalidate();
        }
    }

    // ---------- engine events ----------

    fn drain_events(&mut self) {
        while let Some(event) = web::pop() {
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
            }
            Event::EnvironmentReady(Err(err)) => {
                self.env_error = Some(format!("No se pudo iniciar WebView2 ({err}). Instalá el runtime de WebView2."));
                self.invalidate();
            }
            Event::ControllerReady(id, result) => self.controller_ready(id, result),
            Event::TitleChanged(id) => {
                let records = self.tabs.records(id);
                if let Some(tab) = self.tabs.get_mut(id) {
                    if let Live::Ready(web) = &tab.live {
                        tab.title = web.title();
                        if records {
                            self.history.set_title(&tab.url, &tab.title);
                        }
                    }
                }
                self.after_tab_change(id);
            }
            Event::SourceChanged(id) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    if let Live::Ready(web) = &tab.live {
                        let source = web.source();
                        if !source.is_empty() {
                            tab.url = source;
                        }
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
                self.invalidate();
            }
            Event::NewWindow(from, url) => {
                let at = self.tabs.index_of(from).map(|i| i + 1);
                let test = !self.tabs.records(from) && self.tabs.get(from).is_some_and(|t| !t.private);
                if self.tabs.get(from).is_some_and(|t| t.private) {
                    self.open_private_tab(&url, at);
                } else {
                    self.open_tab(&url, at);
                    // What a test tab opens is a test tab too.
                    if let (true, Some(id)) = (test, self.tabs.active) {
                        self.tabs.mark_test(id);
                    }
                }
            }
            Event::ProcessFailed(id) => {
                let visible = self.visible_tabs().contains(&id);
                self.discard(id);
                if visible {
                    self.show_active();
                }
                self.invalidate();
            }
            Event::PageClicked(id) => self.page_focused(id),
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
            width,
            height,
            self.vertical,
            self.collapsed,
            &metrics,
            self.extension_slots(),
        );
        self.invalidate();
    }

    fn controller_ready(&mut self, id: u64, result: windows::core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller>) {
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
                if web.passwords {
                    let _ = crate::vault_watch::watch(&web.webview, id);
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
            Command::ToggleTabLayout => {
                self.vertical = !self.vertical;
                self.collapsed = false;
                self.session_dirty = true;
                self.relayout();
            }
            Command::ToggleSidebar => {
                self.collapsed = !self.collapsed;
                self.session_dirty = true;
                self.relayout();
            }
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
                if let Some(text) = get_clipboard(self.hwnd) {
                    if let Some(url) = omnibox::resolve(&text, &self.settings.search_url) {
                        self.close_omnibar();
                        self.navigate_active(&url);
                    }
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
                if let Some(tab) = self.tabs.active_tab().filter(|t| !t.private) {
                    if let Live::Ready(web) = &tab.live {
                        self.curtain.toggle_picker(tab.id, web);
                    }
                }
            }
            Command::ShowHidden => {
                let (x, y, width) = self.omnibar_anchor();
                if let Some(tab) = self.tabs.active_tab() {
                    if let Live::Ready(web) = &tab.live {
                        self.curtain.show_hidden(self.hwnd, (x + width / 2, y), web, live_webs(&self.tabs));
                    }
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
                    // An open Ajustes window holds a copy: its next change would undo this.
                    crate::settings_window::sync(&self.settings);
                    // Bypassing the cache: what loaded while blocking was off
                    // sits there and is served without passing the filters.
                    self.with_active_web(|w| w.hard_reload());
                }
            }
            Command::SplitPage => self.split_command(),
            Command::Unsplit => {
                if let Some(id) = self.tabs.active {
                    self.unsplit(id);
                }
            }
            Command::FocusLeftPage => self.focus_side(Side::Left),
            Command::FocusRightPage => self.focus_side(Side::Right),
        }
    }

    fn with_active_web(&mut self, f: impl FnOnce(&mut WebTab)) {
        if let Some(id) = self.tabs.active {
            if let Some(tab) = self.tabs.get_mut(id) {
                if let Live::Ready(web) = &mut tab.live {
                    f(web);
                }
            }
        }
    }

    // ---------- mouse ----------

    fn set_hover(&mut self, hover: Option<Hit>) {
        if self.hover != hover {
            self.hover = hover;
            self.invalidate();
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
        if let Some(dragged) = self.drag {
            self.drag_tab(dragged, x, y);
        }
        self.set_hover(self.layout.hit(x, y));
    }

    fn mouse_down(&mut self, x: i32, y: i32) {
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
            Some(Hit::NewTab) => self.open_omnibar(Mode::NewTab),
            Some(Hit::Extensions) => {
                self.close_omnibar();
                self.extensions_menu();
            }
            Some(Hit::PinnedExtension(nth)) => {
                self.close_omnibar();
                self.open_pinned_extension(nth);
            }
            None => {
                if self.mode.is_some() {
                    self.close_omnibar();
                }
            }
        }
    }

    fn context_menu(&mut self, x: i32, y: i32) {
        let Some(Hit::Tab(id) | Hit::Close(id)) = self.layout.hit(x, y) else { return };
        let Some(tab) = self.tabs.get(id) else { return };
        let pinned = tab.pinned;
        let awake = self.keeps_awake(&tab.url);
        let site = if tab.private { "" } else { shield::site_key(&tab.url) };
        let shielded = (!site.is_empty()).then(|| !self.settings.shield_disabled.contains(site));
        let paired = self.split.splits.partner(id).is_some();
        let can_split = self.settings.split_view && !paired && self.tabs.active.is_some_and(|a| a != id);
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
            let _ = AppendMenuW(menu, MF_STRING, MENU_PIN, &wide(if pinned { "Desfijar" } else { "Fijar" }));
            let _ = AppendMenuW(menu, MF_STRING, MENU_DUPLICATE, w!("Duplicar"));
            let _ = AppendMenuW(menu, MF_STRING | check(awake), MENU_KEEP_AWAKE, w!("Mantener sitio despierto (usa más memoria)"));
            if let Some(on) = shielded {
                let _ = AppendMenuW(menu, MF_STRING | check(on), MENU_SHIELD, w!("Bloqueo de anuncios en este sitio"));
            }
            if paired {
                let _ = AppendMenuW(menu, MF_STRING, MENU_UNSPLIT, w!("Deshacer vista dividida"));
            } else if can_split {
                let _ = AppendMenuW(menu, MF_STRING, MENU_SPLIT, w!("Abrir al lado de la página actual"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE, w!("Cerrar"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_CLOSE_OTHERS, w!("Cerrar las demás"));
            let mut point = POINT { x, y };
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut point);
            let chosen =
                TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, self.hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
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

    fn omnibar_anchor(&self) -> (i32, i32, i32) {
        let content = self.focused_pane();
        let px = |v: f32| (v * self.scale).round() as i32;
        let width = px(640.0).min(content.width() - px(32.0)).max(px(240.0));
        let left = content.left + (content.width() - width) / 2;
        let mut point = POINT { x: left, y: content.top + px(10.0) };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut point);
        }
        (point.x, point.y, width)
    }

    fn place_omnibar(&self) {
        self.place_vault_bar();
        if self.mode.is_some() {
            self.omnibar.place(self.omnibar_anchor());
        }
    }

    fn open_omnibar(&mut self, mode: Mode) {
        if self.mode == Some(Mode::Find) {
            self.with_active_web(|w| w.find_stop());
        }
        let (cue, text, select_all) = match mode {
            Mode::Address => (
                "Escribí una dirección o buscá",
                self.tabs.active_tab().map(|t| t.url.clone()).unwrap_or_default(),
                true,
            ),
            Mode::NewTab => ("Nueva pestaña: dirección o búsqueda", String::new(), false),
            Mode::NewPrivateTab => ("Nueva pestaña privada: dirección o búsqueda", String::new(), false),
            Mode::Switch => ("Ir a una pestaña abierta", String::new(), false),
            Mode::Find => ("Buscar en la página", String::new(), false),
            Mode::Panel(kind) => (kind.cue(), String::new(), false),
            Mode::Split => ("Al lado: dirección, búsqueda o pestaña abierta (Enter vacío: la misma página)", String::new(), false),
        };
        self.mode = Some(mode);
        self.load_panel_data(mode);
        self.rows.clear();
        self.typed_len = text.encode_utf16().count();
        let anchor = self.omnibar_anchor();
        match mode {
            Mode::Panel(_) => self.omnibar.set_list_mode(panel_ui::PANEL_VISIBLE_ROWS, true),
            _ => self.omnibar.set_list_mode(usize::MAX, false),
        }
        self.omnibar.open(anchor, cue, &text, select_all);
        if matches!(mode, Mode::Switch | Mode::Panel(_) | Mode::Split) {
            self.refresh_rows(false);
        }
        self.invalidate();
    }

    fn close_omnibar(&mut self) {
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
                    _ => self.refresh_rows(typing_forward),
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
        let labels: Vec<String> = rows.iter().map(|(label, _)| label.clone()).collect();
        self.rows = rows.into_iter().map(|(_, row)| row).collect();
        self.omnibar.set_rows(&labels);
        self.omnibar.place(self.omnibar_anchor());
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
                if typing_forward && caret_at_end {
                    if let Some(completion) = self.history.inline_completion(&text) {
                        self.omnibar.complete_inline(&text, &completion);
                    }
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
            Mode::Panel(kind) => self.panel_rows(kind, &text),
            Mode::Split => self.split_rows(&text),
            Mode::Find => return,
        };
        self.set_rows(rows);
    }

    fn show_find_status(&mut self) {
        let status = self.tabs.active_tab().and_then(|t| match &t.live {
            Live::Ready(web) => web.find_status(),
            _ => None,
        });
        let label = match status {
            Some((_, 0)) | None => "Sin coincidencias".to_string(),
            Some((index, count)) => format!("{} de {count}    Enter: siguiente · Shift+Enter: anterior", index.max(1)),
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
        self.close_omnibar();
        if mode == Mode::NewTab {
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
        let _ = storage::write_json(&storage::data_dir().join("settings.json"), &self.settings);
    }

    fn save(&mut self, force: bool) {
        let dir = storage::data_dir();
        if self.session_dirty || force {
            let _ = storage::write_json(&dir.join("session.json"), &self.session());
            self.session_dirty = false;
        }
        if self.history.dirty || force {
            let _ = storage::write_json(&dir.join("history.json"), &self.history.entries());
            self.history.dirty = false;
        }
    }

    fn session(&self) -> Session {
        let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
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
        }
    }

    fn shutdown(&mut self) {
        self.save(true);
        for tab in &mut self.tabs.tabs {
            if let Live::Ready(web) = std::mem::replace(&mut tab.live, Live::Asleep) {
                web.close();
            }
        }
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
    unsafe {
        if GlobalMemoryStatusEx(&mut status).is_ok() { status.dwMemoryLoad } else { 0 }
    }
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

