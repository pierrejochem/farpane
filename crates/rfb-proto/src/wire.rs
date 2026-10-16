//! Bounds-checked big-endian reading over a byte slice.

use crate::ProtoError;

/// A cursor over input bytes. Reading past the end returns
/// [`ProtoError::NeedMore`] and leaves the cursor where it was.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes consumed so far.
    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Bytes still available.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Take `n` bytes.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] if fewer than `n` bytes are available.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        let rem = self.remaining();
        if rem < n {
            return Err(ProtoError::NeedMore(n - rem));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// Take a fixed-size array.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] if fewer than `N` bytes are available.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtoError> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    /// # Errors
    /// [`ProtoError::NeedMore`] at end of input.
    pub fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.array::<1>()?[0])
    }

    /// # Errors
    /// [`ProtoError::NeedMore`] at end of input.
    pub fn u16(&mut self) -> Result<u16, ProtoError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// # Errors
    /// [`ProtoError::NeedMore`] at end of input.
    pub fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// # Errors
    /// [`ProtoError::NeedMore`] at end of input.
    pub fn i32(&mut self) -> Result<i32, ProtoError> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    /// Skip `n` padding bytes.
    ///
    /// # Errors
    /// [`ProtoError::NeedMore`] if fewer than `n` bytes are available.
    pub fn skip(&mut self, n: usize) -> Result<(), ProtoError> {
        self.take(n).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_big_endian_and_reports_shortfall() {
        let mut r = Reader::new(&[0x01, 0x02, 0x03]);
        assert_eq!(r.u16(), Ok(0x0102));
        assert_eq!(r.u16(), Err(ProtoError::NeedMore(1)));
        assert_eq!(r.position(), 2, "a failed read must not advance");
        assert_eq!(r.u8(), Ok(0x03));
    }
}
