//! Everything that talks to WebView2. Engine callbacks never touch the app
//! state directly: they queue an `Event` and post one message to the window,
//! so no callback runs while the app is in the middle of something else.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, LPARAM, RECT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT},
            WindowsAndMessaging::{PostMessageW, WM_APP},
        },
    },
    core::{BOOL, HSTRING, Interface, PWSTR, Result},
};

use crate::{
    settings::Theme,
    shortcuts::{Chord, Command, Shortcuts},
};

pub const WM_APP_EVENTS: u32 = WM_APP + 1;

pub enum Event {
    EnvironmentReady(Result<ICoreWebView2Environment>),
    ControllerReady(u64, Result<ICoreWebView2Controller>),
    TitleChanged(u64),
    SourceChanged(u64),
    NavigationStarting(u64),
    NavigationCompleted(u64, bool),
    /// A page asked for a new window: tab, address, whether a click or a
    /// key was behind it. The engine's window is never opened: the app
    /// opens a tab or blocks it.
    NewWindow(u64, String, bool),
    ProcessFailed(u64),
    /// The page of this tab took focus (a click on it).
    PageClicked(u64),
    FindChanged(u64),
    /// A page posted a message: tab, source URL, message as JSON.
    WebMessage(u64, String, String),
    Command(Command),
    /// A download started, progressed or ended (see `download_watch`).
    Download(crate::downloads::Download),
    /// The saved-passwords script of a tab said something (see `vault_watch`).
    Vault(u64, crate::vault_watch::Message),
    /// The profile answered an extensions request (see `extension_host`).
    Extension(crate::extension_host::HostEvent),
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
    static WINDOW: Cell<isize> = const { Cell::new(0) };
}

pub fn set_window(hwnd: HWND) {
    WINDOW.with(|w| w.set(hwnd.0 as isize));
}

pub fn push(event: Event) {
    let was_empty = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let was_empty = q.is_empty();
        q.push_back(event);
        was_empty
    });
    if was_empty {
        let hwnd = HWND(WINDOW.with(|w| w.get()) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_EVENTS, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn pop() -> Option<Event> {
    QUEUE.with(|q| q.borrow_mut().pop_front())
}

/// A tab with a web view behind it.
pub struct WebTab {
    pub controller: ICoreWebView2Controller,
    pub webview: ICoreWebView2,
    pub suspended: bool,
    /// Saved passwords are offered and filled here. A private tab sets it to
    /// false before the view is wired: no capture, no account menu.
    pub passwords: bool,
    find: Option<ICoreWebView2Find>,
}

/// `extensions`: whether pages may run browser extensions. Fixed for the
/// environment's life, and each extension adds processes, so it is on only
/// when one is enabled.
pub fn create_environment(data_dir: &std::path::Path, extensions: bool) -> Result<()> {
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        options.set_language("es".to_string());
        options.set_are_browser_extensions_enabled(extensions);
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|result, env| {
        push(Event::EnvironmentReady(result.and_then(|_| env.ok_or_else(|| windows::core::Error::from(E_POINTER)))));
        Ok(())
    }));
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            None,
            &HSTRING::from(data_dir.as_os_str()),
            &options,
            &handler,
        )
    }
}

pub fn create_controller(env: &ICoreWebView2Environment, parent: HWND, id: u64) -> Result<()> {
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
        push(Event::ControllerReady(
            id,
            result.and_then(|_| controller.ok_or_else(|| windows::core::Error::from(E_POINTER))),
        ));
        Ok(())
    }));
    unsafe { env.CreateCoreWebView2Controller(parent, &handler) }
}

fn modifiers_down() -> (bool, bool, bool) {
    unsafe {
        (
            GetKeyState(VK_CONTROL.0 as i32) < 0,
            GetKeyState(VK_SHIFT.0 as i32) < 0,
            GetKeyState(VK_MENU.0 as i32) < 0,
        )
    }
}

/// The chord being pressed right now with this virtual key.
pub fn current_chord(vk: u32) -> Chord {
    let (ctrl, shift, alt) = modifiers_down();
    Chord { ctrl, shift, alt, vk }
}

