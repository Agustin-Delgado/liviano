//! Just enough SQLite to read a Chromium `Login Data` file, through the
//! copy Windows ships (System32\winsqlite3.dll). It is loaded only while an
//! import runs and released afterwards, so the app neither carries SQLite
//! nor keeps it in memory.

use std::ffi::{CString, c_char, c_int, c_void};

use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
    },
    core::{PCSTR, w},
};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_OPEN_READONLY: c_int = 0x1;
#[cfg(all(test, nea_tests))]
const SQLITE_OPEN_READWRITE_CREATE: c_int = 0x2 | 0x4;

type Open = unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type Close = unsafe extern "system" fn(*mut c_void) -> c_int;
type Prepare =
    unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int;
type Step = unsafe extern "system" fn(*mut c_void) -> c_int;
type Finalize = unsafe extern "system" fn(*mut c_void) -> c_int;
type ColumnBlob = unsafe extern "system" fn(*mut c_void, c_int) -> *const c_void;
type ColumnBytes = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
type ColumnInt = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
#[cfg(all(test, nea_tests))]
type Exec =
    unsafe extern "system" fn(*mut c_void, *const c_char, *const c_void, *mut c_void, *mut *mut c_char) -> c_int;

/// The library, loaded. Dropping it unloads it.
pub struct Sqlite {
    module: HMODULE,
    open: Open,
    close: Close,
    prepare: Prepare,
    step: Step,
    finalize: Finalize,
    column_blob: ColumnBlob,
    column_bytes: ColumnBytes,
    column_int: ColumnInt,
    #[cfg(all(test, nea_tests))]
    exec: Exec,
}

impl Sqlite {
    #[allow(clippy::missing_transmute_annotations, reason = "each target is the type of the field the macro fills")]
    pub fn load() -> Option<Self> {
        unsafe {
            let module = LoadLibraryExW(w!("winsqlite3.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            macro_rules! function {
                ($name:literal) => {
                    match GetProcAddress(module, PCSTR(concat!($name, "\0").as_ptr())) {
                        Some(f) => std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(f),
                        None => {
                            let _ = FreeLibrary(module);
                            return None;
                        }
                    }
                };
            }
            Some(Self {
                module,
                open: function!("sqlite3_open_v2"),
                close: function!("sqlite3_close"),
                prepare: function!("sqlite3_prepare_v2"),
                step: function!("sqlite3_step"),
                finalize: function!("sqlite3_finalize"),
                column_blob: function!("sqlite3_column_blob"),
                column_bytes: function!("sqlite3_column_bytes"),
                column_int: function!("sqlite3_column_int"),
                #[cfg(all(test, nea_tests))]
                exec: function!("sqlite3_exec"),
            })
        }
    }

    fn open_with(&self, path: &std::path::Path, flags: c_int) -> Option<Db<'_>> {
        let path = CString::new(path.to_str()?).ok()?;
        let mut handle = std::ptr::null_mut();
        let status = unsafe { (self.open)(path.as_ptr(), &mut handle, flags, std::ptr::null()) };
        let db = Db { lib: self, handle };
        (status == SQLITE_OK).then_some(db)
    }

    pub fn open_read_only(&self, path: &std::path::Path) -> Option<Db<'_>> {
        self.open_with(path, SQLITE_OPEN_READONLY)
    }

    #[cfg(all(test, nea_tests))]
    pub fn create(&self, path: &std::path::Path) -> Option<Db<'_>> {
        self.open_with(path, SQLITE_OPEN_READWRITE_CREATE)
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}

pub struct Db<'a> {
    lib: &'a Sqlite,
    handle: *mut c_void,
}

/// One row of a query, read by column index.
pub struct Row<'a> {
    lib: &'a Sqlite,
    statement: *mut c_void,
}

impl Row<'_> {
    pub fn blob(&self, column: usize) -> Vec<u8> {
        unsafe {
            let data = (self.lib.column_blob)(self.statement, column as c_int);
            let len = (self.lib.column_bytes)(self.statement, column as c_int).max(0) as usize;
            if data.is_null() || len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data as *const u8, len).to_vec()
            }
        }
    }

    pub fn text(&self, column: usize) -> String {
        String::from_utf8_lossy(&self.blob(column)).into_owned()
    }

    pub fn int(&self, column: usize) -> i32 {
        unsafe { (self.lib.column_int)(self.statement, column as c_int) }
    }
}

impl Db<'_> {
    /// Runs a query and hands every row to `each`. None when it doesn't
    /// compile (a column this version of the file lacks).
    pub fn query(&self, sql: &str, mut each: impl FnMut(&Row)) -> Option<()> {
        let sql = CString::new(sql).ok()?;
        let mut statement = std::ptr::null_mut();
        unsafe {
            if (self.lib.prepare)(self.handle, sql.as_ptr(), -1, &mut statement, std::ptr::null_mut()) != SQLITE_OK {
                (self.lib.finalize)(statement);
                return None;
            }
            let row = Row { lib: self.lib, statement };
            while (self.lib.step)(statement) == SQLITE_ROW {
                each(&row);
            }
            (self.lib.finalize)(statement);
        }
        Some(())
    }

    #[cfg(all(test, nea_tests))]
    pub fn execute(&self, sql: &str) -> bool {
        let sql = CString::new(sql).unwrap();
        unsafe {
            (self.lib.exec)(self.handle, sql.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut())
                == SQLITE_OK
        }
    }
}

impl Drop for Db<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.lib.close)(self.handle);
        }
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/sqlite.rs"]
#[rustfmt::skip]
mod tests;
