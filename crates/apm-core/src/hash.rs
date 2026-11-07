//! # Content hashes
//!
//! A file's identity for duplicate matching (`files.hash`, plan assumption 12) is the SHA-1 of its
//! bytes, the same digest Immich keeps as an asset checksum. This module is the only place bytes
//! are turned into a digest: the indexer, the safe copy that verifies what it wrote and the Immich
//! code all go through [`sha1_reader`] or [`HashingReader`] instead of writing their own loop.
//!
//! The functions only wrap readers they are given. Opening files, and keeping a multi-gigabyte read
//! off the async runtime, is the caller's job.
//!
//! [`Sha1Digest`] is the raw 20 bytes: what the `files.hash` BLOB holds and what [`crate::matching`]
//! and [`crate::conflict`] compare. [`to_hex`] and [`parse_hex`] are the text form used on the wire
//! (`FileDetail::hash`) and in logs; [`from_slice`] turns a BLOB read from the database back into a
//! digest.
//!
//! ```
//! use apm_core::hash::{parse_hex, sha1_bytes, to_hex};
//!
//! let digest = sha1_bytes(b"abc");
//! assert_eq!(to_hex(&digest), "a9993e364706816aba3e25717850c26c9cd0d89d");
//! assert_eq!(parse_hex("A9993E364706816ABA3E25717850C26C9CD0D89D"), Ok(digest));
//! ```

use std::io::{self, ErrorKind, Read};
use std::ops::ControlFlow;

use sha1::{Digest, Sha1};
use thiserror::Error;

/// Length of a SHA-1 digest in bytes.
pub const DIGEST_LEN: usize = 20;

/// Size of the read buffer in [`sha1_reader`]. A cancellation is noticed after at most one chunk.
pub const CHUNK_SIZE: usize = 1 << 20;

/// SHA-1 digest of a file's bytes, as stored in the `files.hash` BLOB(20) column.
pub type Sha1Digest = [u8; DIGEST_LEN];

/// Why a hex string is not a SHA-1 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseHexError {
    #[error("a SHA-1 digest has 40 hex digits, got {0} bytes")]
    Length(usize),
    #[error("not a hex digit at byte {0}")]
    Digit(usize),
}

/// The SHA-1 of `data`.
pub fn sha1_bytes(data: &[u8]) -> Sha1Digest {
    let mut hasher = Sha1::new();
    hasher.update(data);
    finish(hasher)
}

/// The SHA-1 of everything `reader` yields, read in [`CHUNK_SIZE`] chunks.
///
/// `on_chunk` is called after each read with the number of bytes just hashed, so a caller can report
/// progress; returning [`ControlFlow::Break`] stops the read and makes the result `Ok(None)`, which
/// is how a cancelled task abandons a huge file within one chunk. Interrupted reads are retried; any
/// other error is returned as it is.
pub fn sha1_reader<R: Read>(
    reader: R,
    mut on_chunk: impl FnMut(u64) -> ControlFlow<()>,
) -> io::Result<Option<Sha1Digest>> {
    let mut reader = HashingReader::new(reader);
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(Some(reader.digest())),
            Ok(read) => {
                if on_chunk(read as u64).is_break() {
                    return Ok(None);
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// A reader that hashes whatever passes through it.
///
/// Use it where the bytes are being read anyway, a copy loop for instance, so the digest costs no
/// second pass: read from it as usual, then take [`digest`](Self::digest) once the end is reached.
/// The digest is only meaningful when the reader was drained to the end.
#[derive(Debug)]
pub struct HashingReader<R> {
    inner: R,
    hasher: Sha1,
    bytes: u64,
}

impl<R> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha1::new(),
            bytes: 0,
        }
    }

    /// Bytes read, and hashed, so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// The digest of everything read so far.
    pub fn digest(self) -> Sha1Digest {
        finish(self.hasher)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        if let Some(hashed) = buf.get(..read) {
            self.hasher.update(hashed);
        }
        self.bytes += read as u64;
        Ok(read)
    }
}