impl WebTab {
    /// Wires the engine events of a new view to the queue and applies the
    /// look. The view stays hidden until the app shows it.
    pub fn new(id: u64, controller: ICoreWebView2Controller, shortcuts: Rc<Shortcuts>, theme: Theme) -> Result<Self> {
        unsafe {
            let webview = controller.CoreWebView2()?;
            controller.SetIsVisible(false)?;
            let tab = WebTab { controller, webview, suspended: false, passwords: true, find: None };
            tab.apply_theme(theme);

            let settings = tab.webview.Settings()?;
            settings.SetIsStatusBarEnabled(true)?;
            settings.SetAreDevToolsEnabled(true)?;
            // A view that can't be filtered still works, just unfiltered.
            let _ = crate::shield::attach(&tab.webview);

            let mut token = 0i64;
            tab.webview.add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::TitleChanged(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::SourceChanged(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                    push(Event::NavigationStarting(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let mut ok = BOOL(0);
                    if let Some(args) = args {
                        let _ = args.IsSuccess(&mut ok);
                    }
                    push(Event::NavigationCompleted(id, ok.as_bool()));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut uri = PWSTR::null();
                        args.Uri(&mut uri)?;
                        let mut user = BOOL(0);
                        let _ = args.IsUserInitiated(&mut user);
                        args.SetHandled(true)?;
                        push(Event::NewWindow(id, take_pwstr(uri), user.as_bool()));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, _| {
                    push(Event::ProcessFailed(id));
                    Ok(())
                })),
                &mut token,
            )?;
            // The app never moves focus into the page while the field is
            // open, so the page gaining focus then means the person clicked it.
            tab.controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::PageClicked(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                        return Ok(());
                    }
                    let mut vk = 0u32;
                    args.VirtualKey(&mut vk)?;
                    if let Some(command) = shortcuts.lookup(current_chord(vk)) {
                        args.SetHandled(true)?;
                        push(Event::Command(command));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            Ok(tab)
        }
    }

    pub fn apply_theme(&self, theme: Theme) {
        unsafe {
            if let Ok(c2) = self.controller.cast::<ICoreWebView2Controller2>() {
                // White, as in every browser: a page that sets no background
                // is written for one, and over the dark chrome its black text
                // was unreadable. A page that supports dark mode paints its own.
                let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 255, G: 255, B: 255 });
            }
            if let Ok(profile) = self.webview.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()) {
                let scheme = match theme {
                    Theme::System => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_AUTO,
                    Theme::Light => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT,
                    Theme::Dark => COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK,
                };
                let _ = profile.SetPreferredColorScheme(scheme);
            }
        }
    }

    /// The engine's tracking prevention on this view's profile: strict
    /// with `block_trackers`, the engine's default (balanced) without.
    /// Strict, measured: the scripts of Facebook, Google Analytics and
    /// Hotjar fail to load from a third-party page (balanced let all three
    /// through), and the sign-in pages of Google, Microsoft, GitHub and
    /// Amazon still load. It has no per-site exception in the engine.
    pub fn apply_privacy(&self, block_trackers: bool) {
        let level = tracking_level(block_trackers);
        unsafe {
            if let Ok(profile) = self.webview.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()).and_then(|p| p.cast::<ICoreWebView2Profile3>()) {
                let _ = profile.SetPreferredTrackingPreventionLevel(level);
            }
        }
    }

    pub fn navigate(&self, url: &str) {
        unsafe {
            let _ = self.webview.Navigate(&HSTRING::from(url));
        }
    }

    pub fn title(&self) -> String {
        let mut value = PWSTR::null();
        unsafe {
            if self.webview.DocumentTitle(&mut value).is_ok() { take_pwstr(value) } else { String::new() }
        }
    }

    pub fn source(&self) -> String {
        let mut value = PWSTR::null();
        unsafe { if self.webview.Source(&mut value).is_ok() { take_pwstr(value) } else { String::new() } }
    }

    pub fn show(&mut self, bounds: RECT) {
        unsafe {
            if self.suspended {
                if let Ok(w3) = self.webview.cast::<ICoreWebView2_3>() {
                    let _ = w3.Resume();
                }
                self.suspended = false;
            }
            if let Ok(w19) = self.webview.cast::<ICoreWebView2_19>() {
                let _ = w19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL);
            }
            let _ = self.controller.SetBounds(bounds);
            let _ = self.controller.SetIsVisible(true);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = self.controller.SetIsVisible(false);
        }
    }

    pub fn focus(&self) {
        unsafe {
            let _ = self.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        }
    }

    /// Second stage of sleeping: the engine stops timers and scripts and
    /// trims memory. Fails on its own if the page plays audio.
    pub fn suspend(&mut self) {
        unsafe {
            let _ = self.controller.SetIsVisible(false);
            if let Ok(w19) = self.webview.cast::<ICoreWebView2_19>() {
                let _ = w19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW);
            }
            if let Ok(w3) = self.webview.cast::<ICoreWebView2_3>() {
                let _ = w3.TrySuspend(&TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(()))));
            }
        }
        self.suspended = true;
    }

    pub fn is_playing_audio(&self) -> bool {
        let mut playing = BOOL(0);
        unsafe {
            if let Ok(w8) = self.webview.cast::<ICoreWebView2_8>() {
                let _ = w8.IsDocumentPlayingAudio(&mut playing);
            }
        }
        playing.as_bool()
    }

    pub fn back(&self) {
        unsafe {
            let _ = self.webview.GoBack();
        }
    }

    pub fn forward(&self) {
        unsafe {
            let _ = self.webview.GoForward();
        }
    }

    pub fn reload(&self) {
        unsafe {
            let _ = self.webview.Reload();
        }
    }

    pub fn hard_reload(&self) {
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
        unsafe {
            let _ = self.webview.CallDevToolsProtocolMethod(
                &HSTRING::from("Page.reload"),
                &HSTRING::from(r#"{"ignoreCache":true}"#),
                &handler,
            );
        }
    }

    pub fn open_downloads(&self) {
        unsafe {
            if let Ok(w9) = self.webview.cast::<ICoreWebView2_9>() {
                let _ = w9.OpenDefaultDownloadDialog();
            }
        }
    }

    fn find_object(&mut self, id: u64) -> Option<ICoreWebView2Find> {
        if self.find.is_none() {
            let find = unsafe { self.webview.cast::<ICoreWebView2_28>().and_then(|w| w.Find()).ok()? };
            let mut token = 0i64;
            unsafe {
                let _ = find.add_MatchCountChanged(
                    &FindMatchCountChangedEventHandler::create(Box::new(move |_, _| {
                        push(Event::FindChanged(id));
                        Ok(())
                    })),
                    &mut token,
                );
                let _ = find.add_ActiveMatchIndexChanged(
                    &FindActiveMatchIndexChangedEventHandler::create(Box::new(move |_, _| {
                        push(Event::FindChanged(id));
                        Ok(())
                    })),
                    &mut token,
                );
            }
            self.find = Some(find);
        }
        self.find.clone()
    }

    /// Starts (or restarts) a search for `term`, with our own field instead
    /// of the engine's find bar. Returns false when the engine lacks the API.
    pub fn find_start(&mut self, id: u64, env: &ICoreWebView2Environment, term: &str) -> bool {
        let Some(find) = self.find_object(id) else { return false };
        unsafe {
            if term.is_empty() {
                let _ = find.Stop();
                return true;
            }
            let Ok(options) = env.cast::<ICoreWebView2Environment15>().and_then(|e| e.CreateFindOptions()) else {
                return false;
            };
            let _ = options.SetFindTerm(&HSTRING::from(term));
            let _ = options.SetSuppressDefaultFindDialog(true);
            let _ = options.SetShouldHighlightAllMatches(true);
            find.Start(&options, &FindStartCompletedHandler::create(Box::new(|_| Ok(())))).is_ok()
        }
    }

    pub fn find_step(&mut self, id: u64, forward: bool) {
        if let Some(find) = self.find_object(id) {
            unsafe {
                let _ = if forward { find.FindNext() } else { find.FindPrevious() };
            }
        }
    }

    pub fn find_stop(&mut self) {
        if let Some(find) = &self.find {
            unsafe {
                let _ = find.Stop();
            }
        }
    }

    /// (active match, 1-based; total matches)
    pub fn find_status(&self) -> Option<(i32, i32)> {
        let find = self.find.as_ref()?;
        let (mut index, mut count) = (0i32, 0i32);
        unsafe {
            find.ActiveMatchIndex(&mut index).ok()?;
            find.MatchCount(&mut count).ok()?;
        }
        Some(match_position(index, count))
    }

    /// Destroys the view and its renderer: only the tab's metadata remains.
    pub fn close(self) {
        unsafe {
            let _ = self.controller.Close();
        }
    }
}

fn tracking_level(block_trackers: bool) -> COREWEBVIEW2_TRACKING_PREVENTION_LEVEL {
    if block_trackers { COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT } else { COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED }
}

/// The engine's active match index is already 1-based (measured: the first
/// match of two reads 1, and the last one 2); -1 means none is active yet.
fn match_position(index: i32, count: i32) -> (i32, i32) {
    (index.clamp(1, count.max(1)), count)
}

