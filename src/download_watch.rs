//! The engine side of downloads: every view reports the downloads it starts
//! (DownloadStarting, ICoreWebView2_4) and each one its progress and end. As
//! everywhere else, callbacks only queue a snapshot for the app.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{Interface, PWSTR, Result};

use crate::{
    downloads::{Download, State},
    web::{self, Event},
};

/// Progress is reported at most this often: the panel doesn't need more and
/// the queue stays short on a fast connection.
const PROGRESS_EVERY: Duration = Duration::from_millis(400);

thread_local! {
    /// Operations still running, so a download can be cancelled from the panel.
    static RUNNING: RefCell<HashMap<u64, ICoreWebView2DownloadOperation>> = RefCell::new(HashMap::new());
    static LAST_ID: Cell<u64> = const { Cell::new(0) };
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Ids stay unique across runs: they start at the current time in milliseconds.
fn next_id() -> u64 {
    LAST_ID.with(|last| {
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let id = now_ms.max(last.get() + 1);
        last.set(id);
        id
    })
}

fn read_string(f: impl FnOnce(*mut PWSTR) -> Result<()>) -> String {
    let mut value = PWSTR::null();
    if f(&mut value).is_ok() { take_pwstr(value) } else { String::new() }
}

fn snapshot(id: u64, started: u64, operation: &ICoreWebView2DownloadOperation) -> Download {
    unsafe {
        let mut state = COREWEBVIEW2_DOWNLOAD_STATE::default();
        let _ = operation.State(&mut state);
        let (mut received, mut total) = (0i64, 0i64);
        let _ = operation.BytesReceived(&mut received);
        let _ = operation.TotalBytesToReceive(&mut total);
        Download {
            id,
            url: read_string(|p| operation.Uri(p)),
            path: read_string(|p| operation.ResultFilePath(p)),
            state: match state {
                COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED => State::Completed,
                COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED => State::Interrupted,
                _ => State::InProgress,
            },
            received: received.max(0) as u64,
            total: (total > 0).then_some(total as u64),
            started,
        }
    }
}

/// Starts reporting the downloads of `webview` (the view of tab `tab_id`).
/// The engine's own download flyout stays: it is the immediate feedback; the
/// panel is the list.
pub fn watch(webview: &ICoreWebView2, tab_id: u64) -> Result<()> {
    let w4 = webview.cast::<ICoreWebView2_4>()?;
    let mut token = 0i64;
    unsafe {
        w4.add_DownloadStarting(
            &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let operation = args.DownloadOperation()?;
                let id = next_id();
                let started = unix_now();
                follow(id, started, &operation)?;
                RUNNING.with(|r| r.borrow_mut().insert(id, operation.clone()));
                let mut download = snapshot(id, started, &operation);
                if download.path.is_empty() {
                    download.path = read_string(|p| args.ResultFilePath(p));
                }
                web::push(Event::Download(download));
                // The tab was sent to the file's address, but the page never
                // left: read the address back so the session doesn't keep (and
                // download again on the next start) the file's one.
                web::push(Event::SourceChanged(tab_id));
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

fn follow(id: u64, started: u64, operation: &ICoreWebView2DownloadOperation) -> Result<()> {
    let mut token = 0i64;
    let last = Cell::new(Instant::now());
    unsafe {
        operation.add_BytesReceivedChanged(
            &BytesReceivedChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(operation) = sender {
                    if last.get().elapsed() >= PROGRESS_EVERY {
                        last.set(Instant::now());
                        web::push(Event::Download(snapshot(id, started, &operation)));
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        operation.add_StateChanged(
            &StateChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(operation) = sender {
                    let download = snapshot(id, started, &operation);
                    if download.state != State::InProgress {
                        RUNNING.with(|r| r.borrow_mut().remove(&id));
                    }
                    web::push(Event::Download(download));
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Cancels a running download. False when it isn't running any more.
pub fn cancel(id: u64) -> bool {
    let operation = RUNNING.with(|r| r.borrow().get(&id).cloned());
    match operation {
        Some(operation) => unsafe { operation.Cancel().is_ok() },
        None => false,
    }
}
