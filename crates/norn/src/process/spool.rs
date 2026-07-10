//! File-backed output spool for a managed background process.
//!
//! Each managed process spools its stdout and stderr into a **single
//! arrival-ordered on-disk file**, one line per write, each line tagged with
//! the stream it came from. There is deliberately **no size cap and no
//! truncation** — a background process lives until it exits or is killed
//! (owner ruling), and its whole output is retained on disk. Model-facing
//! reads are budgeted at the tool layer (the `process` tool), never here.
//!
//! Reads are incremental: a [`SpoolReader`] holds a byte cursor and
//! [`SpoolReader::read_new`] returns exactly the bytes appended since that
//! cursor. Multiple readers hold independent cursors and never affect each
//! other — the spool file is append-only, so each read simply opens the file,
//! seeks to its own cursor, and reads up to the committed length.
//!
//! The spool publishes a **committed-length watch** ([`Spool::subscribe_len`]):
//! a `tokio::sync::watch` channel of the number of bytes durably appended so
//! far, so a subscriber learns of new output without polling. This is the
//! watch attach seam NP-002's watches consume — this brief only exposes it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::watch;

use crate::util::PrivateRoot;

/// Which standard stream a spooled line originated from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTag {
    /// The process's standard output.
    Stdout,
    /// The process's standard error.
    Stderr,
}

impl StreamTag {
    /// The stable, greppable line-prefix token for this stream.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "out",
            Self::Stderr => "err",
        }
    }
}

/// A single arrival-ordered, append-only, stream-tagged output file for one
/// managed process.
///
/// Writes from the stdout and stderr drains are serialised through an async
/// mutex over the file handle, so interleaved lines land in strict arrival
/// order. After every write the file is flushed and the committed-length
/// counter advanced, so any reader that reads up to the committed length only
/// ever sees fully-flushed bytes.
#[derive(Debug)]
pub struct Spool {
    /// Absolute path of the backing file.
    path: PathBuf,
    root: Arc<PrivateRoot>,
    relative: PathBuf,
    /// The write side: the open file, serialised across the two drains.
    file: AsyncMutex<File>,
    /// Bytes durably appended and flushed so far. Monotonic, unbounded.
    committed: AtomicU64,
    /// Committed-length publisher — the watch attach seam (R2/R8).
    len_tx: watch::Sender<u64>,
}

