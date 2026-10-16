//! Typed RFB messages for both directions.

use alloc::vec::Vec;

use crate::{ProtoError, consts::limits, wire::Reader};

pub mod client;
pub mod server;

#[cfg(test)]
mod proptests;

/// The body of a cut-text message (client or server), after the type byte and
/// three padding bytes. With the Extended Clipboard pseudo-encoding the length
/// is signed: a negative one introduces an extended message.
pub(crate) enum CutText {
    /// Latin-1 text.
    Legacy(Vec<u8>),
    /// `flags` and the rest of the message, uninterpreted (a zlib stream for
    /// `provide`, the size list for `caps`, nothing for the other actions).
    Extended { flags: u32, payload: Vec<u8> },
}

pub(crate) fn read_cut_text(
    r: &mut Reader<'_>,
    field: &'static str,
) -> Result<CutText, ProtoError> {
    let len = r.i32()?;
    if len >= 0 {
        let len = len.unsigned_abs();
        if len > limits::MAX_CUT_TEXT_LEN {
            return Err(ProtoError::LimitExceeded {
                field,
                value: u64::from(len),
                limit: u64::from(limits::MAX_CUT_TEXT_LEN),
            });
        }
        return Ok(CutText::Legacy(r.take(len as usize)?.to_vec()));
    }
    // `i32::MIN` has no positive counterpart in an `i32`, hence `unsigned_abs`.
    let total = len.unsigned_abs();
    if total > limits::MAX_EXT_CLIPBOARD_INFLATED {
        return Err(ProtoError::LimitExceeded {
            field,
            value: u64::from(total),
            limit: u64::from(limits::MAX_EXT_CLIPBOARD_INFLATED),
        });
    }
    if total < 4 {
        return Err(ProtoError::Malformed {
            ctx: "extended clipboard message without flags",
        });
    }
    let flags = r.u32()?;
    let payload = r.take(total as usize - 4)?.to_vec();
    Ok(CutText::Extended { flags, payload })
}

pub(crate) fn write_extended_cut_text(
    out: &mut Vec<u8>,
    message_type: u8,
    flags: u32,
    payload: &[u8],
    field: &'static str,
) -> Result<(), ProtoError> {
    let total = payload.len() as u64 + 4;
    let length = u32::try_from(total)
        .ok()
        .filter(|&n| n <= limits::MAX_EXT_CLIPBOARD_INFLATED)
        .ok_or(ProtoError::LimitExceeded {
            field,
            value: total,
            limit: u64::from(limits::MAX_EXT_CLIPBOARD_INFLATED),
        })?;
    out.extend_from_slice(&[message_type, 0, 0, 0]);
    // At most 16 MiB, so it fits an `i32`; the wire value is its negation.
    let signed = -i32::try_from(length).unwrap_or(i32::MAX);
    out.extend_from_slice(&signed.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}
