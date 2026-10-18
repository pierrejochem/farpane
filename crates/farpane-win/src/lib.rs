//! Windows backend for `rfb-server`: the screen through GDI (`BitBlt` of the
//! whole virtual desktop, so several monitors are one wide picture), change
//! tracking by comparing 64 x 64 tiles with the previous picture, the
//! pointer's shape, the text clipboard, and keyboard and pointer input
//! through `SendInput`.
//!
//! It works for a process that runs on the user's interactive desktop. A
//! Windows service runs in session 0, which has no desktop, so the service
//! starts a session worker in the console session (`session`, over the
//! pipes of `ipc`); the worker runs this backend through `worker::run`, and
//! the service sees it as `remote::RemoteSource` and `remote::RemoteInput`.
//! A worker with SYSTEM rights follows the input desktop (`desktop`), which
//! is how the lock screen and UAC prompts come into view.
//!
//! Limits: an ordinary user's process cannot capture the secure desktop (UAC
//! prompt, lock screen), and nothing here can send Windows' own "secure
//! attention" key combination (Ctrl+Alt+Del). GDI is slower than
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
//! may be used from any one thread at a time). `session` also owns every
//! kernel handle it opens (tokens, pipe ends, the process) in a `Handle`
//! that closes it, hands pipe ends to `File` exactly once, and gives the child
//! only its own two pipe ends.

pub mod ipc;
pub mod keys;
pub mod remote;
pub mod worker;

#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod input;
#[cfg(windows)]
pub mod session;
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