impl Spool {
    /// Create the spool file at `path`, creating parent directories.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from creating the directory tree or the file.
    pub async fn create(path: PathBuf) -> std::io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "process spool must have an absolute parent directory",
            )
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "process spool path has no final component",
            )
        })?;
        Self::create_in(parent, Path::new(file_name)).await
    }

    /// Create a spool under an absolute private root and relative file name.
    pub(crate) async fn create_in(root_path: &Path, relative: &Path) -> std::io::Result<Self> {
        let root_path = root_path.to_path_buf();
        let relative = relative.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let root = Arc::new(PrivateRoot::create(&root_path)?);
            Self::create_under_sync(root, &relative)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    pub(crate) async fn create_under(
        root: Arc<PrivateRoot>,
        relative: &Path,
    ) -> std::io::Result<Self> {
        let relative = relative.to_path_buf();
        tokio::task::spawn_blocking(move || Self::create_under_sync(root, &relative))
            .await
            .map_err(std::io::Error::other)?
    }

    fn create_under_sync(root: Arc<PrivateRoot>, relative: &Path) -> std::io::Result<Self> {
        if let Some(parent) = relative.parent() {
            root.create_dir_all(parent)?;
        }
        let file = File::from_std(root.create_new(relative)?);
        let (len_tx, _len_rx) = watch::channel(0);
        Ok(Self {
            path: root.display_path(relative),
            root,
            relative: relative.to_path_buf(),
            file: AsyncMutex::new(file),
            committed: AtomicU64::new(0),
            len_tx,
        })
    }

    /// The absolute path of the backing file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes committed (written and flushed) so far. Never decreases and is
    /// never capped.
    #[must_use]
    pub fn committed_len(&self) -> u64 {
        self.committed.load(Ordering::Acquire)
    }

    /// The spool path rendered for display: `~/…` when it lies under the
    /// home directory, otherwise the absolute path.
    #[must_use]
    pub fn display_path(&self) -> String {
        if let Some(home) = crate::config::paths::trusted_home_dir()
            && let Ok(stripped) = self.path.strip_prefix(&home)
        {
            return format!("~/{}", stripped.to_string_lossy());
        }
        self.path.to_string_lossy().into_owned()
    }

    /// Append one already-tagged, newline-terminated content chunk under
    /// `tag`. Exactly one trailing newline is guaranteed.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from writing or flushing the file.
    pub async fn append_tagged(&self, tag: StreamTag, content: &str) -> std::io::Result<()> {
        let trimmed = content.strip_suffix('\n').unwrap_or(content);
        let line = format!("{} {trimmed}\n", tag.as_str());
        let bytes = line.into_bytes();
        let mut file = self.file.lock().await;
        file.write_all(&bytes).await?;
        file.flush().await?;
        // Advance and publish under the file lock so the committed length and
        // the on-disk bytes never disagree for a reader racing the watch.
        let new_len = self
            .committed
            .fetch_add(bytes.len() as u64, Ordering::AcqRel)
            .saturating_add(bytes.len() as u64);
        // `send_replace`, not `send`: a tokio watch `send` fails and stores
        // nothing when every receiver has been dropped, so a subscriber that
        // attaches later (e.g. after this process has already exited) would
        // read the stale initial `0` and never observe the appended region.
        // `send_replace` stores the committed length unconditionally, so a late
        // `subscribe_len()` borrows the true length at once.
        self.len_tx.send_replace(new_len);
        Ok(())
    }

    /// Append raw bytes verbatim (no stream tag). Used to seed a spool with
    /// output captured before a foreground bash command migrated to the
    /// background — the historical bytes are preserved exactly, and tagged
    /// lines follow.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from writing or flushing the file.
    pub async fn append_raw(&self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = self.file.lock().await;
        file.write_all(bytes).await?;
        file.flush().await?;
        let new_len = self
            .committed
            .fetch_add(bytes.len() as u64, Ordering::AcqRel)
            .saturating_add(bytes.len() as u64);
        // `send_replace` for the same reason as `append_tagged`: store the
        // committed length unconditionally so a late subscriber sees it.
        self.len_tx.send_replace(new_len);
        Ok(())
    }

    /// Read every byte appended since `cursor`, up to the current committed
    /// length. Returns the bytes and the new cursor (the committed length at
    /// read time). An empty result means no new output.
    ///
    /// The spool is append-only, so this opens the file fresh, seeks to
    /// `cursor`, and reads the committed region — independent of any other
    /// reader's cursor.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from opening, seeking, or reading the file.
    pub async fn read_from(&self, cursor: u64) -> std::io::Result<(Vec<u8>, u64)> {
        let committed = self.committed_len();
        let bytes = self.read_range(cursor, committed).await?;
        Ok((bytes, committed))
    }

    /// Read exactly the byte range `[start, end)` from the spool. Unlike
    /// [`Self::read_from`], the upper bound is an explicit argument rather than
    /// the live committed length, so the returned region is deterministic even
    /// when the spool grows concurrently.
    ///
    /// This is the primitive a caller uses when it has already decided the
    /// region under a lock (the model-cursor advance in
    /// [`ProcessManager::model_output`](super::manager::ProcessManager::model_output)):
    /// two racing readers pick disjoint `[start, end)` windows and this method
    /// reads each without ever re-consulting the committed length. `end <=
    /// start` yields an empty result. The spool is append-only, so any `end`
    /// not exceeding a committed length is stable to read.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from opening, seeking, or reading the file.
    pub async fn read_range(&self, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;
        if end <= start {
            return Ok(Vec::new());
        }
        let to_read = end - start;
        let mut file = File::from_std(self.root.open_read(&self.relative)?);
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut buf = vec![
            0_u8;
            usize::try_from(to_read).map_err(|e| std::io::Error::other(format!(
                "spool region ({to_read} bytes) exceeds addressable memory: {e}"
            )))?
        ];
        file.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Subscribe to the committed-length watch: a receiver that observes each
    /// new committed length as output is appended. The watch attach seam
    /// (R2/R8) — no polling required.
    #[must_use]
    pub fn subscribe_len(&self) -> watch::Receiver<u64> {
        self.len_tx.subscribe()
    }
}

/// An independent incremental reader over a [`Spool`].
///
/// Holds a private byte cursor; [`Self::read_new`] returns exactly the bytes
/// appended since the last read and advances the cursor. Constructing several
/// readers over one spool gives each an independent view — one reader's reads
/// never consume another's unread region.
pub struct SpoolReader {
    spool: std::sync::Arc<Spool>,
    cursor: u64,
}

impl SpoolReader {
    /// Create a reader positioned at the start of the spool.
    #[must_use]
    pub fn new(spool: std::sync::Arc<Spool>) -> Self {
        Self { spool, cursor: 0 }
    }

    /// The reader's current byte cursor.
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Read and return the bytes appended since the last read, advancing the
    /// cursor. Returns an empty vector when there is no new output.
    ///
    /// # Errors
    ///
    /// Returns any I/O error propagated from [`Spool::read_from`].
    pub async fn read_new(&mut self) -> std::io::Result<Vec<u8>> {
        let (bytes, new_cursor) = self.spool.read_from(self.cursor).await?;
        self.cursor = new_cursor;
        Ok(bytes)
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn process_spool_is_private_and_refuses_final_links()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let container = tempfile::tempdir()?;
        let root = container.path().join("spools");
        let path = root.join("run/p1.log");
        let spool = Spool::create(path.clone()).await?;
        spool.append_raw(b"private").await?;
        assert_eq!(
            std::fs::metadata(&root)?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("run"))?.permissions().mode() & 0o777,
            0o700,
        );
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );

        let target = container.path().join("outside.log");
        std::fs::write(&target, "outside")?;
        let linked = root.join("run/p2.log");
        symlink(&target, &linked)?;
        assert!(Spool::create(linked).await.is_err());
        assert_eq!(std::fs::read_to_string(target)?, "outside");
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use std::sync::Arc;

    use super::*;

    async fn temp_spool() -> (tempfile::TempDir, Arc<Spool>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p1.log");
        let spool = Arc::new(Spool::create(path).await.unwrap());
        (dir, spool)
    }

    #[tokio::test]
    async fn interleaved_writes_are_arrival_ordered_and_tagged() {
        let (_dir, spool) = temp_spool().await;
        spool
            .append_tagged(StreamTag::Stdout, "first")
            .await
            .unwrap();
        spool
            .append_tagged(StreamTag::Stderr, "second")
            .await
            .unwrap();
        spool
            .append_tagged(StreamTag::Stdout, "third")
            .await
            .unwrap();

        let (bytes, _) = spool.read_from(0).await.unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "out first\nerr second\nout third\n");
    }

    #[tokio::test]
    async fn two_readers_hold_independent_cursors() {
        let (_dir, spool) = temp_spool().await;
        // Write exactly 100 bytes: ten "out xxxxx\n" lines are 10 bytes each.
        for _ in 0..10 {
            spool
                .append_tagged(StreamTag::Stdout, "xxxxx")
                .await
                .unwrap();
        }
        assert_eq!(spool.committed_len(), 100);

        let mut reader_a = SpoolReader::new(Arc::clone(&spool));
        let mut reader_b = SpoolReader::new(Arc::clone(&spool));

        let a = reader_a.read_new().await.unwrap();
        assert_eq!(a.len(), 100, "A consumes all 100 bytes");
        assert_eq!(reader_a.cursor(), 100);

        // B's cursor is untouched by A's read.
        let b = reader_b.read_new().await.unwrap();
        assert_eq!(
            b.len(),
            100,
            "B still sees the same 100 bytes from its own cursor"
        );

        // A has no new output; B is now caught up too.
        assert!(reader_a.read_new().await.unwrap().is_empty());
        assert!(reader_b.read_new().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn committed_length_watch_wakes_a_subscriber_without_polling() {
        let (_dir, spool) = temp_spool().await;
        let mut len_rx = spool.subscribe_len();
        let mut reader = SpoolReader::new(Arc::clone(&spool));

        let writer = {
            let spool = Arc::clone(&spool);
            tokio::spawn(async move {
                spool
                    .append_tagged(StreamTag::Stdout, "hello")
                    .await
                    .unwrap();
            })
        };

        // Await the watch — no polling loop. `changed` resolves when the
        // committed length advances.
        len_rx.changed().await.unwrap();
        writer.await.unwrap();
        let appended = reader.read_new().await.unwrap();
        assert_eq!(String::from_utf8(appended).unwrap(), "out hello\n");
    }

    #[tokio::test]
    async fn no_size_cap_grows_unbounded() {
        let (_dir, spool) = temp_spool().await;
        for _ in 0..2000 {
            spool
                .append_tagged(StreamTag::Stdout, "a line of unremarkable output")
                .await
                .unwrap();
        }
        // The only counter is the (unbounded) committed length — no cap,
        // no truncation.
        assert!(spool.committed_len() > 50_000);
    }
}
