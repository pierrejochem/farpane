//! Extended Clipboard (UTF-8 text) on top of the cut-text messages.
//!
//! `rfb-proto` carries the messages as a `flags` word plus an uninterpreted
//! payload. This module gives them meaning (rfbproto.rst, "Extended Clipboard
//! Pseudo-Encoding") and drives the exchange: capabilities, then either
//! `notify` and `request` or an unsolicited `provide`. Only the `text` format
//! is implemented; the other formats are announced and skipped, never
//! requested. Both roles use the same state machine.

use std::io::Read;

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rfb_proto::consts::limits::MAX_EXT_CLIPBOARD_INFLATED;

/// Format bits (0-15) and action bits (24-28) of the `flags` word.
pub mod flags {
    pub const TEXT: u32 = 1 << 0;
    pub const RTF: u32 = 1 << 1;
    pub const HTML: u32 = 1 << 2;
    pub const DIB: u32 = 1 << 3;
    pub const FILES: u32 = 1 << 4;
    /// All format bits.
    pub const FORMATS: u32 = 0xFFFF;
    pub const CAPS: u32 = 1 << 24;
    pub const REQUEST: u32 = 1 << 25;
    pub const PEEK: u32 = 1 << 26;
    pub const NOTIFY: u32 = 1 << 27;
    pub const PROVIDE: u32 = 1 << 28;
    /// All action bits.
    pub const ACTIONS: u32 = CAPS | REQUEST | PEEK | NOTIFY | PROVIDE;
}

/// Why a message could not be understood. Fatal for the clipboard exchange,
/// not necessarily for the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    Malformed(&'static str),
    /// The data inflates to more than [`MAX_EXT_CLIPBOARD_INFLATED`].
    TooLarge,
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed clipboard message: {what}"),
            Self::TooLarge => write!(f, "clipboard data too large"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// One message, interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// What the sender can receive, with the largest size it accepts
    /// *unsolicited* for each announced format (0 means "tell me first").
    Caps {
        formats: u32,
        actions: u32,
        sizes: Vec<u32>,
    },
    Request {
        formats: u32,
    },
    Peek,
    Notify {
        formats: u32,
    },
    /// The data. `text` is the UTF-8 text if the text format was included.
    Provide {
        formats: u32,
        text: Option<String>,
    },
}

/// Interpret `flags` and `payload` of an extended clipboard message.
///
/// # Errors
/// [`ClipboardError`] for a payload that does not match its flags, several
/// actions in one message, or data that does not inflate.
pub fn parse(msg_flags: u32, payload: &[u8]) -> Result<Action, ClipboardError> {
    let formats = msg_flags & flags::FORMATS;
    if msg_flags & flags::CAPS != 0 {
        let count = formats.count_ones() as usize;
        if payload.len() != count * 4 {
            return Err(ClipboardError::Malformed("capability sizes"));
        }
        let sizes = payload
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        return Ok(Action::Caps {
            formats,
            actions: msg_flags & flags::ACTIONS & !flags::CAPS,
            sizes,
        });
    }
    let action = msg_flags & flags::ACTIONS;
    if action.count_ones() != 1 {
        return Err(ClipboardError::Malformed("not exactly one action"));
    }
    match action {
        flags::REQUEST | flags::NOTIFY | flags::PEEK if !payload.is_empty() => {
            Err(ClipboardError::Malformed("unexpected data after the flags"))
        }
        flags::REQUEST => Ok(Action::Request { formats }),
        flags::NOTIFY => Ok(Action::Notify { formats }),
        flags::PEEK => Ok(Action::Peek),
        _ => Ok(Action::Provide {
            formats,
            text: provided_text(formats, payload)?,
        }),
    }
}

/// Inflate a `provide` payload and pick the text out of it.
fn provided_text(formats: u32, payload: &[u8]) -> Result<Option<String>, ClipboardError> {
    let mut data = Vec::new();
    ZlibDecoder::new(payload)
        .take(u64::from(MAX_EXT_CLIPBOARD_INFLATED) + 1)
        .read_to_end(&mut data)
        .map_err(|_| ClipboardError::Malformed("zlib data"))?;
    if data.len() > MAX_EXT_CLIPBOARD_INFLATED as usize {
        return Err(ClipboardError::TooLarge);
    }
    let mut rest = data.as_slice();
    let mut text = None;
    // One (size, data) pair per announced format, lowest bit first.
    for bit in (0..16).map(|b| 1u32 << b).filter(|b| formats & b != 0) {
        let (head, tail) = rest
            .split_first_chunk::<4>()
            .ok_or(ClipboardError::Malformed("truncated format size"))?;
        let size = u32::from_be_bytes(*head) as usize;
        if size > tail.len() {
            return Err(ClipboardError::Malformed("format data past the end"));
        }
        let (body, after) = tail.split_at(size);
        if bit == flags::TEXT {
            // Terminated by a null even though the size is explicit.
            let body = body.strip_suffix(&[0]).unwrap_or(body);
            text = Some(String::from_utf8_lossy(body).into_owned());
        }
        rest = after;
    }
    Ok(text)
}

/// A message to send: `flags` and `payload` for `rfb-proto`'s
/// `ExtendedClipboard`.
pub type Message = (u32, Vec<u8>);

