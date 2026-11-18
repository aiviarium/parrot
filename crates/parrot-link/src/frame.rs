//! Link frames (spec 24.2).
//!
//! On a byte stream each Noise message is prefixed by its length as two bytes, big-endian. Inside
//! a Noise transport message the plaintext is one flag byte and a part of a JSON-RPC message; the
//! flag's lowest bit marks the last part. A message is joined from its parts before parsing, and
//! the 32 MiB message limit (spec 3.1) applies to the joined message.

use crate::LinkError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Noise's limit on one message, ciphertext included.
pub const NOISE_MAX: usize = 65_535;
/// The authentication tag ChaChaPoly adds to every transport message.
pub const TAG_LEN: usize = 16;
/// The smallest `maxMessage` a carrier may state.
pub const MIN_MESSAGE: usize = 4 * 1024;
/// The limit on one joined JSON-RPC message.
pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;
/// The largest `blob/put` chunk (E9).
pub const BLOB_CHUNK_MAX: usize = 1024 * 1024;

const LAST: u8 = 0x01;

/// The size of the plaintext part that fits one Noise message of at most `max_message` bytes.
pub fn part_len(max_message: usize) -> usize {
    max_message.clamp(MIN_MESSAGE, NOISE_MAX) - TAG_LEN - 1
}

/// Splits `msg` into plaintexts of at most `part_len + 1` bytes, each a flag byte and a part.
/// An empty message is one empty last part.
pub fn split(msg: &[u8], part_len: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
    assert!(part_len > 0);
    let count = msg.len().div_ceil(part_len).max(1);
    (0..count).map(move |i| {
        let start = i * part_len;
        let end = (start + part_len).min(msg.len());
        let mut out = Vec::with_capacity(end - start + 1);
        out.push(if i + 1 == count { LAST } else { 0 });
        out.extend_from_slice(&msg[start..end]);
        out
    })
}

/// Joins the parts of one message.
pub struct Assembler {
    buf: Vec<u8>,
    limit: usize,
}

impl Assembler {
    pub fn new(limit: usize) -> Assembler {
        Assembler {
            buf: Vec::new(),
            limit,
        }
    }

    /// Adds one plaintext. Returns the message once its last part arrives.
    pub fn push(&mut self, plain: &[u8]) -> Result<Option<Vec<u8>>, LinkError> {
        let (&flag, part) = plain
            .split_first()
            .ok_or_else(|| LinkError::Protocol("empty link frame".into()))?;
        if flag & !LAST != 0 {
            return Err(LinkError::Protocol(format!("unknown frame flags {flag:#04x}")));
        }
        let len = self.buf.len() + part.len();
        if len > self.limit {
            return Err(LinkError::TooLarge(len));
        }
        self.buf.extend_from_slice(part);
        Ok((flag & LAST != 0).then(|| std::mem::take(&mut self.buf)))
    }
}

/// Reads one length-prefixed Noise message. `None` at a clean end of stream between messages.
pub async fn read_noise<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>, LinkError> {
    let mut len = [0u8; 2];
    match r.read_exact(&mut len[..1]).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    r.read_exact(&mut len[1..]).await?;
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(LinkError::Protocol("empty noise message".into()));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Writes one Noise message with its length prefix.
pub async fn write_noise<W: AsyncWrite + Unpin>(w: &mut W, msg: &[u8]) -> Result<(), LinkError> {
    let len = u16::try_from(msg.len()).map_err(|_| LinkError::TooLarge(msg.len()))?;
    let mut out = Vec::with_capacity(msg.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(msg);
    w.write_all(&out).await?;
    Ok(())
}

/// Splits `data` into `blob/put` chunks of at most 1 MiB: `(index, chunk, last)`.
pub fn blob_chunks(data: &[u8]) -> impl Iterator<Item = (usize, &[u8], bool)> {
    let count = data.len().div_ceil(BLOB_CHUNK_MAX).max(1);
    (0..count).map(move |i| {
        let start = i * BLOB_CHUNK_MAX;
        let end = (start + BLOB_CHUNK_MAX).min(data.len());
        (i, &data[start..end], i + 1 == count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_joins() {
        let msg: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let len = part_len(NOISE_MAX);
        assert_eq!(len, 65_518);
        let parts: Vec<_> = split(&msg, len).collect();
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p| p.len() + TAG_LEN <= NOISE_MAX));
        let mut asm = Assembler::new(MAX_MESSAGE);
        let mut out = None;
        for (i, p) in parts.iter().enumerate() {
            out = asm.push(p).unwrap();
            assert_eq!(out.is_some(), i == parts.len() - 1);
        }
        assert_eq!(out.unwrap(), msg);
    }

    #[test]
    fn empty_message_is_one_part() {
        let parts: Vec<_> = split(b"", 10).collect();
        assert_eq!(parts, vec![vec![LAST]]);
        assert_eq!(Assembler::new(10).push(&parts[0]).unwrap(), Some(vec![]));
    }

    #[test]
    fn rejects_bad_frames() {
        let mut asm = Assembler::new(4);
        assert!(asm.push(&[]).is_err());
        assert!(asm.push(&[0x80, 1]).is_err());
        assert!(asm.push(&[0, 1, 2, 3]).unwrap().is_none());
        assert!(matches!(asm.push(&[LAST, 4, 5]), Err(LinkError::TooLarge(5))));
    }

    #[test]
    fn small_carriers_get_small_parts() {
        assert_eq!(part_len(100), MIN_MESSAGE - TAG_LEN - 1);
        assert_eq!(part_len(10_000), 10_000 - TAG_LEN - 1);
    }

    #[test]
    fn blob_chunks_are_at_most_one_mib() {
        let data = vec![1u8; 2 * BLOB_CHUNK_MAX + 5];
        let chunks: Vec<_> = blob_chunks(&data).collect();
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|(_, c, _)| c.len() <= BLOB_CHUNK_MAX));
        assert_eq!(chunks[2].1.len(), 5);
        assert!(chunks[2].2 && !chunks[0].2);
    }
}
