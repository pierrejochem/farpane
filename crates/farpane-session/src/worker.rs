//! The session worker's loop: capture the screen it can see and send it to
//! the service, and carry out the input the service forwards.
//!
//! Platform independent: it runs over any [`Source`], [`Input`] and pair of
//! byte streams, so it is tested on Linux with the test pattern.

use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use rfb_codec::Framebuffer;
use rfb_proto::types::Rect;
use rfb_server::{Input, Source};

use crate::ipc::{self, Command, Frame};

/// Capture `source` about `fps` times a second and send what changed to
/// `to` until the service closes `from` or the pipe breaks.
///
/// # Errors
/// The source failing, or the pipe to the service breaking.
pub fn run(
    mut source: impl Source,
    input: impl Input,
    mut from: impl Read + Send + 'static,
    mut to: impl Write,
    fps: u32,
) -> io::Result<()> {
    let (w, h) = source.size();
    let mut fb = Framebuffer::new(w, h).map_err(io::Error::other)?;
    let input = Arc::new(input);
    let closed = Arc::new(AtomicBool::new(false));
    {
        let closed = Arc::clone(&closed);
        thread::Builder::new()
            .name("farpane-worker-input".into())
            .spawn(move || {
                while let Ok(Some(command)) = ipc::read_command(&mut from) {
                    match command {
                        Command::Key { keysym, down } => input.key(keysym, down),
                        Command::Pointer { x, y, buttons } => input.pointer(x, y, buttons),
                        Command::Clipboard(text) => input.clipboard(&text),
                    }
                }
                closed.store(true, Ordering::Relaxed);
            })?;
    }
    let interval = Duration::from_secs(1) / fps.clamp(1, 120);
    let mut first = true;
    let mut size = (0, 0);
    while !closed.load(Ordering::Relaxed) {
        let capture = source.capture(&mut fb)?;
        let now = (fb.width(), fb.height());
        let resized = first || now != size;
        let damage = if resized {
            vec![Rect {
                x: 0,
                y: 0,
                width: now.0,
                height: now.1,
            }]
        } else {
            capture.damage
        };
        if !damage.is_empty() || capture.cursor.is_some() || capture.clipboard.is_some() {
            let frame = Frame {
                width: now.0,
                height: now.1,
                rects: damage.iter().map(|r| (*r, rect_pixels(&fb, *r))).collect(),
                cursor: capture.cursor,
                clipboard: capture.clipboard,
            };
            ipc::send(&mut to, &frame.encode()?)?;
        }
        first = false;
        size = now;
        thread::sleep(interval);
    }
    Ok(())
}

/// The `B, G, R, X` rows of `r`, without padding between rows.
fn rect_pixels(fb: &Framebuffer, r: Rect) -> Vec<u8> {
    let row = usize::from(r.width) * 4;
    let mut out = Vec::with_capacity(row * usize::from(r.height));
    for y in usize::from(r.y)..usize::from(r.y) + usize::from(r.height) {
        let at = y * fb.stride() + usize::from(r.x) * 4;
        out.extend_from_slice(&fb.pixels()[at..at + row]);
    }
    out
}
