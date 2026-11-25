//! The profile this process runs, and the windows of the others: the start
//! that points the folders and the passwords at a profile, the tag on the
//! main window that another Nea finds it by, and opening a profile (its
//! window comes to the front, or a new process starts it).

use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};

use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, EnumWindows, GetClassNameW, GetPropW, GetWindowThreadProcessId, IsIconic,
            PostMessageW, RegisterWindowMessageW, SW_RESTORE, SetForegroundWindow, SetPropW, ShowWindow,
        },
    },
    core::{BOOL, HSTRING},
};

use crate::{
    profiles::{self, Registry},
    storage, vault,
};

static CURRENT: AtomicU32 = AtomicU32::new(profiles::DEFAULT_ID);

/// The profile of this process.
pub fn current() -> u32 {
    CURRENT.load(Ordering::Relaxed)
}

/// Reads the registry from the default roaming folder.
pub fn load() -> Registry {
    storage::read_json(&profiles::registry_path(&storage::default_roaming_root()))
}

pub fn save(registry: &Registry) -> std::io::Result<()> {
    let root = storage::default_roaming_root();
    std::fs::create_dir_all(&root)?;
    storage::write_json(&profiles::registry_path(&root), registry)
}

/// At start, after the former name's folders moved and before any file is
/// read: the profile the command line names gets its own folders and its
/// own corner of Credential Manager. Profile 0 changes nothing.
pub fn apply(asked: u32) {
    if asked == profiles::DEFAULT_ID {
        return;
    }
    let roaming_root = storage::default_roaming_root();
    let local_root = storage::default_local_root();
    let (Some(roaming), Some(local)) =
        (profiles::profile_dir(&roaming_root, asked), profiles::profile_dir(&local_root, asked))
    else {
        return;
    };
    let id = profiles::resolve(asked, load().has(asked), roaming.exists());
    if id == profiles::DEFAULT_ID {
        return;
    }
    storage::use_profile(roaming, local);
    vault::use_prefix(profiles::credential_prefix(&vault::base_prefix(), id));
    CURRENT.store(id, Ordering::Relaxed);
}

/// The window property's name for this data, as a wide string.
fn property() -> &'static HSTRING {
    static NAME: OnceLock<HSTRING> = OnceLock::new();
    NAME.get_or_init(|| HSTRING::from(profiles::window_property(&storage::default_roaming_root())))
}

/// The message that asks a main window to come to the front. Registered,
/// so it means the same in every process.
pub fn activate_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(&HSTRING::from("NeaProfileActivate")) })
}

/// Tags the main window with this process's profile, and notes that the
/// profile's window opened.
pub fn tag_window(hwnd: HWND) {
    unsafe {
        let _ = SetPropW(hwnd, property(), Some(HANDLE(profiles::property_value(current()) as _)));
    }
    let mut registry = load();
    // With profile 0 alone there is no registry: none is made just for this.
    let file = profiles::registry_path(&storage::default_roaming_root());
    if registry.last != current() && (file.exists() || current() != profiles::DEFAULT_ID) {
        registry.last = current();
        let _ = save(&registry);
    }
}

/// The main window that runs `id` on this data, if one is open.
pub fn find_window(id: u32) -> Option<HWND> {
    struct Search {
        id: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        if String::from_utf16_lossy(&class[..len]) != "NeaWindow" {
            return BOOL(1);
        }
        let value = unsafe { GetPropW(hwnd, property()) };
        if profiles::property_id(value.0 as usize) == Some(search.id) {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { id, found: None };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

/// The window asked by `activate_message` comes back and to the front. The
/// process that asked gave this one the right to take the foreground.
pub fn come_forward(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Opens profile `id`: its window comes to the front if it is open, or a
/// new process starts it, with this one's environment (a test run on other
/// data stays on it).
pub fn open(id: u32) -> Result<(), String> {
    if let Some(hwnd) = find_window(id) {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != 0 {
                let _ = AllowSetForegroundWindow(pid);
            }
            let _ = PostMessageW(Some(hwnd), activate_message(), WPARAM(0), LPARAM(0));
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find the program: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command.arg(profiles::ARG).arg(id.to_string());
    // Not a restart into an update: the new process must not wait for this one.
    command.env_remove(crate::updater::WAIT_VAR);
    // A scripted run's pipe belongs to this process: the new one gets its own.
    if let Ok(pipe) = std::env::var("NEA_BENCH_PIPE") {
        command.env("NEA_BENCH_PIPE", format!("{pipe}-p{id}"));
    }
    command.spawn().map(|_| ()).map_err(|e| format!("Cannot start the profile: {e}"))
}

/// Adds a profile named `name` (already cleaned), with a desktop shortcut
/// when asked, and opens its window. Returns its id, and what went wrong
/// with the shortcut if something did (the profile is made anyway).
pub fn create(name: &str, shortcut: bool) -> Result<(u32, Option<String>), String> {
    let mut registry = load();
    let id = registry.add(name);
    save(&registry).map_err(|e| format!("Cannot save the profile: {e}"))?;
    let shortcut_error = if shortcut { desktop_shortcut(id, name).err() } else { None };
    open(id)?;
    Ok((id, shortcut_error))
}

/// "Nea (Sofía)" on the desktop, which opens that profile.
fn desktop_shortcut(id: u32, name: &str) -> Result<PathBuf, String> {
    let file = profiles::shortcut_file(storage::APP_NAME, name);
    crate::installer::create_desktop_shortcut(&file, &format!("{} {id}", profiles::ARG))
}

/// Renames a profile in the registry.
pub fn rename(id: u32, name: &str) -> Result<(), String> {
    let mut registry = load();
    if !registry.rename(id, name) {
        return Err("That profile is not in the list.".to_string());
    }
    save(&registry).map_err(|e| format!("Cannot save the name: {e}"))
}
