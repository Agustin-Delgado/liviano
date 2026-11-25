//! The executable is its own installer: `--install` copies it to
//! %LOCALAPPDATA%\Programs\Nea, adds a Start menu shortcut and an entry
//! in Apps (HKCU, so no administrator rights), and `--uninstall` takes all of
//! that back out. A copy whose file name contains "setup" installs itself
//! when opened, which is what the downloadable installer is.
//!
//! For tests every place can be moved with an environment variable:
//! NEA_INSTALL_DIR, NEA_START_MENU_DIR and NEA_UNINSTALL_KEY
//! (a path under HKCU\Software).

use std::{
    fs,
    path::{Path, PathBuf},
};

use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
use windows::{
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile},
            Registry::{
                HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
                RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
            },
        },
        UI::{
            Shell::{FOLDERID_Desktop, IShellLinkW, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath, ShellExecuteW, ShellLink},
            WindowsAndMessaging::{
                IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_STYLE,
                MessageBoxW, SW_SHOWNORMAL,
            },
        },
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR},
};

use crate::{legacy, storage, updater};

const EXE_NAME: &str = "nea.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Nea";
const WEBVIEW2_CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
/// Microsoft's Evergreen bootstrapper: a small download that installs the runtime.
pub const WEBVIEW2_BOOTSTRAPPER: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Install,
    Uninstall,
}

/// What the command line (and the file name) ask for, and whether to do it
/// without a word (`--quiet`, for winget and tests).
fn action(args: &[String], exe_stem: &str) -> Option<(Action, bool)> {
    let quiet = args.iter().any(|a| a == "--quiet");
    if args.iter().any(|a| a == "--uninstall") {
        return Some((Action::Uninstall, quiet));
    }
    if args.iter().any(|a| a == "--install") || exe_stem.to_ascii_lowercase().contains("setup") {
        return Some((Action::Install, quiet));
    }
    None
}

/// Handles --install / --uninstall. `Some(exit code)` when it did, and the
/// browser must not start.
pub fn run_from_args() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().ok()?;
    let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (action, quiet) = action(&args, &stem)?;
    let result = match action {
        Action::Install => install(&exe, quiet),
        Action::Uninstall => uninstall(&exe, quiet),
    };
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            if !quiet {
                message(&error, MB_OK | MB_ICONERROR);
            }
            1
        }
    })
}

struct Places {
    dir: PathBuf,
    shortcut: PathBuf,
    key: String,
}

/// A key path below HKCU\Software, the only place a test may point the
/// uninstall entry at. `get`: an env value can cut a character in two.
fn under_software(key: &str) -> bool {
    key.len() > 9 && key.get(..9).is_some_and(|head| head.eq_ignore_ascii_case(r"Software\"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn places() -> Result<Places, String> {
    let dir = env_path("NEA_INSTALL_DIR")
        .or_else(|| env_path("LOCALAPPDATA").map(|p| p.join("Programs").join(storage::APP_NAME)))
        .ok_or("Cannot find %LOCALAPPDATA%.")?;
    let menu = env_path("NEA_START_MENU_DIR")
        .or_else(|| env_path("APPDATA").map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs")))
        .ok_or("Cannot find %APPDATA%.")?;
    let key = std::env::var("NEA_UNINSTALL_KEY")
        .ok()
        .filter(|k| under_software(k))
        .unwrap_or_else(|| UNINSTALL_KEY.to_string());
    Ok(Places { dir, shortcut: menu.join(format!("{}.lnk", storage::DISPLAY_NAME)), key })
}

fn message(text: &str, style: MESSAGEBOX_STYLE) -> bool {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(storage::DISPLAY_NAME), style) == IDYES }
}

fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn read_string(root: HKEY, subkey: &str, name: &str) -> Option<String> {
    let mut buffer = [0u16; 128];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(subkey),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// A "pv" that is empty or 0.0.0.0 is what an uninstalled runtime leaves.
fn installed_version(pv: Option<String>) -> Option<String> {
    pv.map(|v| v.trim().to_string()).filter(|v| !v.is_empty() && v != "0.0.0.0")
}

/// The WebView2 runtime's version, where Microsoft documents it (machine
/// wide, 64 and 32-bit views, then per user), or as the loader finds it.
pub fn webview2_version() -> Option<String> {
    let places = [
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}")),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}")),
        (HKEY_CURRENT_USER, format!(r"Software\{WEBVIEW2_CLIENT}")),
    ];
    if let Some(version) = places.iter().find_map(|(root, key)| installed_version(read_string(*root, key, "pv"))) {
        return Some(version);
    }
    unsafe {
        let mut version = PWSTR::null();
        GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version).ok()?;
        if version.is_null() {
            return None;
        }
        let text = version.to_string().ok();
        CoTaskMemFree(Some(version.0 as *const _));
        installed_version(text)
    }
}

