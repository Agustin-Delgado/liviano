//! A locked window: the pages hidden (they keep running), the chrome gone,
//! the lock screen over the whole client area, and only Enter, Space and
//! the screen's own buttons answering. It opens again after Windows Hello,
//! or the Windows password when Hello is not set up. The rules are in
//! `profile_lock`, the prompts in `hello`, the drawing in `lock_screen`.
//!
//! An open window locks after the person was away (no input on the whole
//! system for the time the profile asks) and when Windows locks the
//! session. Both are watched only for a profile that asks for them.

use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::HDC,
    System::RemoteDesktop::{
        NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
    },
    UI::{
        Input::KeyboardAndMouse::{GetFocus, ReleaseCapture, SetCapture, SetFocus},
        WindowsAndMessaging::{IsChild, KillTimer, SetTimer, SetWindowTextW},
    },
};

use super::{App, retry_later, wide, with_app};
use crate::{
    frame, gfx,
    hello::{self, Step},
    lock_screen::{self, LockHit, LockPaint, Places},
    profile_launch,
    profile_lock::{self, HelloNext, LockOptions, LockedKey, Reason},
    profiles, settings_window,
    tabbar::{Hit, Metrics},
};

/// Checks how long the person was away. Cheap (one call), and a lock 15 s
/// late after 5 minutes is not noticed.
const TIMER_LOCK: usize = 0x50;
const AWAY_CHECK_MS: u32 = 15_000;
/// The session's notices (not in the bindings' WindowsAndMessaging set).
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 0x7;

/// What a verification is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Unlock,
    /// New lock options for a profile, saved once the person is verified.
    Apply {
        id: u32,
        options: LockOptions,
    },
}

#[derive(Default)]
pub(super) struct LockUi {
    /// Why the window is locked; None while it is open.
    reason: Option<Reason>,
    /// A verification on its way, and what for.
    verifying: Option<Purpose>,
    /// The number of the last verification: an answer to an older one is
    /// dropped.
    request: u64,
    /// What the last try said ("Windows Hello did not verify you.").
    note: Option<String>,
    hover: Option<LockHit>,
    pressed: Option<LockHit>,
    /// Windows Hello is set up; None until asked.
    hello: Option<bool>,
    /// The away timer runs.
    timer: bool,
    /// The session's notices come.
    session: bool,
}

/// The lock's messages; None lets the window proc go on.
pub(super) fn window_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    match msg {
        hello::WM_APP_LOCK => {
            let Some(step) = Step::from_wparam(wparam.0) else { return Some(LRESULT(0)) };
            let (request, code) = hello::unpack(lparam.0);
            if with_app(|app| app.lock_answer(step, request, code)).is_none() {
                retry_later(hwnd, msg, wparam, lparam);
            }
            Some(LRESULT(0))
        }
        WM_WTSSESSION_CHANGE if wparam.0 == WTS_SESSION_LOCK => {
            if with_app(|app| app.windows_locked()).is_none() {
                retry_later(hwnd, msg, wparam, lparam);
                return Some(LRESULT(0));
            }
            // The power messages also hide the views (`power_ui`).
            None
        }
        _ => None,
    }
}

unsafe extern "system" fn on_lock_timer(_: HWND, _: u32, _: usize, _: u32) {
    with_app(|app| app.away_tick());
}

impl App {
    pub(super) fn locked(&self) -> bool {
        self.lock.reason.is_some()
    }

    fn lock_options(&self) -> LockOptions {
        self.profiles.lock_of(profile_launch::current())
    }

    /// Before the window shows: a profile that asks for it opens locked,
    /// and what locks an open window is watched.
    pub(super) fn lock_at_start(&mut self) {
        if self.lock_options().starts_locked() {
            self.lock_now(Reason::Started);
        }
        self.sync_lock_watch();
    }