/// Our capabilities: text only, with `text_max` as the largest text we accept
/// without being asked. 0 means every change is announced with `notify` first,
/// which the spec recommends (it keeps updates unambiguous).
#[must_use]
pub fn caps(text_max: u32) -> Message {
    (
        flags::CAPS | flags::TEXT | flags::REQUEST | flags::PEEK | flags::NOTIFY | flags::PROVIDE,
        text_max.to_be_bytes().to_vec(),
    )
}

#[must_use]
pub fn request_text() -> Message {
    (flags::REQUEST | flags::TEXT, Vec::new())
}

/// Announce that text is available (`true`) or that the clipboard holds no
/// text (`false`).
#[must_use]
pub fn notify(has_text: bool) -> Message {
    (
        flags::NOTIFY | if has_text { flags::TEXT } else { 0 },
        Vec::new(),
    )
}

/// The text in the wire form: UTF-8 with CR LF line ends and a terminating
/// null, as the spec requires.
fn wire_text(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                bytes.extend_from_slice(b"\r\n");
            }
            '\n' => bytes.extend_from_slice(b"\r\n"),
            c => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    bytes.push(0);
    bytes
}

/// The size `text` has on the wire, for comparing with a peer's limit.
#[must_use]
pub fn wire_size(text: &str) -> usize {
    wire_text(text).len()
}

/// Provide `text`.
///
/// # Errors
/// [`ClipboardError::TooLarge`] above [`MAX_EXT_CLIPBOARD_INFLATED`].
pub fn provide_text(text: &str) -> Result<Message, ClipboardError> {
    use std::io::Write;
    let body = wire_text(text);
    let size = u32::try_from(body.len()).map_err(|_| ClipboardError::TooLarge)?;
    if size > MAX_EXT_CLIPBOARD_INFLATED - 4 {
        return Err(ClipboardError::TooLarge);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&size.to_be_bytes())
        .and_then(|()| z.write_all(&body))
        .map_err(|_| ClipboardError::Malformed("compression"))?;
    let compressed = z
        .finish()
        .map_err(|_| ClipboardError::Malformed("compression"))?;
    Ok((flags::PROVIDE | flags::TEXT, compressed))
}

/// Which side of the connection this is. The client answers the server's
/// capabilities with its own; the server announces its capabilities itself
/// when the client enables the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

/// What the peer told us it can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PeerCaps {
    text: bool,
    /// The largest text it takes unsolicited.
    text_max: u32,
}

/// The result of handling one message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    /// Messages to send, in order.
    pub send: Vec<Message>,
    /// New clipboard text from the peer (CR LF line ends as on the wire).
    pub received: Option<String>,
}

/// The Extended Clipboard exchange for one connection.
#[derive(Debug)]
pub struct ExtClipboard {
    role: Role,
    peer: Option<PeerCaps>,
    sent_caps: bool,
    /// The text we would provide if asked.
    local: Option<String>,
}

impl ExtClipboard {
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            role,
            peer: None,
            sent_caps: false,
            local: None,
        }
    }

    /// True once the peer has announced that it handles text, so the extended
    /// messages (and UTF-8) can be used instead of the Latin-1 ones.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.peer.is_some_and(|p| p.text)
    }

    /// Our capabilities, to send when the exchange starts (a server does this
    /// itself after a `SetEncodings` that enables the extension; a client
    /// answers the server's, which [`ExtClipboard::on_message`] does).
    pub fn announce(&mut self) -> Message {
        self.sent_caps = true;
        caps(0)
    }

    /// Handle a message from the peer.
    ///
    /// # Errors
    /// [`ClipboardError`] if the message is malformed.
    pub fn on_message(
        &mut self,
        msg_flags: u32,
        payload: &[u8],
    ) -> Result<Reaction, ClipboardError> {
        let mut out = Reaction::default();
        match parse(msg_flags, payload)? {
            Action::Caps { formats, sizes, .. } => {
                let text = formats & flags::TEXT != 0;
                // The sizes are listed for the announced formats, lowest bit
                // first; text is bit 0, so it comes first when announced.
                let text_max = if text {
                    sizes.first().copied().unwrap_or(0)
                } else {
                    0
                };
                self.peer = Some(PeerCaps { text, text_max });
                if self.role == Role::Client && !self.sent_caps {
                    out.send.push(self.announce());
                }
            }
            Action::Notify { formats } => {
                // Ask for the text if it is there and the peer can provide it.
                if self.is_active() && formats & flags::TEXT != 0 {
                    out.send.push(request_text());
                }
            }
            Action::Request { formats } => {
                if formats & flags::TEXT != 0
                    && let Some(text) = &self.local
                {
                    out.send.push(provide_text(text)?);
                }
            }
            Action::Peek => out.send.push(notify(self.local.is_some())),
            Action::Provide { text, .. } => out.received = text,
        }
        Ok(out)
    }

    /// The local clipboard changed to `text`. Returns what to send: the text
    /// itself if the peer accepts that size unsolicited, otherwise a `notify`
    /// (the peer then asks for it). Empty when the extension is not active.
    pub fn local_changed(&mut self, text: &str) -> Vec<Message> {
        let Some(peer) = self.peer.filter(|p| p.text) else {
            return Vec::new();
        };
        self.local = Some(text.to_owned());
        let fits = u32::try_from(wire_size(text)).is_ok_and(|n| n <= peer.text_max);
        if fits && let Ok(provide) = provide_text(text) {
            return vec![provide];
        }
        vec![notify(true)]
    }
}
