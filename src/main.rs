#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod app_icon;
mod autofill;
mod bench;
mod bookmark_bar;
mod bookmarks;
mod cover;
mod crx;
mod curtain;
mod design;
mod diag;
mod download_watch;
mod downloads;
mod extension_host;
mod extensions;
mod fade;
mod favicon;
mod frame;
mod gfx;
mod hello;
mod history;
mod hovercard;
mod http;
mod image;
mod inject;
mod installer;
mod legacy;
mod lock_screen;
mod memory;
mod menu;
mod msg_param;
mod notifications;
mod notify_watch;
mod omnibar;
mod omnibox;
mod pacer;
mod panels;
mod pip;
mod profile_launch;
mod profile_lock;
mod profiles;
mod reader;
mod select;
mod session;
mod settings;
mod settings_window;
mod shield;
mod shortcuts;
mod space_card;
mod spaces;
mod split;
mod sqlite;
mod storage;
mod system_dll;
mod tab_groups;
mod tabbar;
mod tabs;
mod text_focus;
mod tooltip;
mod unzip;
mod update_sign;
mod updater;
mod vault;
mod vault_bar;
mod vault_crypto;
mod vault_import;
mod vault_watch;
mod vault_worker;
mod web;
mod webstore;
mod widgets;

// Shared by the tests that use the real Credential Manager.
#[cfg(all(test, nea_tests))]
#[path = "../tests/support/credentials.rs"]
#[rustfmt::skip]
mod test_credentials;

// Input generators for the fuzz loops.
#[cfg(all(test, nea_tests))]
#[path = "../tests/support/fuzz.rs"]
#[rustfmt::skip]
mod test_fuzz;

// Fuzz loops over every parser of outside data.
#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/fuzz.rs"]
#[rustfmt::skip]
mod fuzz_tests;

use windows::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

fn main() -> windows::core::Result<()> {
    // "Restart to update": the instance that started this one closes first.
    updater::wait_for_previous();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        // The manifest already asks for this; the call covers a build without it.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Some(code) = installer::run_from_args() {
        std::process::exit(code);
    }
    // Before any file is read: what the former name left behind moves over.
    legacy::migrate();
    // Then the profile: its folders and passwords, before anything is read.
    // The move above only ever looks at profile 0's.
    profile_launch::apply(profiles::from_args(std::env::args_os().skip(1)));
    if let Some(code) = installer::move_legacy_install() {
        std::process::exit(code);
    }
    // A verified update was waiting: the new executable is already running.
    if updater::apply_pending() {
        return Ok(());
    }
    diag::install(&storage::data_dir());
    app::run()?;
    // The window closed and saved the session: the update goes in, and the
    // new version starts.
    if updater::restart_requested() {
        updater::restart_into_update();
    }
    Ok(())
}
