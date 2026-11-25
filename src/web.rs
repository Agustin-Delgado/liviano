//! Everything that talks to WebView2. Engine callbacks never touch the app
//! state directly: they queue an `Event` and post one message to the window,
//! so no callback runs while the app is in the middle of something else.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
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
    /// The environment asked for under this engine number (`generation`).
    EnvironmentReady(u64, Result<ICoreWebView2Environment>),
    /// A view for this tab, asked for under this engine number.
    ControllerReady(u64, u64, Result<ICoreWebView2Controller>),
    /// The browser process of this engine number ended; true when it failed
    /// (a crash, a kill), false when it ended because nothing used it.
    BrowserExited(u64, bool),
    TitleChanged(u64),
    SourceChanged(u64),
    NavigationStarting(u64),
    NavigationCompleted(u64, bool),
    /// A page asked for a new window: tab, address, whether a click or a
    /// key was behind it, and the request that the engine holds until the
    /// app answers it (`answer_new_window`). The engine's window is never
    /// opened: the app opens a tab or blocks it.
    NewWindow(u64, String, bool, NewWindowRequest),
    /// The page of this tab called `window.close()`.
    CloseRequested(u64),
    /// A process of the engine died or hung: tab, engine number, and the
    /// kind, which says which process.
    ProcessFailed(u64, u64, COREWEBVIEW2_PROCESS_FAILED_KIND),
    /// The page of this tab took focus (a click on it).
    PageClicked(u64),
    FindChanged(u64),
    /// The tab's back/forward history changed (`HistoryChanged`).
    HistoryChanged(u64),
    /// The tab's history, asked for its back (true) or forward menu: the
    /// JSON of DevTools' `Page.getNavigationHistory`.
    NavHistory(u64, bool, String),
    /// A page posted a message: tab, source URL, message as JSON.
    WebMessage(u64, String, String),
    Command(Command),
    /// The page of this tab entered or left a full screen element.
    FullScreen(u64),
    /// The engine changed the zoom factor of this tab's view: the person
    /// zoomed (Ctrl+wheel, Ctrl+= and Ctrl+-, which the engine handles), or
    /// a navigation went to a site with a different level.
    ZoomChanged(u64, f64),
    /// A download started, progressed or ended (see `download_watch`).
    Download(crate::downloads::Download),
    /// The saved-passwords script of a tab said something (see `vault_watch`).
    Vault(u64, crate::vault_watch::Message),
    /// The profile answered an extensions request (see `extension_host`).
    Extension(crate::extension_host::HostEvent),
    /// Where the page of a tab is scrolled: tab, address, x, y (`read_scroll`).
    Scrolled(u64, String, i64, i64),
    /// A page asked to notify, sent a notification or closed one (see `notify_watch`).
    Notify(u64, crate::notify_watch::Message),
}

impl Event {
    /// What the diagnostics call it (see `diag::doing`).
    pub fn name(&self) -> &'static &'static str {
        match self {
            Event::EnvironmentReady(..) => &"event:EnvironmentReady",
            Event::BrowserExited(..) => &"event:BrowserExited",
            Event::ControllerReady(..) => &"event:ControllerReady",
            Event::TitleChanged(_) => &"event:TitleChanged",
            Event::SourceChanged(_) => &"event:SourceChanged",
            Event::NavigationStarting(_) => &"event:NavigationStarting",
            Event::NavigationCompleted(..) => &"event:NavigationCompleted",
            Event::NewWindow(..) => &"event:NewWindow",
            Event::CloseRequested(_) => &"event:CloseRequested",
            Event::ProcessFailed(..) => &"event:ProcessFailed",
            Event::PageClicked(_) => &"event:PageClicked",
            Event::FindChanged(_) => &"event:FindChanged",
            Event::HistoryChanged(_) => &"event:HistoryChanged",
            Event::NavHistory(..) => &"event:NavHistory",
            Event::WebMessage(..) => &"event:WebMessage",
            Event::Command(_) => &"event:Command",
            Event::FullScreen(_) => &"event:FullScreen",
            Event::ZoomChanged(..) => &"event:ZoomChanged",
            Event::Download(_) => &"event:Download",
            Event::Vault(..) => &"event:Vault",
            Event::Extension(_) => &"event:Extension",
            Event::Scrolled(..) => &"event:Scrolled",
            Event::Notify(..) => &"event:Notify",
        }
    }

