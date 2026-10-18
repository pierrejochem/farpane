//! The messages between the Farpane service and its session worker.
//!
//! The service owns the network, the accounts and the policy; the worker runs
//! in the user's session, where the screen is. They talk over two anonymous
//! pipes that only the two processes hold, so there is nothing to connect to
//! and no authentication on top. The framing is `u32` length (little endian,
//! counting the tag and the body), one tag byte, then the body.
//!
//! This module is plain Rust with no Win32 calls, so the format is tested on
//! any system. Every length read from the pipe is checked against
//! [`MAX_MESSAGE`] before memory is reserved for it.

use std::io::{self, Read, Write};

use rfb_codec::Cursor;
use rfb_proto::types::Rect;

/// Largest message accepted: a full 16384 x 8192 frame fits.
pub const MAX_MESSAGE: usize = 640 << 20;
/// Largest clipboard text accepted.
pub const MAX_TEXT: usize = 16 << 20;

const TAG_FRAME: u8 = 1;
const TAG_KEY: u8 = 2;
const TAG_POINTER: u8 = 3;
const TAG_CLIPBOARD: u8 = 4;

/// From the worker to the service: what the screen looks like now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The size of the whole picture after this message.
    pub width: u16,
    pub height: u16,
    /// Changed parts, each with `B, G, R, X` pixels, rows without padding.
    pub rects: Vec<(Rect, Vec<u8>)>,
    pub cursor: Option<Cursor>,
    pub clipboard: Option<String>,
}

/// From the service to the worker: input that a client sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Key { keysym: u32, down: bool },
    Pointer { x: u16, y: u16, buttons: u8 },
    Clipboard(String),
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// A message body being filled in.
struct Out(Vec<u8>);

impl Out {
    fn new(tag: u8) -> Self {
        Self(vec![0, 0, 0, 0, tag])
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }
    fn finish(mut self) -> io::Result<Vec<u8>> {
        let len = u32::try_from(self.0.len() - 4)
            .ok()
            .filter(|l| (*l as usize) <= MAX_MESSAGE)
            .ok_or_else(|| bad("message too large"))?;
        self.0[..4].copy_from_slice(&len.to_le_bytes());
        Ok(self.0)
    }
}

/// A message body being read.
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n {
            return Err(bad("message ends early"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn text(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        if n > MAX_TEXT {
            return Err(bad("text too long"));
        }
        // Invalid UTF-8 cannot come from our own worker; replace rather than fail.
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn done(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad("trailing bytes in message"))
        }
    }
}

fn put_text(out: &mut Out, text: &str) -> io::Result<()> {
    if text.len() > MAX_TEXT {
        return Err(bad("text too long"));
    }
    out.u32(u32::try_from(text.len()).map_err(|_| bad("text too long"))?);
    out.bytes(text.as_bytes());
    Ok(())
}

impl Frame {
    /// Serialise into one framed message.
    ///
    /// # Errors
    /// A rectangle whose pixels do not match its size, one outside the
    /// picture, or a message over [`MAX_MESSAGE`].
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut out = Out::new(TAG_FRAME);
        out.u16(self.width);
        out.u16(self.height);
        out.u32(u32::try_from(self.rects.len()).map_err(|_| bad("too many rectangles"))?);
        for (r, pixels) in &self.rects {
            check_rect(*r, self.width, self.height, pixels.len())?;
            out.u16(r.x);
            out.u16(r.y);
            out.u16(r.width);
            out.u16(r.height);
            out.bytes(pixels);
        }
        match &self.cursor {
            None => out.u8(0),
            Some(c) => {
                out.u8(1);
                out.u16(c.hotspot.0);
                out.u16(c.hotspot.1);
                out.u16(c.width);
                out.u16(c.height);
                if c.rgba.len() != usize::from(c.width) * usize::from(c.height) * 4 {
                    return Err(bad("cursor pixels do not match its size"));
                }
                out.bytes(&c.rgba);
            }
        }
        match &self.clipboard {
            None => out.u8(0),
            Some(t) => {
                out.u8(1);
                put_text(&mut out, t)?;
            }
        }
        out.finish()
    }

    fn decode(body: &[u8]) -> io::Result<Self> {
        let mut r = In(body);
        let width = r.u16()?;
        let height = r.u16()?;
        let count = r.u32()? as usize;
        // Each rectangle needs at least its 8-byte header, so the count is bounded by the data.
        if count > body.len() / 8 {
            return Err(bad("rectangle count exceeds the message"));
        }
        let mut rects = Vec::with_capacity(count);
        for _ in 0..count {
            let rect = Rect {
                x: r.u16()?,
                y: r.u16()?,
                width: r.u16()?,
                height: r.u16()?,
            };
            let len = usize::from(rect.width) * usize::from(rect.height) * 4;
            check_rect(rect, width, height, len)?;
            rects.push((rect, r.take(len)?.to_vec()));
        }
        let cursor = if r.u8()? == 0 {
            None
        } else {
            let hotspot = (r.u16()?, r.u16()?);
            let (w, h) = (r.u16()?, r.u16()?);
            let len = usize::from(w) * usize::from(h) * 4;
            Some(Cursor {
                hotspot,
                width: w,
                height: h,
                rgba: r.take(len)?.to_vec(),
            })
        };
        let clipboard = if r.u8()? == 0 { None } else { Some(r.text()?) };
        r.done()?;
        Ok(Self {
            width,
            height,
            rects,
            cursor,
            clipboard,
        })
    }
}

