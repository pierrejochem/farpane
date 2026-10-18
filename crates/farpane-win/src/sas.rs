//! The secure attention sequence (Ctrl+Alt+Delete).
//!
//! Windows reserves that combination: `SendInput` cannot produce it. A
//! service may ask for it with `SendSAS` from `sas.dll`, provided the
//! software-SAS policy allows services (`SoftwareSASGeneration`, set by the
//! installer and by `farpane-server service install`).

use std::io;

use windows::{
    Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    core::{PCSTR, w},
};

/// Ask Windows to show the Ctrl+Alt+Delete screen.
///
/// # Errors
/// `sas.dll` is missing. Windows itself ignores the request when the policy
/// does not allow services to send it (nothing is reported then).
pub fn send() -> io::Result<()> {
    // SAFETY: loads a system library by name; the handle is kept for the process lifetime.
    let library = unsafe { LoadLibraryW(w!("sas.dll")) }.map_err(io::Error::other)?;
    // SAFETY: the name is a NUL-terminated string literal.
    let Some(function) = (unsafe { GetProcAddress(library, PCSTR(c"SendSAS".as_ptr().cast())) })
    else {
        return Err(io::Error::other("sas.dll has no SendSAS"));
    };
    // SAFETY: `SendSAS` is `void SendSAS(BOOL AsUser)`; 0 (FALSE) is the service form.
    let call: unsafe extern "system" fn(i32) = unsafe { std::mem::transmute(function) };
    // SAFETY: the signature above matches the export.
    unsafe { call(0) };
    Ok(())
}