    /// The engine number it belongs to, for the events of one engine.
    pub fn generation(&self) -> Option<u64> {
        match self {
            Event::EnvironmentReady(generation, _)
            | Event::ControllerReady(_, generation, _)
            | Event::BrowserExited(generation, _)
            | Event::ProcessFailed(_, generation, _) => Some(*generation),
            _ => None,
        }
    }

    /// The tab it is about, or 0.
    pub fn tab(&self) -> u64 {
        match self {
            Event::ControllerReady(id, ..)
            | Event::TitleChanged(id)
            | Event::SourceChanged(id)
            | Event::NavigationStarting(id)
            | Event::NavigationCompleted(id, _)
            | Event::NewWindow(id, ..)
            | Event::CloseRequested(id)
            | Event::ProcessFailed(id, ..)
            | Event::PageClicked(id)
            | Event::FindChanged(id)
            | Event::HistoryChanged(id)
            | Event::NavHistory(id, ..)
            | Event::WebMessage(id, ..)
            | Event::FullScreen(id)
            | Event::ZoomChanged(id, _)
            | Event::Vault(id, _)
            | Event::Scrolled(id, ..)
            | Event::Notify(id, _) => *id,
            _ => 0,
        }
    }
}

/// A `window.open` (or a link to a new window) that the engine holds open
/// with a deferral until the app gives it the view of the new tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewWindowRequest {
    pub id: u64,
    /// The page asked for a window of a given size: a sign-in pop-up, as
    /// "Sign in with Google" and the other identity providers open it.
    pub popup: bool,
}

type HeldNewWindow = (ICoreWebView2NewWindowRequestedEventArgs, ICoreWebView2Deferral);

