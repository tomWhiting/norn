//! Type definitions for the always-on `NORN.md` context layer.
//!
//! A [`ContextFile`] is a passive data record describing one file on
//! disk: its path, its full text content, and the modification time
//! observed when it was read. The mtime is the snapshot the later
//! staleness check (NX-002) will compare against; it is captured here
//! so the load and the stat happen in a single observation.

use std::path::PathBuf;
use std::time::SystemTime;

/// A single `NORN.md` context file that has been read into memory.
///
/// Constructed by [`crate::context::loader::ContextLoader`]; never
/// constructed elsewhere. The struct intentionally exposes its three
/// fields as public — the loader populates them in one place, every
/// other consumer reads them, and the rules engine has no equivalent
/// "private fields plus accessors" precedent.
#[derive(Clone, Debug)]
pub struct ContextFile {
    /// Absolute path the file was read from.
    ///
    /// Stored so callers (and the staleness check in NX-002) can re-stat
    /// or re-read the same file without having to reconstruct the path
    /// from `~/.norn/` plus the filename.
    pub path: PathBuf,
    /// Full UTF-8 text of the file, exactly as read from disk.
    ///
    /// No trimming, no normalisation, no separator munging — the
    /// loader's [`crate::context::loader::ContextLoader::formatted_context`]
    /// adds the inter-file separator when concatenating.
    pub content: String,
    /// Modification timestamp observed when [`Self::content`] was read.
    ///
    /// `None` only when the platform did not report an mtime (extremely
    /// rare — most filesystems supply one). NX-002 treats a missing
    /// mtime as "always re-read on next iteration" so the consumer
    /// never has to silently mishandle this case.
    pub mtime: Option<SystemTime>,
}
