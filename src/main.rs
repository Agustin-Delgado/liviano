#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bench;
mod bookmarks;
mod crx;
mod curtain;
mod design;
mod diag;
mod download_watch;
mod downloads;
mod extension_host;
mod frame;
mod extensions;
mod favicon;
mod gfx;
mod history;
mod hovercard;
mod http;
mod image;
mod inject;
mod installer;
mod omnibar;
mod omnibox;
mod panels;
mod pip;
mod reader;
mod session;
mod settings;
mod settings_window;
mod shield;
mod shortcuts;
mod split;
mod sqlite;
mod storage;
mod system_dll;
mod tabbar;
mod tabs;
mod update_sign;
mod updater;
mod vault;
mod vault_bar;
mod vault_crypto;
mod vault_import;
mod vault_watch;
mod unzip;
mod web;
mod webstore;
mod widgets;

use windows::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

fn main() -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        // The manifest already asks for this; the call covers a build without it.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Some(code) = installer::run_from_args() {
        std::process::exit(code);
    }
    // A verified update was waiting: the new executable is already running.
    if updater::apply_pending() {
        return Ok(());
    }
    diag::install(&storage::data_dir());
    app::run()
}