fn check_rect(r: Rect, width: u16, height: u16, pixel_bytes: usize) -> io::Result<()> {
    let inside = u32::from(r.x) + u32::from(r.width) <= u32::from(width)
        && u32::from(r.y) + u32::from(r.height) <= u32::from(height);
    if !inside || pixel_bytes != usize::from(r.width) * usize::from(r.height) * 4 {
        return Err(bad("rectangle outside the picture or wrong pixel count"));
    }
    Ok(())
}

impl Command {
    /// Serialise into one framed message.
    ///
    /// # Errors
    /// Clipboard text over [`MAX_TEXT`].
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let out = match self {
            Self::Key { keysym, down } => {
                let mut o = Out::new(TAG_KEY);
                o.u32(*keysym);
                o.u8(u8::from(*down));
                o
            }
            Self::Pointer { x, y, buttons } => {
                let mut o = Out::new(TAG_POINTER);
                o.u16(*x);
                o.u16(*y);
                o.u8(*buttons);
                o
            }
            Self::Clipboard(text) => {
                let mut o = Out::new(TAG_CLIPBOARD);
                put_text(&mut o, text)?;
                o
            }
        };
        out.finish()
    }

    fn decode(tag: u8, body: &[u8]) -> io::Result<Self> {
        let mut r = In(body);
        let cmd = match tag {
            TAG_KEY => Self::Key {
                keysym: r.u32()?,
                down: r.u8()? != 0,
            },
            TAG_POINTER => Self::Pointer {
                x: r.u16()?,
                y: r.u16()?,
                buttons: r.u8()?,
            },
            TAG_CLIPBOARD => Self::Clipboard(r.text()?),
            _ => return Err(bad("unknown command")),
        };
        r.done()?;
        Ok(cmd)
    }
}

/// Read one framed message: its tag and body. `Ok(None)` is a clean end of
/// the stream between messages.
fn read_raw(from: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 4];
    match from.read(&mut head[..1])? {
        0 => return Ok(None),
        _ => from.read_exact(&mut head[1..])?,
    }
    let len = u32::from_le_bytes(head) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(bad("message length out of range"));
    }
    let mut tag = [0u8; 1];
    from.read_exact(&mut tag)?;
    // Grow with the data actually received instead of trusting the length up front.
    let mut body = Vec::new();
    from.take((len - 1) as u64).read_to_end(&mut body)?;
    if body.len() != len - 1 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some((tag[0], body)))
}

