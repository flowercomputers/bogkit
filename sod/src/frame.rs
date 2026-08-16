//! Frame encoding, BLAKE3 hashing, and the on-disk/wire record format.
//!
//! A frame's identity is the BLAKE3 hash of its encoded bytes (SOD-2), and
//! every frame carries the hash of its predecessor from the same origin, so
//! each origin's feed is a hash chain: tamper-evident, equivocation-
//! detectable, and relayable through untrusted peers.
//!
//! The on-disk record format is `u32 LE body-length | 4-byte length-check
//! (blake3 of the length bytes, truncated) | body | 32-byte blake3(body)`.
//! The length-check exists so a corrupted length prefix is *detected*
//! (interior corruption, refused) instead of being misread as a clean torn
//! tail and silently truncating every valid record after it. (Sync
//! transports serialize [`Frame`]s directly via postcard; records are a
//! LogStore concern only.)

use serde::{Deserialize, Serialize};

use crate::SodError;

/// Identity of a replica: 128 random bits, generated when its log is
/// created, and never outliving the log (SOD-3). Deleting or resetting a
/// replica's log requires generating a fresh id — reuse causes silent
/// divergence at peers that remember the old feed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReplicaId(pub [u8; 16]);

impl ReplicaId {
    /// Generate a fresh id from the OS RNG.
    #[cfg(feature = "os-rng")]
    pub fn generate() -> Self {
        let mut b = [0u8; 16];
        getrandom::getrandom(&mut b).expect("OS RNG unavailable");
        ReplicaId(b)
    }

    /// Construct from caller-supplied entropy (for targets without the
    /// `os-rng` feature, e.g. browsers passing `crypto.getRandomValues`).
    pub const fn from_bytes(b: [u8; 16]) -> Self {
        ReplicaId(b)
    }
}

impl core::fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for ReplicaId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

/// BLAKE3 hash of a frame's encoded bytes: the frame's identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FrameHash(pub [u8; 32]);

/// The `prev_hash` of the first frame (`seq == 1`) in a feed.
pub const ZERO_HASH: FrameHash = FrameHash([0u8; 32]);

impl core::fmt::Display for FrameHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl core::fmt::Debug for FrameHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

/// One committed batch of deltas from one origin.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frame {
    /// Hash of this origin's previous frame; [`ZERO_HASH`] at `seq == 1`.
    pub prev_hash: FrameHash,
    /// The replica that committed this frame.
    pub origin: ReplicaId,
    /// 1-based, contiguous per origin.
    pub seq: u64,
    /// Origin-stamped event time, milliseconds since the unix epoch.
    /// Produced by the committing application; never observed as a clock
    /// inside sod (SOD-7).
    pub event_time: u64,
    /// `(postcard-encoded datum, signed multiplicity)` deltas.
    pub payload: Vec<(Vec<u8>, i64)>,
}

impl Frame {
    /// The frame's body bytes (postcard).
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("frame encoding is infallible")
    }

    /// The frame's identity: BLAKE3 of [`encode`](Frame::encode).
    pub fn hash(&self) -> FrameHash {
        FrameHash(*blake3::hash(&self.encode()).as_bytes())
    }

    /// Append the full record — `len | len-check | body | hash` — to `out`.
    pub fn encode_record(&self, out: &mut Vec<u8>) {
        let body = self.encode();
        let len_bytes = (body.len() as u32).to_le_bytes();
        out.extend_from_slice(&len_bytes);
        out.extend_from_slice(&len_check(&len_bytes));
        let hash = blake3::hash(&body);
        out.extend_from_slice(&body);
        out.extend_from_slice(hash.as_bytes());
    }
}

/// Record header: 4-byte LE length + 4-byte length-check.
const HEADER: usize = 8;

fn len_check(len_bytes: &[u8; 4]) -> [u8; 4] {
    blake3::hash(len_bytes).as_bytes()[..4].try_into().unwrap()
}