enum Value {
    Text(String),
    Number(u32),
}

/// What Apps shows and runs for the entry.
fn uninstall_values(exe: &Path, dir: &Path, size_bytes: u64) -> Vec<(&'static str, Value)> {
    let exe = exe.display();
    vec![
        ("DisplayName", Value::Text(storage::DISPLAY_NAME.to_string())),
        ("DisplayVersion", Value::Text(updater::CURRENT_VERSION.to_string())),
        ("Publisher", Value::Text(storage::APP_NAME.to_string())),
        ("DisplayIcon", Value::Text(format!("\"{exe}\",0"))),
        ("InstallLocation", Value::Text(dir.display().to_string())),
        ("UninstallString", Value::Text(format!("\"{exe}\" --uninstall"))),
        ("QuietUninstallString", Value::Text(format!("\"{exe}\" --uninstall --quiet"))),
        ("NoModify", Value::Number(1)),
        ("NoRepair", Value::Number(1)),
        ("EstimatedSize", Value::Number(size_bytes.div_ceil(1024).min(u32::MAX as u64) as u32)),
    ]
}

fn write_uninstall_entry(key_path: &str, exe: &Path, dir: &Path) -> Result<(), String> {
    let size = fs::metadata(exe).map(|m| m.len()).unwrap_or(0);
    unsafe {
        let mut key = HKEY::default();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key_path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("Cannot create the entry in Apps ({}).", status.0));
        }
        let mut failed = None;
        for (name, value) in uninstall_values(exe, dir, size) {
            let status = match value {
                Value::Text(text) => {
                    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                    RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes))
                }
                Value::Number(n) => RegSetValueExW(key, &HSTRING::from(name), None, REG_DWORD, Some(&n.to_le_bytes())),
            };
            if status != ERROR_SUCCESS {
                failed = Some(name);
            }
        }
        let _ = RegCloseKey(key);
        match failed {
            Some(name) => Err(format!("Cannot write {name} in the Apps entry.")),
            None => Ok(()),
        }
    }
}

fn create_shortcut(path: &Path, target: &Path, dir: &Path, arguments: &str) -> windows::core::Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target))?;
        if !arguments.is_empty() {
            link.SetArguments(&HSTRING::from(arguments))?;
        }
        link.SetWorkingDirectory(&HSTRING::from(dir))?;
        link.SetDescription(&HSTRING::from("Web browser"))?;
        link.SetIconLocation(&HSTRING::from(target), 0)?;
        link.cast::<IPersistFile>()?.Save(&HSTRING::from(path), true)
    }
}

/// Where a profile's desktop shortcut goes. NEA_DESKTOP_DIR moves it for
/// tests, and a run on other data (NEA_DATA_DIR) keeps it in that folder,
/// so a test never writes to the person's desktop.
fn desktop_dir() -> Option<PathBuf> {
    if let Some(dir) = env_path("NEA_DESKTOP_DIR") {
        return Some(dir);
    }
    if let Some(data) = env_path("NEA_DATA_DIR") {
        return Some(data.join("Desktop"));
    }
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        text.map(PathBuf::from)
    }
}