/// Lowercase hex form of a digest (40 characters).
pub fn to_hex(digest: &Sha1Digest) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(DIGEST_LEN * 2);
    for byte in digest {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Parses 40 hex digits, upper or lower case.
pub fn parse_hex(text: &str) -> Result<Sha1Digest, ParseHexError> {
    let bytes = text.as_bytes();
    if bytes.len() != DIGEST_LEN * 2 {
        return Err(ParseHexError::Length(bytes.len()));
    }
    let mut digest = [0_u8; DIGEST_LEN];
    for (index, (slot, pair)) in digest.iter_mut().zip(bytes.chunks_exact(2)).enumerate() {
        let high = nibble(pair[0]).ok_or(ParseHexError::Digit(index * 2))?;
        let low = nibble(pair[1]).ok_or(ParseHexError::Digit(index * 2 + 1))?;
        *slot = (high << 4) | low;
    }
    Ok(digest)
}

/// A digest from raw bytes, such as a `files.hash` BLOB; `None` unless there are exactly 20.
pub fn from_slice(bytes: &[u8]) -> Option<Sha1Digest> {
    bytes.try_into().ok()
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

fn finish(hasher: Sha1) -> Sha1Digest {
    let mut digest = [0_u8; DIGEST_LEN];
    digest.copy_from_slice(&hasher.finalize());
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    /// Deterministic, incompressible-enough bytes without a random number generator.
    fn data(len: usize) -> Vec<u8> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    /// Hands out at most `step` bytes per read, like a slow network share.
    struct Trickle<'a> {
        rest: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let take = self.step.min(buf.len()).min(self.rest.len());
            let (now, later) = self.rest.split_at(take);
            buf[..take].copy_from_slice(now);
            self.rest = later;
            Ok(take)
        }
    }

    #[test]
    fn matches_the_published_test_vectors() {
        for (input, expected) in [
            (&b""[..], "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (b"abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                b"The quick brown fox jumps over the lazy dog",
                "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12",
            ),
        ] {
            assert_eq!(to_hex(&sha1_bytes(input)), expected);
        }
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            to_hex(&sha1_bytes(&million_a)),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn hex_round_trips_and_accepts_either_case() {
        let digest = sha1_bytes(b"photo");
        let hex = to_hex(&digest);
        assert_eq!(hex.len(), 40);
        assert_eq!(hex, hex.to_ascii_lowercase());
        assert_eq!(parse_hex(&hex), Ok(digest));
        assert_eq!(parse_hex(&hex.to_ascii_uppercase()), Ok(digest));
    }

    #[test]
    fn bad_hex_is_reported_with_its_position() {
        assert_eq!(parse_hex(""), Err(ParseHexError::Length(0)));
        assert_eq!(parse_hex(&"a".repeat(39)), Err(ParseHexError::Length(39)));
        assert_eq!(parse_hex(&"a".repeat(41)), Err(ParseHexError::Length(41)));
        let mut text = "0".repeat(40);
        text.replace_range(7..8, "g");
        assert_eq!(parse_hex(&text), Err(ParseHexError::Digit(7)));
        let multibyte = format!("{}\u{e9}", "0".repeat(38));
        assert_eq!(parse_hex(&multibyte), Err(ParseHexError::Digit(38)));
    }

    #[test]
    fn a_blob_is_a_digest_only_at_exactly_twenty_bytes() {
        let digest = sha1_bytes(b"x");
        assert_eq!(from_slice(&digest), Some(digest));
        assert_eq!(from_slice(&digest[..19]), None);
        assert_eq!(from_slice(&[digest.as_slice(), &[0]].concat()), None);
        assert_eq!(from_slice(&[]), None);
    }

    #[test]
    fn the_reader_agrees_with_the_slice_around_chunk_boundaries() {
        for len in [
            0,
            1,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            2 * CHUNK_SIZE + 17,
        ] {
            let bytes = data(len);
            let mut seen = 0;
            let digest = sha1_reader(Cursor::new(&bytes), |read| {
                seen += read;
                ControlFlow::Continue(())
            });
            assert_eq!(digest.unwrap(), Some(sha1_bytes(&bytes)), "{len} bytes");
            assert_eq!(seen, len as u64, "{len} bytes");
        }
    }

    #[test]
    fn breaking_out_stops_reading_and_gives_no_digest() {
        let bytes = data(3 * CHUNK_SIZE);
        let mut cursor = Cursor::new(&bytes);
        let mut calls = 0;
        let digest = sha1_reader(&mut cursor, |_| {
            calls += 1;
            ControlFlow::Break(())
        });
        assert_eq!(digest.unwrap(), None);
        assert_eq!(calls, 1);
        assert_eq!(cursor.position(), CHUNK_SIZE as u64);
    }

    #[test]
    fn interrupted_reads_are_retried_and_other_errors_surface() {
        struct Flaky {
            script: Vec<io::Result<Vec<u8>>>,
        }
        impl Read for Flaky {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.script.is_empty() {
                    return Ok(0);
                }
                self.script.remove(0).map(|bytes| {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    bytes.len()
                })
            }
        }

        let interrupted = || io::Error::from(ErrorKind::Interrupted);
        let flaky = Flaky {
            script: vec![
                Err(interrupted()),
                Ok(b"ab".to_vec()),
                Err(interrupted()),
                Ok(b"c".to_vec()),
            ],
        };
        assert_eq!(
            sha1_reader(flaky, |_| ControlFlow::Continue(())).unwrap(),
            Some(sha1_bytes(b"abc"))
        );

        let failing = Flaky {
            script: vec![
                Ok(b"ab".to_vec()),
                Err(io::Error::from(ErrorKind::PermissionDenied)),
            ],
        };
        let error = sha1_reader(failing, |_| ControlFlow::Continue(())).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_hashing_reader_counts_and_hashes_what_passes_through() {
        let bytes = data(10_000);
        let mut reader = HashingReader::new(Trickle {
            rest: &bytes,
            step: 7,
        });
        let mut copy = Vec::new();
        io::copy(&mut reader, &mut copy).unwrap();
        assert_eq!(copy, bytes);
        assert_eq!(reader.bytes_read(), 10_000);
        assert_eq!(reader.digest(), sha1_bytes(&bytes));
    }

    proptest! {
        #[test]
        fn the_digest_does_not_depend_on_how_reads_are_split(
            bytes in proptest::collection::vec(any::<u8>(), 0..4096),
            step in 1_usize..600,
        ) {
            let mut reader = HashingReader::new(Trickle { rest: &bytes, step });
            io::copy(&mut reader, &mut io::sink()).unwrap();
            prop_assert_eq!(reader.digest(), sha1_bytes(&bytes));
        }

        #[test]
        fn hex_round_trips_for_any_digest(digest in any::<[u8; DIGEST_LEN]>()) {
            prop_assert_eq!(parse_hex(&to_hex(&digest)), Ok(digest));
        }
    }
}
