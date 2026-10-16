//! Legacy clipboard text for `ClientCutText` / `ServerCutText`: Latin-1 bytes
//! with LF line ends, at most [`MAX_CUT_TEXT_LEN`] bytes.

use rfb_proto::consts::limits::MAX_CUT_TEXT_LEN;

/// Why local text could not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardError {
    /// Longer than the protocol limit; nothing is sent.
    TooLarge { bytes: usize },
}

/// Text ready to send, plus what was lost on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub latin1: Vec<u8>,
    /// Characters outside Latin-1 that were replaced by `?`.
    pub replaced: usize,
}

/// Convert local clipboard text for the wire: CR LF and lone CR become LF,
/// characters above U+00FF become `?` (the legacy message cannot carry them).
///
/// # Errors
/// [`ClipboardError::TooLarge`] if the result would exceed the protocol limit.
pub fn to_remote(text: &str) -> Result<Outgoing, ClipboardError> {
    let mut latin1 = Vec::with_capacity(text.len());
    let mut replaced = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let c = if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            '\n'
        } else {
            c
        };
        if let Ok(byte) = u8::try_from(u32::from(c)) {
            latin1.push(byte);
        } else {
            latin1.push(b'?');
            replaced += 1;
        }
    }
    if latin1.len() > MAX_CUT_TEXT_LEN as usize {
        return Err(ClipboardError::TooLarge {
            bytes: latin1.len(),
        });
    }
    Ok(Outgoing { latin1, replaced })
}

/// Convert text from the server for the local clipboard.
///
/// Control characters other than tab and newline are dropped: a remote
/// machine that can put escape sequences or carriage returns on your clipboard
/// can make a later paste into a terminal do things you did not see.
#[must_use]
pub fn from_remote(latin1: &[u8]) -> String {
    latin1
        .iter()
        .map(|&b| char::from(b))
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_text_round_trips() {
        let out = to_remote("caf\u{e9} \u{fc}ber\ttab\nline").unwrap();
        assert_eq!(out.latin1, b"caf\xe9 \xfcber\ttab\nline");
        assert_eq!(out.replaced, 0);
        assert_eq!(from_remote(&out.latin1), "caf\u{e9} \u{fc}ber\ttab\nline");
    }

    #[test]
    fn line_ends_become_lf() {
        assert_eq!(to_remote("a\r\nb\rc\nd").unwrap().latin1, b"a\nb\nc\nd");
        assert_eq!(to_remote("\r\n\r\n").unwrap().latin1, b"\n\n");
    }

    #[test]
    fn characters_outside_latin1_are_replaced_and_counted() {
        let out = to_remote("price: 5\u{20ac} \u{4f60}\u{597d}").unwrap();
        assert_eq!(out.latin1, b"price: 5? ??");
        assert_eq!(out.replaced, 3);
    }

    #[test]
    fn oversized_text_is_refused() {
        let limit = MAX_CUT_TEXT_LEN as usize;
        assert!(to_remote(&"x".repeat(limit)).is_ok());
        assert_eq!(
            to_remote(&"x".repeat(limit + 1)),
            Err(ClipboardError::TooLarge { bytes: limit + 1 })
        );
        // Counted after conversion: CR LF pairs shrink to one byte.
        assert!(to_remote(&"\r\n".repeat(limit)).is_ok());
    }

    #[test]
    fn remote_text_loses_control_characters_but_keeps_layout() {
        assert_eq!(
            from_remote(b"ls\x1b[2J\x07 -la\r\n\tdone\x00"),
            "ls[2J -la\n\tdone"
        );
        assert_eq!(
            from_remote(b"\x80\x9f\xa0\xff"),
            "\u{a0}\u{ff}",
            "C1 controls go, NBSP stays"
        );
        assert_eq!(from_remote(b""), "");
    }
}
