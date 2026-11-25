//! Proving that the person at the computer is the Windows user: Windows
//! Hello (`UserConsentVerifier`), and the Windows account password when
//! Hello is not set up. Nothing here waits: each step posts `WM_APP_LOCK`
//! to the window when it ends, with the step and its answer, so the window
//! keeps drawing while the prompt is up.
//!
//! The Hello prompt belongs to Nea's window
//! (`IUserConsentVerifierInterop::RequestVerificationForWindowAsync`), so
//! it opens over it and not somewhere behind. The password prompt runs on a
//! thread of its own, with the window as its owner.

use windows::{
    Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier},
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM},
        Security::{
            Credentials::{
                CRED_PACK_PROTECTED_CREDENTIALS, CREDUI_INFOW, CREDUIWIN_ENUMERATE_CURRENT_USER,
                CredUIPromptForWindowsCredentialsW, CredUnPackAuthenticationBufferW,
            },
            EqualSid, GetTokenInformation, LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT, LogonUserW,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Threading::{GetCurrentProcess, OpenProcessToken},
            WinRT::IUserConsentVerifierInterop,
        },
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
    core::{HSTRING, PCWSTR, PWSTR},
};
use windows_future::IAsyncOperation;

use crate::profile_lock::{self, ERROR};

/// Posted to the window when a step ends: the step in `wParam`, and in
/// `lParam` the request's number and the answer (`pack`).
pub const WM_APP_LOCK: u32 = WM_APP + 0x58;

/// The steps, as `wParam` of `WM_APP_LOCK`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Whether Hello is set up, for a verification on its way.
    Availability = 1,
    /// What the Hello prompt answered.
    Hello = 2,
    /// What the password prompt answered (`PASSWORD_*`).
    Password = 3,
    /// Whether Hello is set up, for Settings.
    SettingsAvailability = 4,
}

impl Step {
    pub fn from_wparam(value: usize) -> Option<Step> {
        [Step::Availability, Step::Hello, Step::Password, Step::SettingsAvailability]
            .into_iter()
            .find(|s| *s as usize == value)
    }
}

pub const PASSWORD_VERIFIED: u8 = 0;
pub const PASSWORD_CANCELED: u8 = 1;
/// A password that does not open this Windows account, past the last try.
pub const PASSWORD_WRONG: u8 = 2;

/// How many times the password prompt comes back after a wrong password.
const PASSWORD_TRIES: usize = 5;
/// Windows' "the user name or password is incorrect", shown in the prompt.
const ERROR_LOGON_FAILURE: u32 = 1326;
const ERROR_CANCELLED: u32 = 1223;

/// The request's number and an answer in one `lParam`.
pub fn pack(request: u64, code: u8) -> isize {
    ((request << 8) | u64::from(code)) as isize
}

pub fn unpack(lparam: isize) -> (u64, u8) {
    let value = lparam as u64;
    (value >> 8, (value & 0xFF) as u8)
}

fn post(hwnd: isize, step: Step, request: u64, code: u8) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd as *mut core::ffi::c_void)),
            WM_APP_LOCK,
            WPARAM(step as usize),
            LPARAM(pack(request, code)),
        );
    }
}

/// Asks whether Hello is set up. The answer comes as `step`, with the
/// `UserConsentVerifierAvailability` value (or `ERROR`).
pub fn check(hwnd: HWND, step: Step, request: u64) {
    let window = hwnd.0 as isize;
    let started = UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|op| op.when(move |answer| post(window, step, request, answer.map_or(ERROR, |a| a.0 as u8))));
    if started.is_err() {
        post(window, step, request, ERROR);
    }
}

/// Shows the Hello prompt over `hwnd` with `message`. The answer comes as
/// `Step::Hello`, with the `UserConsentVerificationResult` value.
pub fn verify(hwnd: HWND, message: &str, request: u64) {
    let window = hwnd.0 as isize;
    let started = (|| -> windows::core::Result<()> {
        let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
        let op: IAsyncOperation<UserConsentVerificationResult> =
            unsafe { interop.RequestVerificationForWindowAsync(hwnd, &HSTRING::from(message))? };
        op.when(move |answer| post(window, Step::Hello, request, answer.map_or(ERROR, |r| r.0 as u8)))
    })();
    if started.is_err() {
        post(window, Step::Hello, request, ERROR);
    }
}

/// Asks for the Windows password of the current user, on a thread of its
/// own. The answer comes as `Step::Password` (`PASSWORD_*` or `ERROR`).
pub fn password(hwnd: HWND, caption: String, message: String, request: u64) {
    let window = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("nea-password".to_string()).spawn(move || {
        let code = password_prompt(HWND(window as *mut core::ffi::c_void), &caption, &message);
        post(window, Step::Password, request, code);
    });
    if spawned.is_err() {
        post(window, Step::Password, request, ERROR);
    }
}