thread_local! {
    static QUEUE: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
    /// The new-window requests that wait for an answer, by `NewWindowRequest::id`.
    static NEW_WINDOWS: RefCell<HashMap<u64, HeldNewWindow>> = RefCell::new(HashMap::new());
    static NEXT_NEW_WINDOW: Cell<u64> = const { Cell::new(1) };
    static WINDOW: Cell<isize> = const { Cell::new(0) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The number of the engine now in use. Each environment asked for, and
/// each restart after a crash, gets a new one: an answer or an event of an
/// earlier engine (a view made for a browser process that died since) is
/// then known as such and dropped.
pub fn generation() -> u64 {
    GENERATION.with(|g| g.get())
}

/// Starts a new engine number: everything of the previous one is old now.
pub fn next_generation() -> u64 {
    GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
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

/// Whether events wait in the queue.
pub fn queued() -> bool {
    QUEUE.with(|q| !q.borrow().is_empty())
}

/// A tab with a web view behind it.
pub struct WebTab {
    pub controller: ICoreWebView2Controller,
    pub webview: ICoreWebView2,
    pub suspended: bool,
    /// The engine was told that the page may use less memory: the first
    /// stage of a tab out of sight (refer to `memory`).
    pub lowered: bool,
    /// Where the view is shown, or `None` while hidden. Every tab switch
    /// walks all the tabs: telling the engine what it already knows is a
    /// round trip to its process per tab.
    shown: Option<RECT>,
    /// Saved passwords are offered and filled here. A private tab sets it to
    /// false before the view is wired: no capture, no account menu.
    pub passwords: bool,
    find: Option<ICoreWebView2Find>,
    /// Shown out of the window, 1 pixel high, since then, so that the
    /// engine drops what draws it at full size (refer to `park`).
    parked: Option<std::time::Instant>,
    /// The page's keyboard is in a text field (`text_focus`).
    pub typing: Rc<Cell<bool>>,
}

/// The engine switches of Nea. FedCM (`navigator.credentials.get` with
/// `identity`) is off: WebView2 shows no account chooser for it, so a
/// request stayed open with no end (measured with a local identity
/// provider). Without it, a site gets `NotSupportedError` at once and uses
/// its other sign-in path (the pop-up or the iframes of the provider).
pub const ENGINE_ARGUMENTS: &str = "--disable-features=FedCm";

/// `extensions`: whether pages may run browser extensions. Fixed for the
/// environment's life, and each extension adds processes, so it is on only
/// when one is enabled.
pub fn create_environment(data_dir: &std::path::Path, extensions: bool) -> Result<()> {
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        options.set_are_browser_extensions_enabled(extensions);
        options.set_additional_browser_arguments(ENGINE_ARGUMENTS.to_string());
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let generation = next_generation();
    crate::diag::trace(|| format!("environment requested engine={generation}"));
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, env| {
        crate::diag::trace(|| format!("environment ready ok={}", result.is_ok()));
        let env = result.and_then(|_| env.ok_or_else(|| windows::core::Error::from(E_POINTER)));
        push(Event::EnvironmentReady(generation, env));
        Ok(())
    }));
    unsafe { CreateCoreWebView2EnvironmentWithOptions(None, &HSTRING::from(data_dir.as_os_str()), &options, &handler) }
}

pub fn create_controller(env: &ICoreWebView2Environment, parent: HWND, id: u64) -> Result<()> {
    crate::diag::trace(|| format!("controller requested tab={id}"));
    let handler = controller_handler(id);
    unsafe { env.CreateCoreWebView2Controller(parent, &handler) }
}

/// The answer to a view request for tab `id`, queued with the engine number
/// of now. It can be an error: E_ABORT when the request ended before the
/// view did (the engine or the window went first), or the engine's own.
pub fn controller_handler(id: u64) -> ICoreWebView2CreateCoreWebView2ControllerCompletedHandler {
    let generation = generation();
    CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
        crate::diag::trace(|| format!("controller ready tab={id} ok={}", result.is_ok()));
        push(Event::ControllerReady(
            id,
            generation,
            result.and_then(|_| controller.ok_or_else(|| windows::core::Error::from(E_POINTER))),
        ));
        Ok(())
    }))
}

/// Reports the end of the engine's browser process. The views each report
/// it too, but only the views: with every tab asleep, or only a spare or a
/// view being made, nothing else would tell that the engine is gone.
pub fn watch_browser(env: &ICoreWebView2Environment, generation: u64) {
    let Ok(env5) = env.cast::<ICoreWebView2Environment5>() else { return };
    let handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, args| {
        let (mut kind, mut pid) = (COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL, 0u32);
        if let Some(args) = args {
            unsafe {
                let _ = args.BrowserProcessExitKind(&mut kind);
                let _ = args.BrowserProcessId(&mut pid);
            }
        }
        let failed = kind == COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_FAILED;
        if failed {
            crate::diag::log(
                crate::diag::Log::Crash,
                format!("{} engine: browser process {pid} failed (engine {generation})", crate::diag::stamp()),
            );
        }
        push(Event::BrowserExited(generation, failed));
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        let _ = env5.add_BrowserProcessExited(&handler, &mut token);
    }
}

/// Keeps the engine waiting on a new-window request until the app answers
/// it. Without the deferral, the page got no window back: `window.open`
/// returned `null`, the new page had no `window.opener`, and a sign-in
/// pop-up could not give its result to the page that opened it.
fn hold_new_window(args: &ICoreWebView2NewWindowRequestedEventArgs) -> NewWindowRequest {
    let id = NEXT_NEW_WINDOW.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    let mut popup = BOOL(0);
    unsafe {
        if let Ok(features) = args.WindowFeatures() {
            let _ = features.HasSize(&mut popup);
        }
        if let Ok(deferral) = args.GetDeferral() {
            NEW_WINDOWS.with(|w| w.borrow_mut().insert(id, (args.clone(), deferral)));
        }
    }
    NewWindowRequest { id, popup: popup.as_bool() }
}

