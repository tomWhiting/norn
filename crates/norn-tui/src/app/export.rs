//! Explicit original-byte export with caller-owned scope and no hidden transcript persistence.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Publication policy selected explicitly by the operator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportMode {
    /// Default: atomically refuse any occupied destination, including a symlink.
    #[default]
    CreateNew,
    /// Explicitly replace the destination directory entry using native rename.
    /// A final symlink is replaced, never followed to truncate its target.
    ReplaceExplicit,
}

/// What this invocation has published, not a lock against other filesystem writers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    /// This invocation has not published its bytes at the chosen destination.
    NotPublished,
    /// Complete bytes were published; a subsequent cleanup or sync may have failed.
    Published,
}

impl std::fmt::Display for Publication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotPublished => "not published",
            Self::Published => "published",
        })
    }
}

/// Synchronization actually performed; no unsupported power-loss guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportSync {
    /// The file and its parent directory accepted synchronization on Unix.
    FileAndParent,
    /// The file accepted synchronization; no portable directory sync was available.
    FileOnly,
}

/// Completed export plus the exact caller-supplied source/revision/range scope.
#[derive(Debug)]
pub struct ExportReceipt<S> {
    /// Absolute destination chosen by the operator.
    pub destination: PathBuf,
    /// Actual bytes copied from the supplied original content.
    pub bytes_written: u64,
    /// Original typed scope, including any partial/unavailable source coverage.
    /// The writer does not reinterpret this as a complete transcript.
    pub scope: S,
    /// Explicit publication choice used for this operation.
    pub mode: ExportMode,
    /// Synchronization actually completed before this receipt.
    pub synchronization: ExportSync,
}

/// Filesystem operation named by an export error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportStage {
    /// Exclusively create an owned same-directory temporary file.
    CreateTemporary,
    /// Copy original bytes into the unpublished temporary file.
    Write,
    /// Synchronize the complete temporary file before publication.
    SyncFile,
    /// Publish using a no-replace hard link or explicitly replacing rename.
    Publish,
    /// Remove the temporary name after create-new publication.
    RemoveTemporary,
    /// Synchronize the parent after publication and temporary-name cleanup.
    SyncParent,
}

impl std::fmt::Display for ExportStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CreateTemporary => "creating temporary export",
            Self::Write => "writing original bytes",
            Self::SyncFile => "synchronizing export bytes",
            Self::Publish => "publishing export",
            Self::RemoveTemporary => "removing temporary export",
            Self::SyncParent => "synchronizing export directory",
        })
    }
}

/// Located failures never quote content and preserve publication/cleanup state.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Relative or nameless destinations cannot be resolved safely in a worker.
    #[error("export destination {destination:?} is invalid: {reason}")]
    InvalidDestination {
        /// Exact path supplied by the caller.
        destination: PathBuf,
        /// Structural path problem, never original export content.
        reason: &'static str,
    },
    /// An operation failed before or after publishing the completed bytes.
    #[error(
        "export {destination:?}: {stage} failed ({publication}, temporary {temporary:?}): {source}"
    )]
    Io {
        /// Explicit operator destination.
        destination: PathBuf,
        /// Temporary pathname involved, if any; not evidence that it still exists.
        temporary: Option<PathBuf>,
        /// Operation that failed.
        stage: ExportStage,
        /// Whether this invocation already published complete destination bytes.
        publication: Publication,
        /// Original filesystem or input error.
        source: io::Error,
    },
    /// Preserve both failures and the temporary path requiring operator attention.
    #[error("{primary}; cleanup of temporary export {temporary:?} also failed: {cleanup}")]
    Cleanup {
        /// Primary error, including the actual publication state.
        #[source]
        primary: Box<ExportError>,
        /// Temporary path whose removal failed; no silent retained transcript.
        temporary: PathBuf,
        /// Cleanup failure, kept alongside rather than replacing the primary error.
        cleanup: io::Error,
    },
}

impl ExportError {
    /// Whether a failure happened after completed bytes were published.
    #[must_use]
    pub fn publication(&self) -> Publication {
        match self {
            Self::InvalidDestination { .. } => Publication::NotPublished,
            Self::Io { publication, .. } => *publication,
            Self::Cleanup { primary, .. } => primary.publication(),
        }
    }
}

