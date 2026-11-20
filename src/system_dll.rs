//! System libraries loaded only while a rare feature runs (importing
//! passwords, confirming the Windows user): linking them would map them into
//! the process for its whole life. Only System32 is searched.

use std::ffi::CStr;

use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    },
    core::{PCSTR, PCWSTR},
};

/// A loaded library; dropping it unloads it.
pub struct Library(HMODULE);

impl Library {
    pub fn load(name: PCWSTR) -> Option<Self> {
        unsafe { LoadLibraryExW(name, None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok().map(Self) }
    }

    /// The function `name`, typed as `F`.
    ///
    /// # Safety
    /// `F` must be an `unsafe extern "system" fn` type matching the export,
    /// and it must not be called after the library is dropped.
    pub unsafe fn function<F: Copy>(&self, name: &CStr) -> Option<F> {
        const { assert!(std::mem::size_of::<F>() == std::mem::size_of::<usize>()) };
        let address = unsafe { GetProcAddress(self.0, PCSTR(name.as_ptr() as *const u8)) }?;
        Some(unsafe { std::mem::transmute_copy(&address) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}
