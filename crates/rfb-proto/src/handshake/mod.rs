//! Handshake state machines for both roles (RFB 3.3, 3.7 and 3.8).
//!
//! Both machines are driven the same way: call `poll` with the bytes received
//! so far. It returns how many bytes it consumed and, at most, one event. Call
//! it again until it returns `(0, None)`. Bytes to send are appended to `out`.
//! Security types beyond None and VNC Auth hook in through `rfb-security`
//! later; VNC Auth itself is a challenge/response the caller answers.

mod client;
mod server;

pub use client::{ClientConfig, ClientEvent, ClientHandshake};
pub use server::{ServerConfig, ServerEvent, ServerHandshake};

use crate::{ProtoError, consts::limits, wire::Reader};

/// Run `f` over `input`. `Ok(None)` means more bytes are needed.
fn try_read<T>(
    input: &[u8],
    f: impl FnOnce(&mut Reader<'_>) -> Result<T, ProtoError>,
) -> Result<Option<(T, usize)>, ProtoError> {
    let mut r = Reader::new(input);
    match f(&mut r) {
        Ok(v) => Ok(Some((v, r.position()))),
        Err(ProtoError::NeedMore(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A `U32` length followed by that many bytes (reason strings, desktop name).
fn read_string<'a>(r: &mut Reader<'a>, field: &'static str) -> Result<&'a [u8], ProtoError> {
    let len = r.u32()?;
    if len > limits::MAX_NAME_LEN {
        return Err(ProtoError::LimitExceeded {
            field,
            value: u64::from(len),
            limit: u64::from(limits::MAX_NAME_LEN),
        });
    }
    r.take(len as usize)
}

fn write_string(
    out: &mut alloc::vec::Vec<u8>,
    s: &[u8],
    field: &'static str,
) -> Result<(), ProtoError> {
    let len = u32::try_from(s.len())
        .ok()
        .filter(|&n| n <= limits::MAX_NAME_LEN)
        .ok_or(ProtoError::LimitExceeded {
            field,
            value: s.len() as u64,
            limit: u64::from(limits::MAX_NAME_LEN),
        })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s);
    Ok(())
}

#[cfg(test)]
mod tests;
