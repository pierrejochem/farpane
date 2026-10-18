//! Windows backend for `rfb-server`: the screen through GDI (`BitBlt` of the
//! whole virtual desktop, so several monitors are one wide picture), change
//! tracking by comparing 64 x 64 tiles with the previous picture, the
//! pointer's shape, the text clipboard, and keyboard and pointer input
//! through `SendInput`.
//!
//! It works for a process that runs on the user's interactive desktop. A
//! Windows service runs in session 0, which has no desktop; sharing a user's
//! screen from the service needs the session worker (design doc, section 19),
//! which is not built.
//!
//! Limits: the secure desktop (UAC prompt, lock screen, Ctrl+Alt+Del screen)
//! is not captured and `SendInput` cannot type into it, so Windows' own
//! "secure attention" key combination cannot be sent either; while the secure
//! desktop is up the picture simply stops changing. GDI is slower than
//! DXGI Desktop Duplication, which would also give the changed rectangles
//! for free; it is the planned replacement for large screens.
//!
//! # Unsafe code
//!
//! The Win32 API is `unsafe` to call. The invariants this crate relies on:
//! every handle it creates (device contexts, bitmaps) is owned by one struct
//! that releases it in `Drop`; buffers handed to the API are sized from the
//! same dimensions the API is told; and the handle-owning structs are moved
//! to the capture thread once and used from there only (a GDI device context
//! may be used from any one thread at a time).

pub mod keys;

#[cfg(windows)]
mod input;
#[cfg(windows)]
mod source;

#[cfg(windows)]
pub use input::WindowsInput;
#[cfg(windows)]
pub use source::WindowsSource;

/// Open the Windows desktop for sharing: a source and an input.
///
/// # Errors
/// The screen cannot be read (no interactive desktop, for example in a
/// service) or is too large for RFB.
#[cfg(windows)]
pub fn open() -> std::io::Result<(WindowsSource, WindowsInput)> {
    let clipboard = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    Ok((
        WindowsSource::new(clipboard.clone())?,
        WindowsInput::new(clipboard),
    ))
}