/// A desktop shortcut named `file` that starts this executable with
/// `arguments` (a profile's). Returns where it went.
pub fn create_desktop_shortcut(file: &str, arguments: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the program: {e}"))?;
    let desktop = desktop_dir().ok_or("Cannot find the desktop.")?;
    fs::create_dir_all(&desktop).map_err(|e| format!("Cannot create {}: {e}", desktop.display()))?;
    let path = desktop.join(file);
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    create_shortcut(&path, &exe, &dir, arguments).map_err(|e| format!("Cannot create the shortcut: {e}"))?;
    Ok(path)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The copy, the license, the shortcut and the entry in Apps. Returns the
/// installed executable.
fn install_files(exe: &Path, places: &Places) -> Result<PathBuf, String> {
    fs::create_dir_all(&places.dir).map_err(|e| format!("Cannot create {}: {e}", places.dir.display()))?;
    let target = places.dir.join(EXE_NAME);
    // Installing over an open Nea works: the running file steps aside.
    if !same_file(exe, &target) {
        updater::replace_exe(&target, exe).map_err(|e| format!("Cannot copy the program: {e}"))?;
    }
    let _ = fs::write(places.dir.join("LICENSE.txt"), include_str!("../LICENSE"));
    if let Some(menu) = places.shortcut.parent() {
        let _ = fs::create_dir_all(menu);
    }
    create_shortcut(&places.shortcut, &target, &places.dir, "")
        .map_err(|e| format!("Cannot create the shortcut: {e}"))?;
    write_uninstall_entry(&places.key, &target, &places.dir)?;
    Ok(target)
}

fn install(exe: &Path, quiet: bool) -> Result<(), String> {
    let places = places()?;
    if webview2_version().is_none()
        && !quiet
        && message(
            "Nea uses the Microsoft WebView2 engine, which is not installed on this computer. \
             Without it, Nea cannot show pages.\n\nOpen the official WebView2 download?",
            MB_YESNO | MB_ICONWARNING,
        )
    {
        open_url(WEBVIEW2_BOOTSTRAPPER);
    }
    let target = install_files(exe, &places)?;
    if !quiet
        && message(
            "Nea is installed. You can find it in the Start menu.\n\nOpen Nea now?",
            MB_YESNO | MB_ICONINFORMATION,
        )
    {
        let _ = std::process::Command::new(&target).spawn();
    }
    Ok(())
}

/// Runs `command` in a hidden shell a couple of seconds from now, once this
/// process has exited.
fn run_after_exit(command: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let _ = std::process::Command::new(shell)
        .raw_arg(format!("/d /c ping -n 3 127.0.0.1 >nul & {command}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Removes the install folder. When the uninstaller is the installed copy it
/// can't delete itself, only move: it goes to the temp folder and a hidden
/// `del` collects it once it has exited (or, if it can't move, the whole
/// folder goes that way).
fn remove_install_dir(dir: &Path, exe: &Path) -> Result<(), String> {
    let inside = fs::canonicalize(dir).is_ok_and(|dir| fs::canonicalize(exe).is_ok_and(|e| e.starts_with(&dir)));
    if inside {
        let parked = std::env::temp_dir().join(format!("nea-uninstall-{}.exe", std::process::id()));
        if fs::rename(exe, &parked).is_err() {
            run_after_exit(&format!("rmdir /s /q \"{}\"", dir.display()));
            return Ok(());
        }
        run_after_exit(&format!("del /f /q \"{}\"", parked.display()));
    }
    fs::remove_dir_all(dir).map_err(|e| format!("Cannot delete {}: {e}", dir.display()))
}

fn uninstall(exe: &Path, quiet: bool) -> Result<(), String> {
    let places = places()?;
    let _ = fs::remove_file(&places.shortcut);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(places.key.as_str()));
    }
    if places.dir.exists() {
        remove_install_dir(&places.dir, exe)?;
    }
    // Downloaded updates are part of the install, not the user's data.
    let _ = fs::remove_dir_all(updater::update_dir());
    crate::notifications::unregister(crate::notifications::AUMID);
    if !quiet {
        let data = storage::data_dir();
        message(
            &format!(
                "Nea is uninstalled.\n\nYour bookmarks, history and settings stay in {}. Delete that folder if you do not want to keep them.",
                data.display()
            ),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    Ok(())
}

/// Where the installer put the browser under its former name. The legacy
/// variables are there for tests, like the current ones.
fn legacy_places(new: &Places) -> Option<Places> {
    let dir = env_path("LIVIANO_INSTALL_DIR")
        .or_else(|| env_path("LOCALAPPDATA").map(|p| p.join("Programs").join(legacy::NAME)))?;
    let shortcut = new.shortcut.with_file_name(format!("{}.lnk", legacy::NAME));
    let key = std::env::var("LIVIANO_UNINSTALL_KEY")
        .ok()
        .filter(|k| under_software(k))
        .unwrap_or_else(|| format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}", legacy::NAME));
    Some(Places { dir, shortcut, key })
}

/// Whether `exe` is the copy installed in `dir`.
fn runs_from(exe: &Path, dir: &Path) -> bool {
    match (exe.parent().and_then(|p| fs::canonicalize(p).ok()), fs::canonicalize(dir)) {
        (Some(parent), Ok(dir)) => parent == dir,
        _ => false,
    }
}

/// An install of the former name updated itself to this version and still
/// runs from its old folder under its old file name. It installs itself
/// under the new name (nea.exe, the Nea shortcut and entry in Apps), takes
/// the old shortcut and entry out, starts the new copy, and has the old
/// folder removed once this process has exited. `Some(0)` means this process
/// should exit now; any failure leaves it running from where it is, and the
/// next launch tries again.
pub fn move_legacy_install() -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let places = places().ok()?;
    let legacy = legacy_places(&places)?;
    if !runs_from(&exe, &legacy.dir) || runs_from(&exe, &places.dir) {
        return None;
    }
    let target = install_files(&exe, &places).ok()?;
    let _ = fs::remove_file(&legacy.shortcut);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(legacy.key.as_str()));
    }
    std::process::Command::new(&target).args(std::env::args_os().skip(1)).spawn().ok()?;
    run_after_exit(&format!("rmdir /s /q \"{}\"", legacy.dir.display()));
    Some(0)
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/installer.rs"]
#[rustfmt::skip]
mod tests;
