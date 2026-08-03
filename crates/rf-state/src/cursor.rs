//! Bounds-checked byte reader used by [`crate::header`] and
//! [`crate::chunk`]. Every read is checked against the remaining slice
//! *before* it is used to index or size an allocation — this is the
//! mechanism that satisfies "no OOM from a huge declared length" for the
//! fixed-width envelope fields (chunk payload length guards live in
//! `chunk.rs`, which checks explicitly so it can name the offending tag).

use crate::error::ContainerError;

pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Take `n` bytes, or a [`ContainerError::Truncated`] naming `context`
    /// if fewer than `n` remain. Never slices past `self.data`.
    pub(crate) fn take(
        &mut self,
        n: usize,
        context: &'static str,
    ) -> Result<&'a [u8], ContainerError> {
        if self.remaining() < n {
            return Err(ContainerError::Truncated {
                context,
                needed: n,
                got: self.remaining(),
            });
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub(crate) fn take_u8(&mut self, context: &'static str) -> Result<u8, ContainerError> {
        Ok(self.take(1, context)?[0])
    }

    pub(crate) fn take_u16_le(&mut self, context: &'static str) -> Result<u16, ContainerError> {
        let b = self.take(2, context)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn take_u32_le(&mut self, context: &'static str) -> Result<u32, ContainerError> {
        let b = self.take(4, context)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn take_u64_le(&mut self, context: &'static str) -> Result<u64, ContainerError> {
        let b = self.take(8, context)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub(crate) fn take_array4(&mut self, context: &'static str) -> Result<[u8; 4], ContainerError> {
        let b = self.take(4, context)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    pub(crate) fn take_array32(
        &mut self,
        context: &'static str,
    ) -> Result<[u8; 32], ContainerError> {
        let b = self.take(32, context)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// Consume `self`, returning whatever bytes remain unread.
    pub(crate) fn into_rest(self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_advances_position_and_returns_slice() {
        let mut c = Cursor::new(&[1, 2, 3, 4, 5]);
        assert_eq!(c.take(2, "x").unwrap(), &[1, 2]);
        assert_eq!(c.remaining(), 3);
        assert_eq!(c.into_rest(), &[3, 4, 5]);
    }

    #[test]
    fn take_past_end_errors_without_panicking() {
        let mut c = Cursor::new(&[1, 2]);
        let err = c.take(5, "ctx").unwrap_err();
        assert_eq!(
            err,
            ContainerError::Truncated {
                context: "ctx",
                needed: 5,
                got: 2
            }
        );
    }

    #[test]
    fn take_u64_le_round_trips() {
        let bytes = 0x0102_0304_0506_0708u64.to_le_bytes();
        let mut c = Cursor::new(&bytes);
        assert_eq!(c.take_u64_le("t").unwrap(), 0x0102_0304_0506_0708);
    }
}
