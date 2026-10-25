//! macOS backend for `rfb-server`, part one: everything that needs no Apple
//! API, so it is written and tested on any system.
//!
//! - [`keys`]: X11 keysyms (what RFB sends) to macOS virtual key codes, text
//!   for what no key code covers, and the modifier flags every event carries;
//! - [`mod@pointer`]: client pointer events (position, button mask) to mouse
//!   events, with drags, wheel steps and click counts;
//! - [`layout`]: several displays as one picture, and the mapping from
//!   picture pixels to global screen points;
//! - [`frame`]: padded BGRA frames to packed rows, dirty rectangles to RFB
//!   rectangles;
//! - [`cursor`]: premultiplied cursor bitmaps to the straight RGBA the
//!   server sends.
//!
//! # What is not here
//!
//! The calls into macOS: `ScreenCaptureKit` for the screen, `CGEventPost` for
//! input, `NSPasteboard` for the clipboard, `NSCursor` for the pointer shape.
//! They need a Mac to be tried (and the user's Screen Recording and
//! Accessibility approval), so they are not written yet. When they are, they
//! become `#[cfg(target_os = "macos")]` modules next to these, which supply
//! the numbers and decisions; the service/worker split is
//! `farpane-session`, as on Windows.

pub mod cursor;
pub mod frame;
pub mod keys;
pub mod layout;
pub mod pointer;
