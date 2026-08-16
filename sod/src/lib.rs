//! Sod: symmetric replication for fold apps.
//!
//! A **sod** is a replica: a local database that always accepts writes and
//! converges with its peers by exchanging what the other is missing. There
//! are no client or server roles on the wire — client↔server is a star of
//! pairwise symmetric sessions, p2p is any other graph of the same sessions.
//!
//! Convergence comes from algebra, not coordination: the write primitive is
//! a Z-set delta (datum + signed multiplicity), deltas commute, and views
//! are deterministic functions of the resulting multiset. Replicas that
//! hold the same set of frames hold the same views.
//!
//! Everything platform- or engine-specific enters through a port:
//!
//! - [`engine::Engine`] — "the bog machinery" that materializes deltas.
//!   [`engine::MemEngine`] is the always-compiled oracle;
//!   `FoldEngine` (feature `fold-engine`) wraps a fold `Stream`.
//! - [`store::LogStore`] — the append-only frame log.
//!   [`store::MemLog`] always; [`log_file::FileLog`] on filesystems.
//! - transports (feature `ws`) drive the sans-io [`sync::Session`].
//!
//! Design spec: `docs/superpowers/specs/2026-08-15-sod-design.md`.

pub mod engine;
#[cfg(feature = "fold-engine")]
pub mod engine_fold;
pub mod frame;
pub mod log_file;
pub mod replica;
pub mod store;
pub mod sync;
pub mod time;
pub mod transport;
pub mod vector;

pub use frame::{Frame, FrameHash, ReplicaId, ZERO_HASH, decode_record};
pub use replica::Replica;
pub use sync::{Msg, PROTOCOL_VERSION, Session, sync_pair};
pub use vector::VersionVector;

/// Errors across sod's ports and protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SodError {
    /// A record or frame failed structural or hash verification.
    Corrupt(&'static str),
    /// A frame's seq is not contiguous with the feed we hold.
    Gap {
        origin: ReplicaId,
        have: u64,
        got: u64,
    },
    /// Two distinct frames claimed the same `(origin, seq)` (SOD-2).
    Equivocation { origin: ReplicaId, seq: u64 },
    /// The origin's feed was previously poisoned; its frames are refused.
    Poisoned(ReplicaId),
    /// Sync handshake refused: protocol or schema version differs (SOD-9).
    VersionMismatch {
        ours: (u16, u32),
        theirs: (u16, u32),
    },
    /// An I/O error from a LogStore or transport.
    Io(String),
}

impl core::fmt::Display for SodError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SodError::Corrupt(what) => write!(f, "corrupt record: {what}"),
            SodError::Gap { origin, have, got } => {
                write!(f, "gap in feed {origin}: have {have}, got {got}")
            }
            SodError::Equivocation { origin, seq } => {
                write!(f, "equivocation in feed {origin} at seq {seq}")
            }
            SodError::Poisoned(origin) => write!(f, "feed {origin} is poisoned"),
            SodError::VersionMismatch { ours, theirs } => write!(
                f,
                "version mismatch: ours protocol {}/schema {}, theirs protocol {}/schema {}",
                ours.0, ours.1, theirs.0, theirs.1
            ),
            SodError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for SodError {}
