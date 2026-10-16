//! A persistent zlib inflate stream, as used by the zlib and ZRLE encodings.

use flate2::{Decompress, FlushDecompress, Status};

use crate::DecodeError;

/// One zlib stream that lives as long as the connection. The server ends each
/// rectangle with a sync flush, so every payload continues the same stream and
/// may refer back to earlier data.
#[derive(Default)]
pub(crate) struct Inflater(Option<Decompress>);

impl core::fmt::Debug for Inflater {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Inflater")
            .field("started", &self.0.is_some())
            .finish()
    }
}

impl Inflater {
    /// Forget everything: the next chunk starts a fresh stream.
    pub fn reset(&mut self) {
        self.0 = None;
    }

    /// Inflate `input`, the next chunk of the stream. Fails if the output
    /// would exceed `limit` bytes (zip-bomb guard); the stream is unusable
    /// after any error.
    pub fn inflate(&mut self, input: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
        let z = self.0.get_or_insert_with(|| Decompress::new(true));
        let start = z.total_in();
        let mut out = Vec::new();
        loop {
            let consumed = advanced(z, start);
            if out.len() == out.capacity() {
                // Room for one byte past the limit, so overflow is detectable.
                let extra = out.capacity().max(4096).min(limit + 1 - out.len());
                out.reserve_exact(extra);
            }
            let before = (consumed, out.len());
            let status = z
                .decompress_vec(&input[consumed..], &mut out, FlushDecompress::Sync)
                .map_err(|_| DecodeError::Malformed("corrupt zlib data"))?;
            if out.len() > limit {
                return Err(DecodeError::Malformed("inflated data larger than allowed"));
            }
            if status == Status::StreamEnd {
                return Err(DecodeError::Malformed("zlib stream ended early"));
            }
            let consumed = advanced(z, start);
            // Done when everything was read and a call produced nothing more:
            // stopping at the first call that left spare room can lose the
            // tail of a large chunk.
            if consumed == input.len() && out.len() == before.1 {
                return Ok(out);
            }
            if (consumed, out.len()) == before {
                return Err(DecodeError::Malformed("zlib inflate made no progress"));
            }
        }
    }
}

/// Input bytes `z` has consumed since `start`.
fn advanced(z: &Decompress, start: u64) -> usize {
    // Bounded by the length of a slice we passed in.
    usize::try_from(z.total_in() - start).unwrap_or(usize::MAX)
}