/// `decode_record` error when the length prefix fails its check — the
/// declared length is untrustworthy, so the record cannot even be
/// delimited. Log recovery treats this as interior corruption, never as a
/// torn tail (torn appends produce *short* records, not garbled headers).
pub const CORRUPT_LEN: &str = "record length check failed";

/// Decode and verify one record from the front of `buf`.
///
/// Returns `Ok(Some((frame, hash, consumed)))` on success, `Ok(None)` if
/// `buf` holds only a clean partial record (more bytes needed — at the end
/// of a log file this is a torn tail), and `Err(Corrupt)` if the record
/// fails its length check, hash verification, or decoding.
pub fn decode_record(buf: &[u8]) -> Result<Option<(Frame, FrameHash, usize)>, SodError> {
    if buf.len() < HEADER {
        return Ok(None);
    }
    let len_bytes: [u8; 4] = buf[..4].try_into().unwrap();
    if len_check(&len_bytes) != buf[4..HEADER] {
        return Err(SodError::Corrupt(CORRUPT_LEN));
    }
    let len = u32::from_le_bytes(len_bytes) as usize;
    // untrusted arithmetic: guard overflow on 32-bit targets (wasm32)
    let total = match len.checked_add(HEADER + 32) {
        Some(t) => t,
        None => return Err(SodError::Corrupt("record length overflows")),
    };
    if buf.len() < total {
        return Ok(None);
    }
    let body = &buf[HEADER..HEADER + len];
    let stored: [u8; 32] = buf[HEADER + len..total].try_into().unwrap();
    let computed = blake3::hash(body);
    if computed.as_bytes() != &stored {
        return Err(SodError::Corrupt("record hash mismatch"));
    }
    let frame: Frame =
        postcard::from_bytes(body).map_err(|_| SodError::Corrupt("frame decode failed"))?;
    Ok(Some((frame, FrameHash(stored), total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn test_frame(origin_byte: u8, seq: u64, prev: FrameHash) -> Frame {
        Frame {
            prev_hash: prev,
            origin: ReplicaId([origin_byte; 16]),
            seq,
            event_time: 1_000 + seq,
            payload: vec![(vec![1, 2, 3], 1), (vec![4, 5], -2)],
        }
    }

    #[test]
    fn frame_roundtrip() {
        let f = test_frame(7, 1, ZERO_HASH);
        let mut rec = Vec::new();
        f.encode_record(&mut rec);
        let (decoded, hash, consumed) = decode_record(&rec).unwrap().unwrap();
        assert_eq!(decoded, f);
        assert_eq!(hash, f.hash());
        assert_eq!(consumed, rec.len());
    }

    #[test]
    fn decode_partial_tail_is_none() {
        let f = test_frame(7, 1, ZERO_HASH);
        let mut rec = Vec::new();
        f.encode_record(&mut rec);
        for cut in 0..rec.len() {
            assert_eq!(decode_record(&rec[..cut]).unwrap(), None, "cut={cut}");
        }
    }

    #[test]
    fn decode_flipped_byte_is_corrupt() {
        // Every byte of the record is tamper-evident: the length prefix
        // via the length-check, the body and hash via BLAKE3. (A flipped
        // length byte must NOT read as a clean partial record — that is
        // how mid-file corruption silently truncated logs.)
        let f = test_frame(7, 1, ZERO_HASH);
        let mut rec = Vec::new();
        f.encode_record(&mut rec);
        for i in 0..rec.len() {
            let mut bad = rec.clone();
            bad[i] ^= 0xff;
            assert!(
                matches!(decode_record(&bad), Err(SodError::Corrupt(_))),
                "flip at {i}"
            );
        }
    }

    #[test]
    fn hash_chains() {
        let f1 = test_frame(7, 1, ZERO_HASH);
        let f2 = test_frame(7, 2, f1.hash());
        assert_eq!(f2.prev_hash, f1.hash());
        assert_ne!(f1.hash(), f2.hash());
    }
}