/// Answers a held new-window request. With `view`, the page that asked
/// gets that view as its new window (with `window.opener`), and the engine
/// loads the address in it. Without, the request ends with no window (a
/// blocked pop-up). True when the view became the new window. False also
/// when the request is unknown or belongs to an engine that ended: the
/// caller then loads the address itself.
pub fn answer_new_window(request: u64, view: Option<&ICoreWebView2>) -> bool {
    let Some((args, deferral)) = NEW_WINDOWS.with(|w| w.borrow_mut().remove(&request)) else { return false };
    let adopted = view.is_some_and(|view| unsafe { args.SetNewWindow(view).is_ok() });
    unsafe {
        let _ = deferral.Complete();
    }
    adopted
}

fn modifiers_down() -> (bool, bool, bool) {
    unsafe {
        (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0)
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
    /// On an error the view is closed: a view half wired and dropped
    /// without `Close` stayed in the window, a page nobody could reach.
    pub fn new(id: u64, controller: ICoreWebView2Controller, shortcuts: Rc<Shortcuts>, theme: Theme) -> Result<Self> {
        let generation = generation();
        Self::wire(id, generation, controller.clone(), shortcuts, theme).inspect_err(|_| unsafe {
            let _ = controller.Close();
        })
    }

    fn wire(
        id: u64,
        generation: u64,
        controller: ICoreWebView2Controller,
        shortcuts: Rc<Shortcuts>,
        theme: Theme,
    ) -> Result<Self> {
        unsafe {
            let webview = controller.CoreWebView2()?;
            // A view parked visible at zero size (the spare, or one just
            // made) shows nothing: left so. Hidden for seconds, its first
            // showing rebuilds what draws it, ~80 ms waiting on the engine.
            let mut visible = windows::core::BOOL::default();
            let mut bounds = RECT::default();
            let parked = controller.IsVisible(&mut visible).is_ok()
                && controller.Bounds(&mut bounds).is_ok()
                && visible.as_bool()
                && bounds == RECT::default();
            if !parked {
                controller.SetIsVisible(false)?;
            }
            let shown = parked.then_some(bounds);
            let tab = WebTab {
                controller,
                webview,
                suspended: false,
                lowered: false,
                shown,
                passwords: true,
                find: None,
                parked: None,
                typing: Rc::new(Cell::new(false)),
            };
            tab.apply_theme(theme);
            // Without it the word moves stay with the page and Ctrl+Left
            // does not switch spaces from it: nothing breaks.
            let _ = crate::text_focus::watch(&tab.webview, tab.typing.clone());
            let typing = tab.typing.clone();

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
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if crate::diag::tracing() {
                        let mut uri = PWSTR::null();
                        if let Some(args) = args {
                            let _ = args.Uri(&mut uri);
                        }
                        let uri = take_pwstr(uri);
                        crate::diag::trace(|| format!("navigation starting tab={id} {uri}"));
                    }
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
                    crate::diag::trace(|| format!("navigation completed tab={id} ok={}", ok.as_bool()));
                    push(Event::NavigationCompleted(id, ok.as_bool()));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::HistoryChanged(id));
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
                        let request = hold_new_window(&args);
                        push(Event::NewWindow(id, take_pwstr(uri), user.as_bool(), request));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_WindowCloseRequested(
                &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                    push(Event::CloseRequested(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_ContainsFullScreenElementChanged(
                &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::FullScreen(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.webview.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let failure = args.map(|a| ProcessFailure::read(&a)).unwrap_or_default();
                    crate::diag::log(
                        crate::diag::Log::Crash,
                        format!("{} engine: {} (tab {id})", crate::diag::stamp(), failure.describe()),
                    );
                    push(Event::ProcessFailed(id, generation, failure.kind));
                    Ok(())
                })),
                &mut token,
            )?;
            if crate::diag::tracing() {
                tab.webview.add_ContentLoading(
                    &ContentLoadingEventHandler::create(Box::new(move |_, _| {
                        crate::diag::trace(|| format!("content loading tab={id}"));
                        Ok(())
                    })),
                    &mut token,
                )?;
                if let Ok(w2) = tab.webview.cast::<ICoreWebView2_2>() {
                    w2.add_DOMContentLoaded(
                        &DOMContentLoadedEventHandler::create(Box::new(move |_, _| {
                            crate::diag::trace(|| format!("dom content loaded tab={id}"));
                            Ok(())
                        })),
                        &mut token,
                    )?;
                }
            }
            // The app never moves focus into the page while the field is
            // open, so the page gaining focus then means the person clicked it.
            tab.controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    push(Event::PageClicked(id));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.controller.add_ZoomFactorChanged(
                &ZoomFactorChangedEventHandler::create(Box::new(move |sender, _| {
                    let mut factor = 1.0;
                    if let Some(controller) = sender {
                        controller.ZoomFactor(&mut factor)?;
                    }
                    push(Event::ZoomChanged(id, factor));
                    Ok(())
                })),
                &mut token,
            )?;
            tab.controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    {
                        return Ok(());
                    }
                    let mut vk = 0u32;
                    args.VirtualKey(&mut vk)?;
                    let chord = current_chord(vk);
                    // In a text field a word move is the page's.
                    if typing.get() && chord.moves_the_caret() {
                        return Ok(());
                    }
                    if let Some(command) = shortcuts.lookup(chord) {
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
            if let Ok(profile) = self
                .webview
                .cast::<ICoreWebView2_13>()
                .and_then(|w| w.Profile())
                .and_then(|p| p.cast::<ICoreWebView2Profile3>())
            {
                let _ = profile.SetPreferredTrackingPreventionLevel(level);
            }
        }
    }

    pub fn navigate(&self, url: &str) {
        crate::diag::trace(|| format!("navigate {url}"));
        unsafe {
            let _ = self.webview.Navigate(&HSTRING::from(url));
        }
    }

    pub fn title(&self) -> String {
        let mut value = PWSTR::null();
        unsafe { if self.webview.DocumentTitle(&mut value).is_ok() { take_pwstr(value) } else { String::new() } }
    }

    pub fn source(&self) -> String {
        let mut value = PWSTR::null();
        unsafe { if self.webview.Source(&mut value).is_ok() { take_pwstr(value) } else { String::new() } }
    }

    pub fn show(&mut self, bounds: RECT) {
        unsafe {
            if self.suspended {
                let _doing = crate::diag::doing(&"view:resume", 0);
                if let Ok(w3) = self.webview.cast::<ICoreWebView2_3>() {
                    let _ = w3.Resume();
                }
                self.suspended = false;
            }
            if self.lowered {
                if let Ok(w19) = self.webview.cast::<ICoreWebView2_19>() {
                    let _ = w19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL);
                }
                self.lowered = false;
            }
            self.parked = None;
            if self.shown != Some(bounds) {
                let _doing = crate::diag::doing(&"view:bounds", 0);
                let _ = self.controller.SetBounds(bounds);
            }
            if self.shown.is_none() {
                let _doing = crate::diag::doing(&"view:visible", 0);
                let _ = self.controller.SetIsVisible(true);
            }
            self.shown = Some(bounds);
        }
    }

    /// Whether the view is on screen (shown, and not hidden since). A
    /// parked view is not: nobody sees it.
    pub fn is_shown(&self) -> bool {
        self.shown.is_some() && self.parked.is_none()
    }

    pub fn hide(&mut self) {
        self.parked = None;
        if self.shown.take().is_some() {
            unsafe {
                let _ = self.controller.SetIsVisible(false);
            }
        }
    }

    /// The engine keeps the zoom in the view. A navigation to a different
    /// site can change it, and then the engine sends `ZoomChanged`. It sends
    /// none for this call.
    pub fn set_zoom(&self, factor: f64) {
        unsafe {
            let _ = self.controller.SetZoomFactor(factor);
        }
    }

    pub fn focus(&self) {
        unsafe {
            let _ = self.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        }
    }

    /// First stage of sleeping: the page goes on, and the engine drops what
    /// it can make again. Only `show` sets the level back.
    pub fn lower(&mut self) {
        if self.lowered {
            return;
        }
        unsafe {
            if let Ok(w19) = self.webview.cast::<ICoreWebView2_19>() {
                let _ = w19.SetMemoryUsageTargetLevel(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW);
            }
        }
        self.lowered = true;
    }

    /// Before a suspend: the hidden view is shown again out of the window
    /// (nobody sees it), as high as one pixel and as wide as before. The
    /// engine keeps the surfaces of a hidden view at the size it was last
    /// drawn at, approximately 11 MB in the GPU process at 1400 x 900; it
    /// draws one frame at the new size and lets them go. The width stays,
    /// so the text of the page keeps its lines and its scroll position.
    pub fn park(&mut self, now: std::time::Instant) {
        if self.shown.is_some() || self.suspended {
            return;
        }
        let mut bounds = RECT::default();
        unsafe {
            let _ = self.controller.Bounds(&mut bounds);
        }
        let parked = crate::memory::parked_bounds(bounds);
        unsafe {
            let _ = self.controller.SetBounds(parked);
            let _ = self.controller.SetIsVisible(true);
        }
        self.shown = Some(parked);
        self.parked = Some(now);
    }

    /// How long the view is parked, or `None` when it is not.
    pub fn parked_for(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.parked.map(|at| now.saturating_duration_since(at))
    }

    /// Second stage of sleeping: the engine stops timers and scripts and
    /// trims memory. Fails on its own if the page plays audio.
    pub fn suspend(&mut self) {
        // A background tab is hidden already.
        self.hide();
        self.lower();
        unsafe {
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

    /// Asks where the page is scrolled; the answer comes as `Event::Scrolled`.
    pub fn read_scroll(&self, id: u64) {
        let url = self.source();
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            if result.is_ok()
                && let Some((x, y)) = parse_scroll(&json)
            {
                push(Event::Scrolled(id, url, x, y));
            }
            Ok(())
        }));
        unsafe {
            let _ = self.webview.ExecuteScript(&HSTRING::from(SCROLL_SCRIPT), &handler);
        }
    }

    /// Scrolls a page that just loaded to (`x`, `y`), unless it scrolled
    /// already on its own (or the person did).
    pub fn restore_scroll(&self, x: i64, y: i64) {
        let handler = ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(())));
        unsafe {
            let _ = self.webview.ExecuteScript(&HSTRING::from(restore_scroll_script(x, y)), &handler);
        }
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

    /// Whether back and forward have somewhere to go.
    pub fn can_go(&self) -> (bool, bool) {
        let (mut back, mut forward) = (BOOL(0), BOOL(0));
        unsafe {
            let _ = self.webview.CanGoBack(&mut back);
            let _ = self.webview.CanGoForward(&mut forward);
        }
        (back.as_bool(), forward.as_bool())
    }

    /// Asks for the tab's history (WebView2 has no API of its own for it:
    /// DevTools' `Page.getNavigationHistory` answers); it arrives as
    /// `Event::NavHistory`.
    pub fn request_history(&self, id: u64, back: bool) {
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
            if result.is_ok() {
                push(Event::NavHistory(id, back, json));
            }
            Ok(())
        }));
        unsafe {
            let _ = self.webview.CallDevToolsProtocolMethod(
                &HSTRING::from("Page.getNavigationHistory"),
                &HSTRING::from("{}"),
                &handler,
            );
        }
    }

    /// Goes to an entry of the history `request_history` listed.
    pub fn go_to_history_entry(&self, entry: i64) {
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
        let params = format!(r#"{{"entryId":{entry}}}"#);
        unsafe {
            let _ = self.webview.CallDevToolsProtocolMethod(
                &HSTRING::from("Page.navigateToHistoryEntry"),
                &HSTRING::from(params),
                &handler,
            );
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

/// The page's scroll position, as the JSON array `[x, y]` in pixels.
const SCROLL_SCRIPT: &str = "[Math.round(window.scrollX), Math.round(window.scrollY)]";

/// The answer of `SCROLL_SCRIPT`, or `None` when it is not two numbers.
pub fn parse_scroll(json: &str) -> Option<(i64, i64)> {
    let values: Vec<f64> = serde_json::from_str(json).ok()?;
    match values[..] {
        [x, y] if x.is_finite() && y.is_finite() => Some((x as i64, y as i64)),
        _ => None,
    }
}

/// Goes to (`x`, `y`) only from the top: a page that the person (or the
/// page) scrolled already stays where it is.
pub fn restore_scroll_script(x: i64, y: i64) -> String {
    format!("if (!window.scrollX && !window.scrollY) window.scrollTo({x}, {y});")
}

/// What the engine says about a process that failed, for the log.
#[derive(Default)]
pub struct ProcessFailure {
    pub kind: COREWEBVIEW2_PROCESS_FAILED_KIND,
    reason: Option<COREWEBVIEW2_PROCESS_FAILED_REASON>,
    exit_code: Option<i32>,
    description: String,
}

impl ProcessFailure {
    fn read(args: &ICoreWebView2ProcessFailedEventArgs) -> Self {
        let mut failure = ProcessFailure::default();
        unsafe {
            let _ = args.ProcessFailedKind(&mut failure.kind);
            if let Ok(args2) = args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
                let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                if args2.Reason(&mut reason).is_ok() {
                    failure.reason = Some(reason);
                }
                let mut code = 0i32;
                if args2.ExitCode(&mut code).is_ok() {
                    failure.exit_code = Some(code);
                }
                let mut text = PWSTR::null();
                if args2.ProcessDescription(&mut text).is_ok() {
                    failure.description = take_pwstr(text);
                }
            }
        }
        failure
    }

    fn describe(&self) -> String {
        let kind = match self.kind {
            COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => "browser process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => "page process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => "page not responding",
            COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => "frame process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_UTILITY_PROCESS_EXITED => "utility process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_SANDBOX_HELPER_PROCESS_EXITED => "sandbox process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED => "GPU process ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_PPAPI_PLUGIN_PROCESS_EXITED => "plugin ended",
            COREWEBVIEW2_PROCESS_FAILED_KIND_PPAPI_BROKER_PROCESS_EXITED => "plugin broker ended",
            _ => "unknown process",
        };
        let reason = match self.reason {
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_UNEXPECTED) => "unexpected",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_UNRESPONSIVE) => "not responding",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_TERMINATED) => "terminated",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_CRASHED) => "crashed",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_LAUNCH_FAILED) => "did not start",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_OUT_OF_MEMORY) => "out of memory",
            Some(COREWEBVIEW2_PROCESS_FAILED_REASON_PROFILE_DELETED) => "profile deleted",
            Some(_) => "other reason",
            None => "unknown reason",
        };
        let mut text = format!("{kind} ({}, reason {reason}", self.kind.0);
        if let Some(code) = self.exit_code {
            text.push_str(&format!(", exit code 0x{code:08X}"));
        }
        if !self.description.is_empty() {
            text.push_str(&format!(", {}", self.description));
        }
        text.push(')');
        text
    }
}

fn tracking_level(block_trackers: bool) -> COREWEBVIEW2_TRACKING_PREVENTION_LEVEL {
    if block_trackers {
        COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT
    } else {
        COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED
    }
}

/// The engine's active match index is already 1-based (measured: the first
/// match of two reads 1, and the last one 2); -1 means none is active yet.
fn match_position(index: i32, count: i32) -> (i32, i32) {
    (index.clamp(1, count.max(1)), count)
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/web.rs"]
#[rustfmt::skip]
mod tests;
