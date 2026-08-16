//! [`FileLog`]: the filesystem [`LogStore`] with torn-tail recovery.
//!
//! One append-only file of records (`len | body | blake3`). On open the
//! whole file is scanned and verified:
//!
//! - A clean partial record at the *end* of the file is a torn tail — the
//!   result of a crash mid-append — and is truncated away (SOD-5).
//! - A record that fails hash verification *followed by nothing* is treated
//!   the same way (the crash corrupted the tail).
//! - A bad record followed by further valid data is **corruption**, not a
//!   torn tail: `open` refuses with [`SodError::Corrupt`] rather than
//!   silently dropping interior data.
//!
//! v1 keeps every frame in memory as well as on disk (the replica serves
//! sync suffixes from memory); log compaction and streaming reads are
//! future work recorded in the spec.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::frame::decode_record;
use crate::store::LogStore;
use crate::{Frame, SodError};

pub struct FileLog {
    file: File,
    frames: Vec<Frame>,
}

fn io_err(e: std::io::Error) -> SodError {
    SodError::Io(e.to_string())
}

impl FileLog {
    /// Open (or create) the log at `path`, scanning, verifying, and
    /// recovering as described in the module docs.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SodError> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io_err(e)),
        };

        let mut frames = Vec::new();
        let mut pos = 0usize;
        let mut valid_end = 0usize;
        while pos < bytes.len() {
            match decode_record(&bytes[pos..]) {
                Ok(Some((frame, _hash, consumed))) => {
                    frames.push(frame);
                    pos += consumed;
                    valid_end = pos;
                }
                // Clean partial tail: recoverable iff nothing follows —
                // and by definition nothing does (it consumed the rest).
                Ok(None) => break,
                Err(_) => {
                    // A complete-but-bad record. Torn tail only if we can
                    // prove nothing valid follows it; scanning forward for
                    // a later valid record is ambiguous (record boundaries
                    // are length-prefixed), so the rule is: bad record at
                    // the last position we reached = torn tail, anything
                    // recoverable beyond it would have decoded. We treat a
                    // bad final record as torn; decode_record already
                    // distinguished "short" from "bad", and a bad record
                    // means its declared length fit within the file. If
                    // more bytes remain past that declared length, this is
                    // interior corruption: refuse.
                    let len =
                        u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
                    let declared_end = pos + 4 + len + 32;
                    if declared_end < bytes.len() {
                        return Err(SodError::Corrupt(
                            "interior log corruption (bad record followed by data)",
                        ));
                    }
                    break;
                }
            }
        }

        if valid_end < bytes.len() {
            // Torn tail: restore the file to the last valid record boundary.
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(io_err)?;
            file.set_len(valid_end as u64).map_err(io_err)?;
            file.sync_data().map_err(io_err)?;
        }

        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(io_err)?;
        Ok(FileLog { file, frames })
    }
}

impl LogStore for FileLog {
    fn append(&mut self, frame: &Frame) -> Result<(), SodError> {
        let mut rec = Vec::new();
        frame.encode_record(&mut rec);
        self.file.write_all(&rec).map_err(io_err)?;
        self.frames.push(frame.clone());
        Ok(())
    }

    fn sync(&mut self) -> Result<(), SodError> {
        self.file.sync_data().map_err(io_err)
    }

    fn frames(&self) -> &[Frame] {
        &self.frames
    }
}
