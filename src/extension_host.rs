//! Extensions on the engine side: WebView2's profile lists, loads, enables
//! and removes them. Every call is asynchronous; its outcome comes back as
//! an `Event::Extension` through the app's queue, like any engine event.
//! Extensions only exist in an environment created with them enabled
//! (`web::create_environment`), and the profile is reached through any view.

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{BOOL, HSTRING, Interface, PWSTR};

use crate::{
    extensions::EngineList,
    web::{Event, push},
};

#[derive(Debug)]
pub enum HostEvent {
    Listed(std::result::Result<EngineList, String>),
    /// The folder, and the engine's id and name for it.
    Added(String, std::result::Result<(String, String), String>),
    Changed,
}

fn profile(webview: &ICoreWebView2) -> Option<ICoreWebView2Profile7> {
    unsafe { webview.cast::<ICoreWebView2_13>().ok()?.Profile().ok()?.cast::<ICoreWebView2Profile7>().ok() }
}

fn describe(error: &windows::core::Error) -> String {
    let message = error.message();
    if message.trim().is_empty() { format!("{:#010x}", error.code().0) } else { message.trim().to_string() }
}

fn id_and_name(extension: &ICoreWebView2BrowserExtension) -> Option<(String, String)> {
    let (mut id, mut name) = (PWSTR::null(), PWSTR::null());
    unsafe {
        extension.Id(&mut id).ok()?;
        extension.Name(&mut name).ok()?;
    }
    Some((take_pwstr(id), take_pwstr(name)))
}

/// Runs `then` with the profile's extensions, or reports why it can't.
fn with_list(webview: &ICoreWebView2, then: impl FnOnce(Vec<ICoreWebView2BrowserExtension>) + 'static) -> bool {
    let Some(profile) = profile(webview) else { return false };
    let mut then = Some(then);
    let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |result, list| {
        let mut items = Vec::new();
        if let (Ok(()), Some(list)) = (result, list) {
            let mut count = 0u32;
            unsafe {
                let _ = list.Count(&mut count);
                items.extend((0..count).filter_map(|i| list.GetValueAtIndex(i).ok()));
            }
        }
        if let Some(then) = then.take() {
            then(items);
        }
        Ok(())
    }));
    unsafe { profile.GetBrowserExtensions(&handler).is_ok() }
}

/// What the profile has: engine id -> (name, enabled).
pub fn list(webview: &ICoreWebView2) {
    let started = with_list(webview, |items| {
        let mut engine = EngineList::new();
        for item in items {
            let mut on = BOOL(0);
            unsafe {
                let _ = item.IsEnabled(&mut on);
            }
            if let Some((id, name)) = id_and_name(&item) {
                engine.insert(id, (name, on.as_bool()));
            }
        }
        push(Event::Extension(HostEvent::Listed(Ok(engine))));
    });
    if !started {
        push(Event::Extension(HostEvent::Listed(Err("el motor no admite extensiones".to_string()))));
    }
}

/// Loads an unpacked folder into the profile (also how a store package,
/// already unpacked, is installed). The profile keeps it between runs.
/// Adding a path that is already loaded reloads it in place — same id,
/// storage kept — which is what "Recargar" and a store update use; removing
/// it first would reload it too, but wipes its storage (docs/extensiones.md).
pub fn add(webview: &ICoreWebView2, folder: &str) {
    match profile(webview) {
        Some(profile) => add_with(&profile, folder),
        None => push(Event::Extension(HostEvent::Added(folder.to_string(), Err("el motor no admite extensiones".to_string())))),
    }
}

/// Enables or disables the extensions in `enable`, removes those in
/// `remove`; one `Changed` event when the requests are out.
pub fn apply(webview: &ICoreWebView2, enable: Vec<(String, bool)>, remove: Vec<String>) {
    if enable.is_empty() && remove.is_empty() {
        return;
    }
    let started = with_list(webview, move |items| {
        for item in items {
            let Some((id, _)) = id_and_name(&item) else { continue };
            unsafe {
                if remove.contains(&id) {
                    let _ = item.Remove(&BrowserExtensionRemoveCompletedHandler::create(Box::new(|_| Ok(()))));
                } else if let Some((_, on)) = enable.iter().find(|(e, _)| *e == id) {
                    let _ = item.Enable(*on, &BrowserExtensionEnableCompletedHandler::create(Box::new(|_| Ok(()))));
                }
            }
        }
        push(Event::Extension(HostEvent::Changed));
    });
    if !started {
        push(Event::Extension(HostEvent::Changed));
    }
}

fn add_with(profile: &ICoreWebView2Profile7, folder: &str) {
    let path = folder.to_string();
    let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |result, extension| {
        let outcome = match (result, extension) {
            (Ok(()), Some(extension)) => id_and_name(&extension).ok_or_else(|| "sin datos".to_string()),
            (Ok(()), None) => Err("sin datos".to_string()),
            (Err(error), _) => Err(describe(&error)),
        };
        push(Event::Extension(HostEvent::Added(path.clone(), outcome)));
        Ok(())
    }));
    unsafe {
        if let Err(error) = profile.AddBrowserExtension(&HSTRING::from(folder), &handler) {
            push(Event::Extension(HostEvent::Added(folder.to_string(), Err(describe(&error)))));
        }
    }
}
