//! The [`LogStore`] port — where a replica's frames persist — and
//! [`MemLog`], the trivial in-memory implementation.
//!
//! Sod's only durability requirement is an append-only sequence of verified
//! frame records (SOD-1); everything else about a platform's storage is the
//! implementation's business. Filesystems get
//! [`FileLog`](crate::log_file::FileLog); browsers get an OPFS/IndexedDB
//! implementation as a follow-on; tests get [`MemLog`].

use crate::{Frame, SodError};

/// Append-only frame storage.
///
/// Implementations perform torn-write recovery *at open* — by the time a
/// `LogStore` value exists, every frame it reports has already been
/// hash-verified, and a torn tail (if the platform can produce one) has
/// been discarded.
pub trait LogStore {
    /// Append one frame. Durability is governed by [`sync`](LogStore::sync).
    fn append(&mut self, frame: &Frame) -> Result<(), SodError>;

    /// Harden all appended frames against crashes (fsync or equivalent).
    fn sync(&mut self) -> Result<(), SodError>;

    /// Hand over every stored frame, in append order. Called exactly once,
    /// at [`Replica::open`](crate::Replica::open) — the replica owns the
    /// in-memory copy from then on, so implementations must not retain
    /// frames after this (that would hold every frame in memory twice).
    fn take_frames(&mut self) -> Vec<Frame>;
}

/// In-memory log: for tests, oracles, and replicas whose durability is
/// delegated elsewhere.
#[derive(Default)]
pub struct MemLog {
    frames: Vec<Frame>,
}

impl MemLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl LogStore for MemLog {
    fn append(&mut self, frame: &Frame) -> Result<(), SodError> {
        self.frames.push(frame.clone());
        Ok(())
    }

    fn sync(&mut self) -> Result<(), SodError> {
        Ok(())
    }

    fn take_frames(&mut self) -> Vec<Frame> {
        std::mem::take(&mut self.frames)
    }
}