/// Export only explicitly supplied original bytes, preserving all hard newlines.
///
/// The caller freshly validates its selection's source, body revision, range and
/// partial/unavailable coverage, then passes that typed scope unchanged. Resolve
/// the operator's path to an absolute destination before dispatching this blocking
/// function off the terminal event loop. No history/body reads occur here.
///
/// Cancellation: this synchronous operation runs to completion once started.
/// Dropping a worker handle does not cancel it; integration must observe the
/// result before reporting success, failure or cancellation. Before publication,
/// failure preserves the destination and attempts removal of its owned staging
/// file. A process crash may leave that named staging file; no automatic recovery
/// or background transcript persistence is installed.
///
/// A same-directory hard link publishes `CreateNew` without a check-then-write
/// race. Explicit replacement uses native rename, not compare-and-swap against
/// arbitrary writers. Final symlink targets are never opened. Parent-directory
/// resolution follows native filesystem rules and is not descriptor confinement.
/// Replacement creates a new inode; prior permissions/metadata are not copied.
/// Unix staging files request private mode 0600, subject to the operator's umask;
/// other targets use native creation rules.
/// Unsupported filesystem publication primitives fail without a fallback.
///
/// # Errors
/// Names invalid paths, staging/write/sync/publication errors and cleanup errors.
/// Publication state distinguishes failed exports from post-publication failures.
pub fn export_original<S>(
    destination: &Path,
    original: &[u8],
    mode: ExportMode,
    scope: S,
) -> Result<ExportReceipt<S>, ExportError> {
    let mut reader = original;
    export_reader(destination, &mut reader, mode, scope)
}

fn export_reader<S>(
    destination: &Path,
    original: &mut impl Read,
    mode: ExportMode,
    scope: S,
) -> Result<ExportReceipt<S>, ExportError> {
    let parent = destination_parent(destination)?;
    let temporary = parent.join(format!(".norn-export-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|source| {
        operation_error(
            destination,
            Some(&temporary),
            ExportStage::CreateTemporary,
            Publication::NotPublished,
            source,
        )
    })?;
    let written = io::copy(original, &mut file)
        .map_err(|source| {
            operation_error(
                destination,
                Some(&temporary),
                ExportStage::Write,
                Publication::NotPublished,
                source,
            )
        })
        .and_then(|bytes| {
            file.sync_all().map_err(|source| {
                operation_error(
                    destination,
                    Some(&temporary),
                    ExportStage::SyncFile,
                    Publication::NotPublished,
                    source,
                )
            })?;
            Ok(bytes)
        });
    drop(file);
    let bytes_written = match written {
        Ok(bytes) => bytes,
        Err(error) => return Err(cleanup_after_failure(error, &temporary)),
    };
    let published = match mode {
        ExportMode::CreateNew => fs::hard_link(&temporary, destination),
        ExportMode::ReplaceExplicit => fs::rename(&temporary, destination),
    };
    if let Err(source) = published {
        let error = operation_error(
            destination,
            Some(&temporary),
            ExportStage::Publish,
            Publication::NotPublished,
            source,
        );
        return Err(cleanup_after_failure(error, &temporary));
    }
    if mode == ExportMode::CreateNew {
        fs::remove_file(&temporary).map_err(|source| {
            operation_error(
                destination,
                Some(&temporary),
                ExportStage::RemoveTemporary,
                Publication::Published,
                source,
            )
        })?;
    }
    let synchronization = sync_parent(parent).map_err(|source| {
        operation_error(
            destination,
            None,
            ExportStage::SyncParent,
            Publication::Published,
            source,
        )
    })?;
    Ok(ExportReceipt {
        destination: destination.to_path_buf(),
        bytes_written,
        scope,
        mode,
        synchronization,
    })
}

fn destination_parent(destination: &Path) -> Result<&Path, ExportError> {
    let reason = if !destination.is_absolute() {
        Some("an explicit absolute path is required before worker dispatch")
    } else if destination.file_name().is_none() {
        Some("the path must name an output file")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(ExportError::InvalidDestination {
            destination: destination.to_path_buf(),
            reason,
        });
    }
    destination
        .parent()
        .ok_or_else(|| ExportError::InvalidDestination {
            destination: destination.to_path_buf(),
            reason: "the output file has no parent directory",
        })
}

fn operation_error(
    destination: &Path,
    temporary: Option<&Path>,
    stage: ExportStage,
    publication: Publication,
    source: io::Error,
) -> ExportError {
    ExportError::Io {
        destination: destination.to_path_buf(),
        temporary: temporary.map(Path::to_path_buf),
        stage,
        publication,
        source,
    }
}

fn cleanup_after_failure(primary: ExportError, temporary: &Path) -> ExportError {
    match fs::remove_file(temporary) {
        Ok(()) => primary,
        Err(cleanup) => ExportError::Cleanup {
            primary: Box::new(primary),
            temporary: temporary.to_path_buf(),
            cleanup,
        },
    }
}

fn sync_parent(parent: &Path) -> io::Result<ExportSync> {
    if cfg!(unix) {
        fs::File::open(parent)?.sync_all()?;
        Ok(ExportSync::FileAndParent)
    } else {
        // No portable directory fsync API; still surface a disappeared parent.
        let metadata = fs::metadata(parent)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "export parent is no longer a directory",
            ));
        }
        Ok(ExportSync::FileOnly)
    }
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
