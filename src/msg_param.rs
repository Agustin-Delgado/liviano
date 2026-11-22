//! The structures that some window messages point to with `lParam`
//! (`WM_DRAWITEM`, `WM_NOTIFY`, `WM_NCCALCSIZE`, `WM_DPICHANGED`…).
//!
//! The system sends these with a valid pointer. Any program on the desktop
//! can also post or send them to our windows, with any value: 0, a pointer
//! into its own process (Windows does not marshal `WM_NOTIFY`), or random
//! bits. Read as a reference, such a value is an access violation inside a
//! window procedure, and the process ends. So each read goes through here:
//! the memory must be committed and readable (or writable) for the whole
//! structure, and aligned for it. The handlers then check that the control
//! the structure names is one of ours (`from_child`).

use windows::Win32::{
    Foundation::{HWND, LPARAM},
    System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY,
        PAGE_GUARD, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY, VirtualQuery,
    },
    UI::WindowsAndMessaging::IsChild,
};

/// Whether the protection lets the process read (and, with `write`, write).
fn allows(protect: PAGE_PROTECTION_FLAGS, write: bool) -> bool {
    if protect.0 & (PAGE_GUARD.0 | PAGE_NOACCESS.0) != 0 {
        return false;
    }
    let writable = PAGE_READWRITE.0 | PAGE_WRITECOPY.0 | PAGE_EXECUTE_READWRITE.0 | PAGE_EXECUTE_WRITECOPY.0;
    let readable = writable | PAGE_READONLY.0 | PAGE_EXECUTE_READ.0;
    protect.0 & if write { writable } else { readable } != 0
}

/// Whether `len` bytes at `address` are committed memory that the process
/// can read (and, with `write`, write). A range can cross regions: each
/// region in it is checked.
fn accessible(address: usize, len: usize, write: bool) -> bool {
    let Some(end) = address.checked_add(len) else { return false };
    if address == 0 {
        return false;
    }
    let mut at = address;
    while at < end {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
        if unsafe { VirtualQuery(Some(at as *const _), &mut info, size) } < size {
            return false;
        }
        if info.State != MEM_COMMIT || !allows(info.Protect, write) {
            return false;
        }
        let region_end = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if region_end <= at {
            return false;
        }
        at = region_end;
    }
    true
}

fn usable<T>(lparam: LPARAM, write: bool) -> Option<*mut T> {
    let address = lparam.0 as usize;
    (address.is_multiple_of(std::mem::align_of::<T>()) && accessible(address, std::mem::size_of::<T>(), write))
        .then_some(address as *mut T)
}

/// The `T` that `lparam` points to, if the memory is there to read.
///
/// # Safety
/// `T` must be a plain structure for which any bit pattern is valid (the
/// Win32 structures are). The reference must not outlive the message.
pub unsafe fn read<'a, T>(lparam: LPARAM) -> Option<&'a T> {
    usable::<T>(lparam, false).map(|p| unsafe { &*p })
}

/// The `T` that `lparam` points to, if the memory is there to change.
///
/// # Safety
/// As `read`.
pub unsafe fn write<'a, T>(lparam: LPARAM) -> Option<&'a mut T> {
    usable::<T>(lparam, true).map(|p| unsafe { &mut *p })
}

/// Whether `control` is a window inside `parent`: a structure that names a
/// control of another window (or of another program) is not ours to draw.
pub fn from_child(parent: HWND, control: HWND) -> bool {
    !control.is_invalid() && unsafe { IsChild(parent, control) }.as_bool()
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/msg_param.rs"]
#[rustfmt::skip]
mod tests;