/// Read the next [`Frame`]; `None` when the worker closed the pipe.
///
/// # Errors
/// A broken pipe, or a message that is not a valid frame.
pub fn read_frame(from: &mut impl Read) -> io::Result<Option<Frame>> {
    match read_raw(from)? {
        None => Ok(None),
        Some((TAG_FRAME, body)) => Frame::decode(&body).map(Some),
        Some(_) => Err(bad("unexpected message from the worker")),
    }
}

/// Read the next [`Command`]; `None` when the service closed the pipe.
///
/// # Errors
/// A broken pipe, or a message that is not a valid command.
pub fn read_command(from: &mut impl Read) -> io::Result<Option<Command>> {
    match read_raw(from)? {
        None => Ok(None),
        Some((tag, body)) => Command::decode(tag, &body).map(Some),
    }
}

/// Write an already encoded message and flush.
///
/// # Errors
/// The pipe is closed.
pub fn send(to: &mut impl Write, message: &[u8]) -> io::Result<()> {
    to.write_all(message)?;
    to.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn sample() -> Frame {
        Frame {
            width: 8,
            height: 4,
            rects: vec![(rect(2, 1, 2, 2), (0..16).collect())],
            cursor: Some(Cursor {
                hotspot: (1, 1),
                width: 2,
                height: 1,
                rgba: vec![1, 2, 3, 255, 4, 5, 6, 0],
            }),
            clipboard: Some("grüße €".to_owned()),
        }
    }

    #[test]
    fn frame_round_trips() {
        let bytes = sample().encode().unwrap();
        let got = read_frame(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(got, sample());
    }

    #[test]
    fn frame_golden_bytes() {
        let f = Frame {
            width: 1,
            height: 1,
            rects: vec![(rect(0, 0, 1, 1), vec![9, 8, 7, 0])],
            cursor: None,
            clipboard: None,
        };
        // length 23 = tag + width + height + count + rect header + pixels + two flags
        assert_eq!(
            f.encode().unwrap(),
            [
                23, 0, 0, 0, 1, // length, tag
                1, 0, 1, 0, // size
                1, 0, 0, 0, // one rectangle
                0, 0, 0, 0, 1, 0, 1, 0, // x, y, w, h
                9, 8, 7, 0, // pixels
                0, 0, // no cursor, no clipboard
            ]
        );
    }

    #[test]
    fn commands_round_trip() {
        for c in [
            Command::Key {
                keysym: 0xFF0D,
                down: true,
            },
            Command::Pointer {
                x: 600,
                y: 7,
                buttons: 5,
            },
            Command::Clipboard("a\nb".to_owned()),
        ] {
            let bytes = c.encode().unwrap();
            assert_eq!(read_command(&mut bytes.as_slice()).unwrap(), Some(c));
        }
    }

    #[test]
    fn messages_split_byte_by_byte() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = self.0.len().min(1).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let bytes = sample().encode().unwrap();
        let got = read_frame(&mut OneByte(&bytes)).unwrap().unwrap();
        assert_eq!(got, sample());
    }

    #[test]
    fn clean_end_is_none_and_cut_message_is_error() {
        assert_eq!(read_command(&mut [].as_slice()).unwrap(), None);
        let bytes = sample().encode().unwrap();
        assert!(read_frame(&mut &bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn oversized_and_inconsistent_input_is_refused() {
        // Length far above the limit: refused before any allocation.
        let huge = [0xFF, 0xFF, 0xFF, 0x7F, 1];
        assert!(read_frame(&mut huge.as_slice()).is_err());
        // A rectangle that leaves the picture.
        let mut f = sample();
        f.rects[0].0 = rect(7, 0, 2, 2);
        assert!(f.encode().is_err());
        // A rectangle count larger than the data can hold.
        let mut bytes = sample().encode().unwrap();
        bytes[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut bytes.as_slice()).is_err());
        // The wrong direction.
        let cmd = Command::Key {
            keysym: 1,
            down: false,
        }
        .encode()
        .unwrap();
        assert!(read_frame(&mut cmd.as_slice()).is_err());
    }
}