    /// The away timer and the session's notices follow the options.
    pub(super) fn sync_lock_watch(&mut self) {
        let options = self.lock_options();
        let timer = options.away_limit().is_some();
        if timer != self.lock.timer {
            unsafe {
                if timer {
                    SetTimer(Some(self.hwnd), TIMER_LOCK, AWAY_CHECK_MS, Some(on_lock_timer));
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_LOCK);
                }
            }
            self.lock.timer = timer;
        }
        let session = options.locks_with_windows();
        if session != self.lock.session {
            let done = unsafe {
                if session {
                    WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION).is_ok()
                } else {
                    WTSUnRegisterSessionNotification(self.hwnd).is_ok()
                }
            };
            if done {
                self.lock.session = session;
            }
        }
    }

    fn away_tick(&mut self) {
        let idle = super::system_idle();
        if let Some(reason) = profile_lock::away_lock(self.lock_options(), self.locked(), idle) {
            self.lock_now(reason);
        }
    }

    fn windows_locked(&mut self) {
        if let Some(reason) = profile_lock::session_lock(self.lock_options(), self.locked()) {
            self.lock_now(reason);
        }
    }

    /// Locks the window: what floats over the page goes, the pages leave
    /// the screen (they keep running) and the lock screen takes the window.
    pub(super) fn lock_now(&mut self, reason: Reason) {
        if self.locked() {
            return;
        }
        crate::diag::trace(|| format!("lock: {reason:?}"));
        if self.mode.is_some() {
            self.close_omnibar();
        }
        if self.fullscreen.on() {
            self.leave_fullscreen();
        }
        self.autofill_close();
        self.hide_card();
        self.hide_tip();
        self.hide_space_name();
        self.drag = None;
        settings_window::dismiss();
        self.lock.reason = Some(reason);
        self.lock.note = None;
        self.lock.hover = None;
        self.lock.pressed = None;
        // A verification of options on its way is not this one's.
        if matches!(self.lock.verifying, Some(Purpose::Apply { .. })) {
            self.lock.verifying = None;
        }
        self.hover = None;
        self.relayout();
        self.place_vault_bar();
        self.keyboard_to_window();
        self.update_title();
        self.invalidate();
    }

    /// The window opens again as it was: the active page shows and gets
    /// the keyboard.
    fn unlock(&mut self) {
        if !self.locked() {
            return;
        }
        crate::diag::trace(|| "unlock".to_string());
        self.lock.reason = None;
        self.lock.note = None;
        self.lock.hover = None;
        self.lock.pressed = None;
        self.relayout();
        self.update_title();
        self.focus_page();
        // A window with no tab opens with the field, as at start.
        if self.tabs.tabs.is_empty() && self.painted {
            self.open_omnibar(super::Mode::NewTab);
        }
        self.invalidate();
    }

    /// The keyboard leaves a page for the window (a hidden page must get
    /// no key).
    fn keyboard_to_window(&self) {
        unsafe {
            let focus = GetFocus();
            if !focus.is_invalid() && (focus == self.hwnd || IsChild(self.hwnd, focus).as_bool()) {
                let _ = SetFocus(Some(self.hwnd));
            }
        }
    }

    // ---------- the screen ----------

    /// The layout of a locked window: the caption row with the window's
    /// buttons, and nothing else.
    pub(super) fn lay_out_locked(&mut self, width: i32, height: i32) {
        self.layout = crate::tabbar::layout_locked(width, height, self.vertical, &Metrics { scale: self.scale });
    }

    pub(super) fn locked_title(&self) -> Option<String> {
        self.locked().then(|| {
            let name = self.profiles.get(profile_launch::current()).name;
            profile_lock::locked_title(crate::storage::DISPLAY_NAME, &name)
        })
    }

    fn lock_places(&self) -> Places {
        let (width, height) = self.client_size();
        let s = self.scale;
        let button = gfx::measure(lock_screen::UNLOCK_LABEL, lock_screen::button_font(s));
        let other = self.profiles.shown().then(|| gfx::measure(lock_screen::OTHER_LABEL, lock_screen::link_font(s)));
        lock_screen::place(width, height, self.layout.frame.caption.bottom, s, button, other)
    }

    /// Draws the lock screen, and the window's buttons over it.
    pub(super) fn paint_locked(&mut self, hdc: HDC, dirty: RECT) {
        self.frame_now = None;
        self.painted_sheet = None;
        let (width, height) = self.client_size();
        let dirty = RECT {
            left: dirty.left.max(0),
            top: dirty.top.max(0),
            right: dirty.right.min(width),
            bottom: dirty.bottom.min(height),
        };
        if dirty.right <= dirty.left || dirty.bottom <= dirty.top {
            return;
        }
        let palette = self.palette;
        let profile = self.profiles.get(profile_launch::current());
        let line = profile_lock::lock_line(self.lock.reason.unwrap_or(Reason::Asked));
        let paint = LockPaint {
            palette: &palette,
            scale: self.scale,
            places: self.lock_places(),
            name: &profile.name,
            initial: profiles::initial(&profile.name),
            color: profile.color,
            line: &line,
            note: self.lock.note.as_deref(),
            hover: self.lock.hover,
            pressed: self.lock.pressed,
            waiting: self.lock.verifying.is_some(),
        };
        let maximized = self.is_maximized();
        let mut surface = std::mem::take(&mut self.surface);
        for rect in gfx::visible_rects(hdc, self.hwnd, dirty) {
            let drawn = surface.paint(hdc, rect, |g| {
                lock_screen::paint(g, width, height, &paint);
                // No fades here: the strip's frame clock does not run on
                // this screen, so a hover is on or off.
                let hover = |b| if self.hover == Some(Hit::Window(b)) { 1.0 } else { 0.0 };
                frame::paint(
                    g,
                    &self.layout.frame,
                    &palette,
                    self.scale,
                    maximized,
                    !self.frame.inactive,
                    self.frame.pressed,
                    hover,
                );
            });
            if !drawn {
                gfx::fill_gdi(hdc, rect, palette.chrome);
            }
        }
        self.surface = surface;
    }

    /// The pointer over the lock screen: its buttons light up, and the
    /// window's buttons as anywhere.
    pub(super) fn lock_mouse_move(&mut self, x: i32, y: i32) {
        let hover = self.lock_places().hit(x, y);
        if hover != self.lock.hover {
            self.lock.hover = hover;
            self.invalidate();
        }
        self.set_hover(self.layout.hit(x, y));
    }

    pub(super) fn lock_mouse_leave(&mut self) {
        if self.lock.hover.take().is_some() {
            self.invalidate();
        }
    }

    pub(super) fn lock_mouse_down(&mut self, x: i32, y: i32) {
        self.hide_tip();
        match self.lock_places().hit(x, y) {
            Some(LockHit::Unlock) => {
                self.lock.pressed = Some(LockHit::Unlock);
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.invalidate();
            }
            Some(LockHit::Other) => self.profile_menu(x, y),
            None => {
                if let Some(Hit::Window(button)) = self.layout.hit(x, y) {
                    self.press_window_button(button);
                }
            }
        }
    }

    /// A press on the button acts when it is released on it.
    pub(super) fn lock_mouse_up(&mut self, x: i32, y: i32) {
        if self.lock.pressed.take() != Some(LockHit::Unlock) {
            return;
        }
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        if self.lock_places().hit(x, y) == Some(LockHit::Unlock) {
            self.begin_unlock();
        }
    }

    /// The hand over what answers a click.
    pub(super) fn lock_hand_cursor(&self) -> bool {
        self.locked() && self.lock.hover.is_some()
    }

    /// A key while locked: None when the window is open (the key goes on
    /// as usual), Some(true) when it is taken, Some(false) when Windows
    /// gets it (Alt+F4).
    pub(super) fn lock_key(&mut self, vk: u32, repeat: bool) -> Option<bool> {
        if !self.locked() {
            return None;
        }
        let chord = crate::web::current_chord(vk);
        match profile_lock::locked_key(vk, chord.ctrl, chord.shift, chord.alt) {
            LockedKey::Unlock => {
                if !repeat {
                    self.begin_unlock();
                }
                Some(true)
            }
            LockedKey::Pass => Some(false),
            LockedKey::Swallow => Some(true),
        }
    }

    // ---------- verifying ----------

    fn begin_unlock(&mut self) {
        if self.locked() {
            self.verify(Purpose::Unlock);
        }
    }

    fn verify(&mut self, purpose: Purpose) {
        if self.lock.verifying.is_some() {
            return;
        }
        self.lock.request += 1;
        self.lock.verifying = Some(purpose);
        self.lock.note = None;
        self.invalidate();
        hello::check(self.hwnd, Step::Availability, self.lock.request);
    }

    /// What a prompt answered.
    fn lock_answer(&mut self, step: Step, request: u64, code: u8) {
        if step == Step::SettingsAvailability {
            self.hello_known(profile_lock::hello_available(code));
            return;
        }
        let Some(purpose) = self.lock.verifying.filter(|_| request == self.lock.request) else { return };
        match step {
            Step::Availability => {
                let available = profile_lock::hello_available(code);
                self.hello_known(available);
                match (available, purpose) {
                    (true, _) => hello::verify(self.hwnd, &self.hello_message(purpose), request),
                    (false, Purpose::Unlock) => self.ask_password(request),
                    (false, Purpose::Apply { .. }) => {
                        self.verified(false, Some("Set up Windows Hello in Windows Settings first. Nothing changed."))
                    }
                }
            }
            Step::Hello => match (profile_lock::after_hello(code), purpose) {
                (HelloNext::Verified, _) => self.verified(true, None),
                (HelloNext::Canceled, _) => self.verified(false, None),
                (HelloNext::Fallback, Purpose::Unlock) => self.ask_password(request),
                (HelloNext::Fallback, Purpose::Apply { .. }) => {
                    self.verified(false, Some("Windows Hello did not verify you. Nothing changed."))
                }
            },
            Step::Password => match code {
                hello::PASSWORD_VERIFIED => self.verified(true, None),
                hello::PASSWORD_CANCELED => self.verified(false, None),
                hello::PASSWORD_WRONG => self.verified(false, Some("The password did not match.")),
                _ => self.verified(false, Some("The password prompt did not open.")),
            },
            Step::SettingsAvailability => {}
        }
    }

    fn hello_message(&self, purpose: Purpose) -> String {
        match purpose {
            Purpose::Unlock => format!("Unlock {} in Nea", self.profiles.get(profile_launch::current()).name),
            Purpose::Apply { id, .. } => format!("Change the lock of {} in Nea", self.profiles.get(id).name),
        }
    }

    /// Hello cannot verify now (not set up, no device, too many tries):
    /// the Windows password opens the profile, so no one is kept out of it.
    fn ask_password(&self, request: u64) {
        let name = self.profiles.get(profile_launch::current()).name;
        let message = format!("Windows Hello cannot verify you now. Type your Windows password to unlock {name}.");
        hello::password(self.hwnd, crate::storage::DISPLAY_NAME.to_string(), message, request);
    }

    fn verified(&mut self, ok: bool, note: Option<&str>) {
        let Some(purpose) = self.lock.verifying.take() else { return };
        match purpose {
            Purpose::Unlock => {
                if ok {
                    self.unlock();
                } else {
                    self.lock.note = note.map(str::to_owned);
                    self.invalidate();
                }
            }
            Purpose::Apply { id, options } => {
                let result = if ok { profile_launch::set_lock(id, options) } else { Err(String::new()) };
                let line = match (&result, note) {
                    (Ok(()), _) => lock_saved(options),
                    (Err(why), _) if !why.is_empty() => why.clone(),
                    (Err(_), Some(note)) => note.to_owned(),
                    (Err(_), None) => "Nothing changed.".to_owned(),
                };
                settings_window::show_status(&line);
                self.reload_profiles();
                // The controls go back to what is saved, also when nothing
                // changed in the file.
                self.sync_settings_profiles();
            }
        }
    }

    // ---------- Settings ----------

    /// Asks once whether Hello is set up, for the Profiles group.
    pub(super) fn check_hello_for_settings(&self) {
        hello::check(self.hwnd, Step::SettingsAvailability, 0);
    }

    fn hello_known(&mut self, available: bool) {
        self.lock.hello = Some(available);
        settings_window::set_lock_hello(Some(available));
    }

    /// The Profiles group asks for new lock options for `id`.
    pub(super) fn set_lock_options(&mut self, id: u32, options: LockOptions) {
        let before = self.profiles.lock_of(id);
        let own = id == profile_launch::current();
        if profile_lock::needs_verification(before, options, own) {
            if self.lock.verifying.is_some() {
                settings_window::show_status("Another prompt is open. Try again when it closes.");
                self.sync_settings_profiles();
                return;
            }
            settings_window::show_status("Confirm it is you in the Windows Hello prompt…");
            self.verify(Purpose::Apply { id, options });
            return;
        }
        match profile_launch::set_lock(id, options) {
            Ok(()) => settings_window::show_status(&lock_saved(options)),
            Err(why) => settings_window::show_status(&why),
        }
        self.reload_profiles();
        self.sync_settings_profiles();
    }

    /// The title while locked has no page in it.
    pub(super) fn set_locked_title(&self) -> bool {
        let Some(title) = self.locked_title() else { return false };
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &wide(&title));
        }
        true
    }

    // ---------- script control ----------

    pub(super) fn lock_state(&self) -> serde_json::Value {
        serde_json::json!({
            "locked": self.locked(),
            "reason": self.lock.reason.map(|r| r.text()),
            "verifying": self.lock.verifying.is_some(),
            "options": {
                "lock": self.lock_options().lock,
                "starts_locked": self.lock_options().starts_locked(),
                "watched": self.lock.timer || self.lock.session,
            },
        })
    }

    /// A script's unlock, without the verification: only in a test run
    /// (`profile_lock::script_unlock_allowed`).
    pub(super) fn script_unlock(&mut self) -> Result<(), String> {
        let bench_on = crate::bench::wanted(self.settings.bench_enabled);
        let data = std::env::var_os("NEA_DATA_DIR");
        if !profile_lock::script_unlock_allowed(bench_on, data.as_deref()) {
            return Err("unlock is only for a test run (NEA_BENCH=1 and NEA_DATA_DIR)".to_string());
        }
        self.lock.verifying = None;
        self.unlock();
        Ok(())
    }
}

/// What the status line says once the options are saved.
fn lock_saved(options: LockOptions) -> String {
    if options.locks() { "The lock is saved.".to_owned() } else { "The lock is off.".to_owned() }
}
