//! The engine side of web notifications: a page asks to notify
//! (PermissionRequested) and sends a notification (NotificationReceived,
//! ICoreWebView2_24). As everywhere else, callbacks only queue a message
//! for the app; a request waits, held with its deferral, until the app
//! answers it (`answer`).

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{BOOL, HSTRING, Interface, PWSTR, Result};

use crate::web::{self, Event};

/// What a tab's page did about notifications.
pub enum Message {
    /// It asked to show notifications: the page's address, and the request
    /// held until `answer`.
    Asked(String, u64),
    /// It sent one.
    Received(Received),
    /// It closed the one the app showed under this key.
    Closed(u64),
}

/// A notification a page sent.
pub struct Received {
    /// The origin of the page that sent it, as the engine says it.
    pub origin: String,
    pub title: String,
    pub body: String,
    /// A notification with the tag of an earlier one replaces it.
    pub tag: String,
    pub silent: bool,
    pub notification: ICoreWebView2Notification,
}

/// The answer to a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Block,
    /// No decision: the page may ask again.
    Later,
}

type Held = (ICoreWebView2PermissionRequestedEventArgs, ICoreWebView2Deferral);

thread_local! {
    static HELD: RefCell<HashMap<u64, Held>> = RefCell::new(HashMap::new());
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

fn read(f: impl FnOnce(*mut PWSTR) -> Result<()>) -> String {
    let mut value = PWSTR::null();
    if f(&mut value).is_ok() { take_pwstr(value) } else { String::new() }
}

/// Wires the notification events of a view. The other permissions keep the
/// engine's own question.
pub fn watch(webview: &ICoreWebView2, tab: u64) -> Result<()> {
    let mut token = 0i64;
    unsafe {
        webview.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut kind)?;
                if kind != COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS {
                    return Ok(());
                }
                let uri = read(|p| args.Uri(p));
                // The question is Nea's, not the engine's.
                if let Ok(args2) = args.cast::<ICoreWebView2PermissionRequestedEventArgs2>() {
                    let _ = args2.SetHandled(true);
                }
                let deferral = args.GetDeferral()?;
                let id = NEXT.with(|n| n.replace(n.get() + 1));
                HELD.with(|h| h.borrow_mut().insert(id, (args.clone(), deferral)));
                web::push(Event::Notify(tab, Message::Asked(uri, id)));
                Ok(())
            })),
            &mut token,
        )?;
        let Ok(w24) = webview.cast::<ICoreWebView2_24>() else { return Ok(()) };
        w24.add_NotificationReceived(
            &NotificationReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let notification = args.Notification()?;
                // Handled before the callback returns, or the engine shows
                // its own.
                args.SetHandled(true)?;
                let mut silent = BOOL(0);
                let _ = notification.IsSilent(&mut silent);
                let received = Received {
                    origin: read(|p| args.SenderOrigin(p)),
                    title: read(|p| notification.Title(p)),
                    body: read(|p| notification.Body(p)),
                    tag: read(|p| notification.Tag(p)),
                    silent: silent.as_bool(),
                    notification,
                };
                web::push(Event::Notify(tab, Message::Received(received)));
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Hears when the page closes the notification shown under `key`.
pub fn watch_close(notification: &ICoreWebView2Notification, tab: u64, key: u64) {
    let mut token = 0i64;
    unsafe {
        let _ = notification.add_CloseRequested(
            &NotificationCloseRequestedEventHandler::create(Box::new(move |_, _| {
                web::push(Event::Notify(tab, Message::Closed(key)));
                Ok(())
            })),
            &mut token,
        );
    }
}

/// Answers a held request. `remember`: the engine keeps the decision in
/// the profile, so `Notification.permission` says it on the next visit
/// (a site such as Slack reads it before it sends anything).
pub fn answer(request: u64, answer: Answer, remember: bool) {
    let Some((args, deferral)) = HELD.with(|h| h.borrow_mut().remove(&request)) else { return };
    let state = match answer {
        Answer::Allow => COREWEBVIEW2_PERMISSION_STATE_ALLOW,
        Answer::Block => COREWEBVIEW2_PERMISSION_STATE_DENY,
        Answer::Later => COREWEBVIEW2_PERMISSION_STATE_DEFAULT,
    };
    unsafe {
        if let Ok(args3) = args.cast::<ICoreWebView2PermissionRequestedEventArgs3>() {
            let _ = args3.SetSavesInProfile(remember && answer != Answer::Later);
        }
        let _ = args.SetState(state);
        let _ = deferral.Complete();
    }
}

fn profile(webview: &ICoreWebView2) -> Option<ICoreWebView2Profile4> {
    unsafe { webview.cast::<ICoreWebView2_13>().ok()?.Profile().ok()?.cast::<ICoreWebView2Profile4>().ok() }
}

/// The notification decisions the profile holds (some may predate Nea's
/// own question: the engine's), as (origin, allowed). They come to
/// `done` on this thread.
pub fn read_profile(webview: &ICoreWebView2, done: impl FnOnce(Vec<(String, bool)>) + 'static) {
    let Some(profile) = profile(webview) else { return };
    let mut done = Some(done);
    let handler = GetNonDefaultPermissionSettingsCompletedHandler::create(Box::new(move |result, list| {
        let mut found = Vec::new();
        if result.is_ok()
            && let Some(list) = list
        {
            let mut count = 0u32;
            unsafe {
                let _ = list.Count(&mut count);
                for i in 0..count {
                    let Ok(setting) = list.GetValueAtIndex(i) else { continue };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    let mut state = COREWEBVIEW2_PERMISSION_STATE::default();
                    let _ = setting.PermissionKind(&mut kind);
                    let _ = setting.PermissionState(&mut state);
                    if kind != COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS {
                        continue;
                    }
                    let origin = read(|p| setting.PermissionOrigin(p));
                    match state {
                        COREWEBVIEW2_PERMISSION_STATE_ALLOW => found.push((origin, true)),
                        COREWEBVIEW2_PERMISSION_STATE_DENY => found.push((origin, false)),
                        _ => {}
                    }
                }
            }
        }
        if let Some(done) = done.take() {
            done(found);
        }
        Ok(())
    }));
    unsafe {
        let _ = profile.GetNonDefaultPermissionSettings(&handler);
    }
}

/// Makes the profile forget its decision for `origin`: the site asks again.
pub fn forget_in_profile(webview: &ICoreWebView2, origin: &str) {
    let Some(profile) = profile(webview) else { return };
    let handler = SetPermissionStateCompletedHandler::create(Box::new(|_| Ok(())));
    unsafe {
        let _ = profile.SetPermissionState(
            COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS,
            &HSTRING::from(origin),
            COREWEBVIEW2_PERMISSION_STATE_DEFAULT,
            &handler,
        );
    }
}

/// Tells the engine what became of a notification it handed over.
pub fn report(notification: &ICoreWebView2Notification, what: Report) {
    unsafe {
        let _ = match what {
            Report::Shown => notification.ReportShown(),
            Report::Clicked => notification.ReportClicked(),
            Report::Closed => notification.ReportClosed(),
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    Shown,
    Clicked,
    Closed,
}
