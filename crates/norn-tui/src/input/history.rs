//! Persistent input history.
//!
//! [`InputHistory`] holds the chronological list of previous submissions
//! and the transient navigation state used while the user cycles through
//! them with the Up/Down arrows. When file-backed, entries persist to
//! `$NORN_HOME/history.txt` (normally `~/.norn/history.txt`) — one entry per line, with newlines and
//! backslashes escaped so multi-line submissions round-trip losslessly.
//!
//! A history with no resolvable path (the home directory could not be
//! found, or the in-memory constructor was used) still functions fully
//! for the session; it simply never touches disk.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use norn::resource::PrivateLineLog;

/// Resolve the default history file path: `~/.norn/history.txt`.
///
/// Returns `None` when the home directory cannot be resolved. Callers
/// fall back to in-memory history rather than failing — the TUI must
/// still start on a system without a resolvable `HOME` (CO5 forbids
/// `.unwrap()`/`.expect()` in library code).
#[must_use]
pub fn default_history_path() -> Option<PathBuf> {
    norn::config::paths::norn_dir().map(|root| root.join("history.txt"))
}

/// Escape a submission for single-line on-disk storage.
///
/// A backslash becomes `\\` and a newline becomes `\n`, so a multi-line
/// submission occupies exactly one physical line in the history file and
/// survives a round trip through [`decode`].
fn encode(entry: &str) -> String {
    let mut out = String::with_capacity(entry.len());
    for ch in entry.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// Decode a stored history line back into its original submission.
///
/// Inverts [`encode`]: `\\` becomes a backslash and `\n` becomes a
/// newline. An unrecognised escape (or a trailing lone backslash) is
/// preserved verbatim so no input is silently dropped.
fn decode(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Chronological input history with optional file backing.
#[derive(Default)]
pub struct InputHistory {
    /// Submitted entries, oldest first.
    entries: Vec<String>,
    /// Current navigation index into `entries`; `None` when not
    /// navigating.
    nav: Option<usize>,
    /// The editor draft saved before the first navigation step, restored
    /// when the user navigates forward past the newest entry.
    draft: Option<String>,
    /// Private backing log; `None` for in-memory-only history.
    log: Option<PrivateLineLog>,
}

impl fmt::Debug for InputHistory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InputHistory")
            .field("entry_count", &self.entries.len())
            .field("navigating", &self.nav.is_some())
            .field("draft_present", &self.draft.is_some())
            .field("backing_path", &self.log.as_ref().map(PrivateLineLog::path))
            .finish()
    }
}

