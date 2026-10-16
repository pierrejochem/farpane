//! What the server needs from the platform: a screen to capture and a way to
//! inject input. Real backends (X11, Wayland portal, macOS, Windows) live in
//! their own crates; [`TestPattern`] is here for tests and demos.

use std::io;

use rfb_codec::{Cursor, Framebuffer};
use rfb_proto::types::Rect;

/// What one call to [`Source::capture`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capture {
    /// The parts of the picture that changed since the previous call.
    /// Empty means nothing changed.
    pub damage: Vec<Rect>,
    /// The pointer's shape, if it changed since the previous call. The
    /// first call should report it. A cursor with no pixels hides it.
    pub cursor: Option<Cursor>,
    /// New text on the machine's clipboard since the previous call (not text
    /// a client just put there). Sent to every client.
    pub clipboard: Option<String>,
}

/// A screen that can be captured. Called from one dedicated thread, a few
/// tens of times a second.
pub trait Source: Send + 'static {
    /// The screen size at start; the framebuffer is created with it.
    fn size(&self) -> (u16, u16);

    /// Bring `fb` up to date and report what changed. A source whose screen
    /// changes size calls [`Framebuffer::resize`] itself (which clears the
    /// picture) and reports the whole screen as damaged.
    ///
    /// # Errors
    /// Any error ends capturing for good; clients are disconnected.
    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture>;
}

/// Where keyboard, pointer and clipboard input from clients goes. Called
/// from client tasks; implementations serialise as they need to.
pub trait Input: std::fmt::Debug + Send + Sync + 'static {
    /// A key went down or up (an X11 keysym).
    fn key(&self, keysym: u32, down: bool);
    /// The pointer moved to (`x`, `y`) with `buttons` held (bit 0 left, 1
    /// middle, 2 right, 3 and 4 the wheel, as in `PointerEvent`).
    fn pointer(&self, x: u16, y: u16, buttons: u8);
    /// A client copied `text` (already UTF-8).
    fn clipboard(&self, text: &str);
}

/// Input that goes nowhere.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoInput;

impl Input for NoInput {
    fn key(&self, _: u32, _: bool) {}
    fn pointer(&self, _: u16, _: u16, _: u8) {}
    fn clipboard(&self, _: &str) {}
}

/// An animated test picture: a gradient with a square that moves each frame
/// (so there is always a small damaged area) and a number that counts frames.
#[derive(Debug)]
pub struct TestPattern {
    width: u16,
    height: u16,
    frame: u32,
}

impl TestPattern {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            frame: 0,
        }
    }

    /// Where the square is after `frame` frames.
    #[must_use]
    pub fn square(&self, frame: u32) -> Rect {
        let side = (self.width.min(self.height) / 4).max(1);
        let span_x = u32::from(self.width - side).max(1);
        let span_y = u32::from(self.height - side).max(1);
        Rect {
            x: u16::try_from(frame * 7 % span_x).unwrap_or(0),
            y: u16::try_from(frame * 3 % span_y).unwrap_or(0),
            width: side,
            height: side,
        }
    }

    /// The background colour at (`x`, `y`) as `B, G, R, X`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // a gradient: wrapping is the point
    pub fn background(x: u16, y: u16) -> [u8; 4] {
        [(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8, 0]
    }

    /// The test cursor: a 12 x 12 white arrow-ish triangle with a clear
    /// background, hotspot at its tip.
    #[must_use]
    pub fn arrow() -> Cursor {
        let mut rgba = Vec::with_capacity(12 * 12 * 4);
        for y in 0..12u8 {
            for x in 0..12u8 {
                let inside = x <= y && y < 12 && x < 8;
                rgba.extend_from_slice(&if inside {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 0]
                });
            }
        }
        Cursor {
            hotspot: (0, 0),
            width: 12,
            height: 12,
            rgba,
        }
    }

    fn paint(fb: &mut Framebuffer, rect: Rect, f: impl Fn(u16, u16) -> [u8; 4]) {
        let mut data = Vec::with_capacity(usize::from(rect.width) * usize::from(rect.height) * 4);
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                data.extend_from_slice(&f(x, y));
            }
        }
        // The rectangle was derived from the picture's own size.
        let _ = fb.write_bgrx(rect, &data);
    }
}

impl Source for TestPattern {
    fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        let whole = Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        };
        if self.frame == 0 {
            Self::paint(fb, whole, Self::background);
            self.frame = 1;
            return Ok(Capture {
                damage: vec![whole],
                cursor: Some(Self::arrow()),
                clipboard: None,
            });
        }
        let old = self.square(self.frame - 1);
        let new = self.square(self.frame);
        self.frame += 1;
        Self::paint(fb, old, Self::background);
        Self::paint(fb, new, |_, _| [255, 255, 255, 0]);
        Ok(Capture {
            damage: vec![old, new],
            cursor: None,
            clipboard: None,
        })
    }
}

impl Source for Box<dyn Source> {
    fn size(&self) -> (u16, u16) {
        (**self).size()
    }

    fn capture(&mut self, fb: &mut Framebuffer) -> io::Result<Capture> {
        (**self).capture(fb)
    }
}

impl Input for Box<dyn Input> {
    fn key(&self, keysym: u32, down: bool) {
        (**self).key(keysym, down);
    }

    fn pointer(&self, x: u16, y: u16, buttons: u8) {
        (**self).pointer(x, y, buttons);
    }

    fn clipboard(&self, text: &str) {
        (**self).clipboard(text);
    }
}
