//! A ring of the most recent raw output, addressed by absolute byte offsets.

use std::collections::VecDeque;

/// Reading from an offset that is no longer, or not yet, in the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OffsetError {
    /// The bytes were dropped. `oldest` is the oldest offset still held; the reader needs a fresh
    /// snapshot.
    #[error("offset {offset} is older than the oldest buffered byte {oldest}")]
    TooOld { offset: u64, oldest: u64 },
    /// The offset is past the end of the output so far.
    #[error("offset {offset} is past the end of the output {end}")]
    Future { offset: u64, end: u64 },
}

/// The last `capacity` bytes of a session's output.
#[derive(Clone, Debug)]
pub struct OutputBuffer {
    data: VecDeque<u8>,
    start: u64,
    capacity: usize,
}

impl OutputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::new(),
            start: 0,
            capacity,
        }
    }

    /// An empty buffer whose next byte has offset `start`.
    pub fn starting_at(capacity: usize, start: u64) -> Self {
        Self {
            data: VecDeque::new(),
            start,
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Offset of the oldest byte held.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Offset one past the newest byte: the total number of bytes ever pushed.
    pub fn end(&self) -> u64 {
        self.start + self.data.len() as u64
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            let drop = self.data.len() as u64 + (bytes.len() - self.capacity) as u64;
            self.data.clear();
            self.data.extend(&bytes[bytes.len() - self.capacity..]);
            self.start += drop;
            return;
        }
        let overflow = (self.data.len() + bytes.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.data.drain(..overflow);
            self.start += overflow as u64;
        }
        self.data.extend(bytes);
    }

    /// All bytes from `offset` to the end.
    pub fn read_from(&self, offset: u64) -> Result<Vec<u8>, OffsetError> {
        self.read_range(offset, self.end())
    }

    /// Bytes from `from` to `to` (exclusive).
    pub fn read_range(&self, from: u64, to: u64) -> Result<Vec<u8>, OffsetError> {
        if from < self.start {
            return Err(OffsetError::TooOld {
                offset: from,
                oldest: self.start,
            });
        }
        if from > self.end() || to > self.end() {
            return Err(OffsetError::Future {
                offset: from.max(to),
                end: self.end(),
            });
        }
        let a = (from - self.start) as usize;
        let b = (to.max(from) - self.start) as usize;
        let (x, y) = self.data.as_slices();
        let mut out = Vec::with_capacity(b - a);
        if a < x.len() {
            out.extend_from_slice(&x[a..b.min(x.len())]);
        }
        if b > x.len() {
            out.extend_from_slice(&y[a.saturating_sub(x.len())..b - x.len()]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_bytes_with_offsets() {
        let mut b = OutputBuffer::new(10);
        b.push(b"hello ");
        b.push(b"world!");
        assert_eq!(b.start(), 2);
        assert_eq!(b.end(), 12);
        assert_eq!(b.read_from(6).unwrap(), b"world!");
        assert_eq!(b.read_from(2).unwrap(), b"llo world!");
        assert_eq!(b.read_from(12).unwrap(), b"");
        assert_eq!(b.read_from(1), Err(OffsetError::TooOld { offset: 1, oldest: 2 }));
        assert!(matches!(b.read_from(13), Err(OffsetError::Future { .. })));
        b.push(b"0123456789abc");
        assert_eq!(b.start(), 15);
        assert_eq!(b.read_from(15).unwrap(), b"3456789abc");
        assert_eq!(b.read_range(16, 18).unwrap(), b"45");
    }

    #[test]
    fn wraps_around() {
        let mut b = OutputBuffer::new(7);
        let mut all = Vec::new();
        for i in 0..100u8 {
            let chunk = vec![i; (i % 5) as usize + 1];
            b.push(&chunk);
            all.extend_from_slice(&chunk);
            let from = b.start();
            assert_eq!(b.read_from(from).unwrap(), &all[from as usize..]);
            assert!(b.len() <= 7);
        }
    }
}