fn password_prompt(owner: HWND, caption: &str, message: &str) -> u8 {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let caption = HSTRING::from(caption);
    let message = HSTRING::from(message);
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: owner,
        pszMessageText: PCWSTR(message.as_ptr()),
        pszCaptionText: PCWSTR(caption.as_ptr()),
        ..Default::default()
    };
    let mut answer = PASSWORD_WRONG;
    let mut last_error = 0;
    for _ in 0..PASSWORD_TRIES {
        match one_try(&info, last_error) {
            Some(true) => {
                answer = PASSWORD_VERIFIED;
                break;
            }
            Some(false) => last_error = ERROR_LOGON_FAILURE,
            None => {
                answer = PASSWORD_CANCELED;
                break;
            }
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
    answer
}

/// One prompt: Some(true) for the current user's password, Some(false)
/// for a wrong one, None when the person closed the prompt (or it failed).
fn one_try(info: &CREDUI_INFOW, last_error: u32) -> Option<bool> {
    let mut package = 0u32;
    let mut out: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut out_size = 0u32;
    let status = unsafe {
        CredUIPromptForWindowsCredentialsW(
            Some(info),
            last_error,
            &mut package,
            None,
            0,
            &mut out,
            &mut out_size,
            None,
            CREDUIWIN_ENUMERATE_CURRENT_USER,
        )
    };
    if status == ERROR_CANCELLED || status != 0 || out.is_null() {
        return None;
    }
    let mut user = [0u16; 514];
    let mut domain = [0u16; 338];
    let mut secret = [0u16; 514];
    let (mut user_len, mut domain_len, mut secret_len) = (user.len() as u32, domain.len() as u32, secret.len() as u32);
    let unpacked = unsafe {
        CredUnPackAuthenticationBufferW(
            CRED_PACK_PROTECTED_CREDENTIALS,
            out,
            out_size,
            Some(PWSTR(user.as_mut_ptr())),
            &mut user_len,
            Some(PWSTR(domain.as_mut_ptr())),
            Some(&mut domain_len),
            Some(PWSTR(secret.as_mut_ptr())),
            &mut secret_len,
        )
    };
    // The packed buffer holds the password too: wiped before it is freed.
    unsafe {
        wipe_bytes(out.cast::<u8>(), out_size as usize);
        CoTaskMemFree(Some(out));
    }
    let verified = unpacked.is_ok() && {
        let text = |buffer: &[u16]| {
            let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..end])
        };
        let (name, domain) = profile_lock::split_account(&text(&user), &text(&domain));
        is_current_user(&name, domain.as_deref(), &secret)
    };
    wipe(&mut secret);
    Some(verified)
}

/// Whether `name` with `secret` signs in to Windows as the user that runs
/// this process. Another account's password is not enough, even a right one.
fn is_current_user(name: &str, domain: Option<&str>, secret: &[u16]) -> bool {
    let name = HSTRING::from(name);
    let domain = domain.map(HSTRING::from);
    let mut token = HANDLE::default();
    let signed_in = unsafe {
        LogonUserW(
            PCWSTR(name.as_ptr()),
            domain.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            PCWSTR(secret.as_ptr()),
            LOGON32_LOGON_INTERACTIVE,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    };
    if signed_in.is_err() {
        return false;
    }
    let mut own = HANDLE::default();
    let same = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut own) }.is_ok()
        && match (token_user(token), token_user(own)) {
            (Some(a), Some(b)) => unsafe { sid_of(&a).zip(sid_of(&b)).is_some_and(|(a, b)| EqualSid(a, b).is_ok()) },
            _ => false,
        };
    unsafe {
        let _ = CloseHandle(token);
        if !own.is_invalid() {
            let _ = CloseHandle(own);
        }
    }
    same
}

/// The `TOKEN_USER` of a token, in a buffer of its size (it holds the SID).
fn token_user(token: HANDLE) -> Option<Vec<u64>> {
    let mut size = 0u32;
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
    }
    if size == 0 {
        return None;
    }
    // u64 for the alignment of the pointer inside.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    unsafe { GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr().cast()), size, &mut size) }
        .ok()
        .map(|()| buffer)
}

/// The SID inside a `TOKEN_USER` buffer from `token_user`.
unsafe fn sid_of(buffer: &[u64]) -> Option<windows::Win32::Security::PSID> {
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    (!user.User.Sid.is_invalid()).then_some(user.User.Sid)
}

/// Writes zeros that the compiler cannot leave out.
fn wipe(buffer: &mut [u16]) {
    for c in buffer.iter_mut() {
        unsafe { std::ptr::write_volatile(c, 0) };
    }
}

unsafe fn wipe_bytes(start: *mut u8, len: usize) {
    for i in 0..len {
        unsafe { std::ptr::write_volatile(start.add(i), 0) };
    }
}

#[cfg(all(test, nea_tests))]
#[path = "../tests/unit/hello.rs"]
#[rustfmt::skip]
mod tests;