impl InputHistory {
    /// Construct an in-memory history with no file backing.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load history from `path`.
    ///
    /// A missing file is normal on first run and yields an empty (but
    /// still path-backed) history. Any other I/O error is logged via
    /// `tracing::warn!` and likewise treated as empty history, so a
    /// read failure on startup is never fatal (CO5: surface or log,
    /// never silently swallow).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        let log = match PrivateLineLog::new(path) {
            Ok(log) => log,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "input history path is not safe; using in-memory history"
                );
                return Self::in_memory();
            }
        };
        let entries = match log.read_to_string() {
            Ok(contents) => contents
                .lines()
                .filter(|line| !line.is_empty())
                .map(decode)
                .collect(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to read input history; disabling disk backing"
                );
                return Self::in_memory();
            }
        };
        Self {
            entries,
            nav: None,
            draft: None,
            log: Some(log),
        }
    }

    /// Number of stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append `entry` to the history, persisting it when file-backed.
    ///
    /// Empty entries are ignored. When a backing path is set, the parent
    /// directory is created if needed and the encoded entry is appended
    /// to the file; the in-memory list is updated only after a
    /// successful write so memory and disk never diverge. I/O errors
    /// propagate to the caller (the editor's `submit`).
    pub fn append(&mut self, entry: &str) -> io::Result<()> {
        if entry.is_empty() {
            return Ok(());
        }
        if let Some(log) = &self.log {
            log.append_line(&encode(entry))?;
        }
        self.entries.push(entry.to_owned());
        Ok(())
    }

    /// Step to the previous (older) history entry.
    ///
    /// `current` is the live editor text. On the first step it is saved
    /// as the draft so a later forward step past the newest entry can
    /// restore it. Returns `None` — leaving the editor untouched — when
    /// the history is empty.
    pub fn prev(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = match self.nav {
            None => {
                self.draft = Some(current.to_owned());
                self.entries.len() - 1
            }
            Some(idx) => idx.saturating_sub(1),
        };
        self.nav = Some(idx);
        self.entries.get(idx).cloned()
    }

    /// Step to the next (newer) history entry.
    ///
    /// Advancing past the newest entry ends navigation and restores the
    /// saved draft (draining it, so a fresh draft is captured on the next
    /// [`prev`](Self::prev)). Returns `None` when not currently
    /// navigating.
    pub fn advance(&mut self) -> Option<String> {
        let idx = self.nav?;
        if idx + 1 < self.entries.len() {
            self.nav = Some(idx + 1);
            self.entries.get(idx + 1).cloned()
        } else {
            self.nav = None;
            self.draft.take()
        }
    }

    /// Reset the navigation cursor.
    ///
    /// Called after the editor buffer is replaced out-of-band (a submit
    /// or a clear) so that the next [`prev`](Self::prev) re-captures the
    /// draft from the current buffer rather than navigating relative to a
    /// stale position. The saved draft is left intact; it is harmlessly
    /// overwritten on the next `prev`.
    pub fn cancel_navigation(&mut self) {
        self.nav = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trips_multiline_and_backslash() {
        let original = "line one\nline\\two\nend";
        let encoded = encode(original);
        assert!(!encoded.contains('\n'), "encoded form must be single-line");
        assert_eq!(decode(&encoded), original);
    }

    #[test]
    fn prev_returns_newest_first_then_walks_back() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("a")?;
        history.append("b")?;
        assert_eq!(history.prev(""), Some("b".to_string()));
        assert_eq!(history.prev(""), Some("a".to_string()));
        // Saturates at the oldest entry.
        assert_eq!(history.prev(""), Some("a".to_string()));
        Ok(())
    }

    #[test]
    fn prev_on_empty_history_returns_none() {
        let mut history = InputHistory::in_memory();
        assert_eq!(history.prev("draft"), None);
    }

    #[test]
    fn next_past_newest_restores_draft() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("a")?;
        history.append("b")?;
        // One step back, then forward past the newest entry.
        assert_eq!(history.prev("my draft"), Some("b".to_string()));
        assert_eq!(history.advance(), Some("my draft".to_string()));
        Ok(())
    }

    #[test]
    fn next_walks_forward_then_restores_draft() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("a")?;
        history.append("b")?;
        assert_eq!(history.prev("draft"), Some("b".to_string()));
        assert_eq!(history.prev("draft"), Some("a".to_string()));
        assert_eq!(history.advance(), Some("b".to_string()));
        assert_eq!(history.advance(), Some("draft".to_string()));
        // Draft was drained — no further forward movement.
        assert_eq!(history.advance(), None);
        Ok(())
    }

    #[test]
    fn next_without_navigation_is_a_noop() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("a")?;
        assert_eq!(history.advance(), None);
        Ok(())
    }

    #[test]
    fn cancel_navigation_lets_prev_recapture_the_draft() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("a")?;
        assert_eq!(history.prev("first draft"), Some("a".to_string()));
        history.cancel_navigation();
        assert_eq!(history.prev("second draft"), Some("a".to_string()));
        assert_eq!(history.advance(), Some("second draft".to_string()));
        Ok(())
    }

    #[test]
    fn append_ignores_empty_entries() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("")?;
        assert!(history.is_empty());
        Ok(())
    }

    #[test]
    fn load_from_missing_file_is_empty_and_path_backed() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("history.txt");
        let mut history = InputHistory::load_from(&path);
        assert!(history.is_empty());
        // Path-backed: the first append creates the file.
        history.append("created")?;
        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn round_trip_persists_entries_in_chronological_order() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("history.txt");

        let mut history = InputHistory::load_from(&path);
        history.append("first")?;
        history.append("second\nwith newline")?;
        history.append("third")?;
        drop(history);

        let reloaded = InputHistory::load_from(&path);
        assert_eq!(
            reloaded.entries,
            vec![
                "first".to_string(),
                "second\nwith newline".to_string(),
                "third".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn append_creates_parent_directory() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested").join("dir").join("history.txt");
        let mut history = InputHistory::load_from(&path);
        history.append("entry")?;
        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn corrupt_utf8_disables_disk_backing() -> io::Result<()> {
        use std::fs;

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("history.txt");
        fs::write(&path, [0xff, b'\n'])?;

        let mut history = InputHistory::load_from(&path);
        assert!(history.log.is_none());
        history.append("memory only")?;
        assert_eq!(fs::read(&path)?, [0xff, b'\n']);
        Ok(())
    }

    #[test]
    fn debug_omits_entries_and_draft() -> io::Result<()> {
        let mut history = InputHistory::in_memory();
        history.append("history-secret")?;
        let _ = history.prev("draft-secret");

        let debug = format!("{history:?}");

        assert!(!debug.contains("history-secret"));
        assert!(!debug.contains("draft-secret"));
        assert!(debug.contains("entry_count"));
        Ok(())
    }
}
