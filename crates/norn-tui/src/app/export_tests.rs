//! Export safety and scope assertions; no runtime, clipboard or provider work.

use std::fs;
use std::io::{self, Read};
use std::ops::Range;
use std::path::Path;

use super::{
    ExportError, ExportMode, ExportStage, Publication, cleanup_after_failure, export_original,
    export_reader, operation_error,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, Eq)]
struct Scope {
    source: String,
    revision: u64,
    range: Range<usize>,
    unavailable: Vec<String>,
}

fn no_temporary_files(parent: &Path) -> TestResult {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        assert!(
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".norn-export-")
        );
    }
    Ok(())
}

#[test]
fn original_unicode_hard_newlines_and_partial_scope_are_exported_exactly() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("chosen.txt");
    let original = "original\r\n👩‍💻 e\u{301}\n\tcontent\u{1b}[2J".as_bytes();
    let scope = Scope {
        source: "selected-body".to_owned(),
        revision: 7,
        range: 10..10 + original.len(),
        unavailable: vec!["older history not requested".to_owned()],
    };
    let receipt = export_original(&destination, original, ExportMode::default(), scope)?;
    assert_eq!(receipt.destination, destination);
    assert_eq!(receipt.mode, ExportMode::CreateNew);
    assert_eq!(receipt.bytes_written, u64::try_from(original.len())?);
    assert!(
        fs::read(&destination)? == original,
        "original bytes were transformed"
    );
    assert_eq!(receipt.scope.source, "selected-body");
    assert_eq!(receipt.scope.revision, 7);
    assert_eq!(receipt.scope.range, 10..10 + original.len());
    assert_eq!(
        receipt.scope.unavailable,
        vec!["older history not requested".to_owned()]
    );
    no_temporary_files(directory.path())
}

#[test]
fn create_new_refuses_existing_destination_without_changing_original_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("existing");
    fs::write(&destination, b"original")?;
    let error = export_original(&destination, b"replacement", ExportMode::default(), ())
        .err()
        .ok_or("existing destination was overwritten")?;
    assert_eq!(error.publication(), Publication::NotPublished);
    assert!(
        matches!(&error, ExportError::Io { stage:ExportStage::Publish, source, .. } if source.kind() == io::ErrorKind::AlreadyExists)
    );
    assert!(
        fs::read(&destination)? == b"original",
        "existing bytes changed"
    );
    assert!(error.to_string().contains("existing"));
    no_temporary_files(directory.path())
}

#[test]
fn explicit_replace_publishes_new_inode_without_truncating_other_hard_links() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("replace");
    let alias = directory.path().join("original-alias");
    fs::write(&destination, b"old")?;
    fs::hard_link(&destination, &alias)?;
    let receipt = export_original(
        &destination,
        b"new",
        ExportMode::ReplaceExplicit,
        "explicit replacement",
    )?;
    assert_eq!(receipt.bytes_written, 3);
    assert!(
        fs::read(&destination)? == b"new",
        "replacement was not published"
    );
    assert!(fs::read(&alias)? == b"old", "existing inode was truncated");
    no_temporary_files(directory.path())
}

#[test]
fn empty_export_is_a_real_zero_byte_receipt() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("empty");
    let receipt = export_original(
        &destination,
        b"",
        ExportMode::CreateNew,
        "empty selected range",
    )?;
    assert_eq!(receipt.bytes_written, 0);
    assert_eq!(fs::metadata(&destination)?.len(), 0);
    no_temporary_files(directory.path())
}

#[test]
fn relative_destination_is_rejected_before_creating_any_file() -> TestResult {
    let destination = Path::new("relative-export-must-not-be-created");
    let error = export_original(destination, b"data", ExportMode::CreateNew, ())
        .err()
        .ok_or("relative destination accepted")?;
    assert!(matches!(error, ExportError::InvalidDestination { .. }));
    assert_eq!(error.publication(), Publication::NotPublished);
    Ok(())
}

#[test]
fn missing_parent_is_reported_and_never_created_implicitly() -> TestResult {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("not-created");
    let destination = missing.join("export");
    let error = export_original(&destination, b"data", ExportMode::CreateNew, ())
        .err()
        .ok_or("missing parent accepted")?;
    assert!(
        matches!(&error, ExportError::Io { stage:ExportStage::CreateTemporary, source, .. } if source.kind() == io::ErrorKind::NotFound)
    );
    assert!(!missing.exists());
    assert_eq!(error.publication(), Publication::NotPublished);
    Ok(())
}

struct FailingOriginal {
    delivered_prefix: bool,
}

impl Read for FailingOriginal {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.delivered_prefix {
            return Err(io::Error::other("fixture source failed after a prefix"));
        }
        buffer[0] = b'x';
        self.delivered_prefix = true;
        Ok(1)
    }
}

#[test]
fn staging_failure_removes_partial_temp_and_preserves_existing_destination() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("unchanged");
    fs::write(&destination, b"original")?;
    let mut original = FailingOriginal {
        delivered_prefix: false,
    };
    let error = export_reader(&destination, &mut original, ExportMode::ReplaceExplicit, ())
        .err()
        .ok_or("failed source was published")?;
    assert!(matches!(
        &error,
        ExportError::Io {
            stage: ExportStage::Write,
            ..
        }
    ));
    assert_eq!(error.publication(), Publication::NotPublished);
    assert!(
        fs::read(&destination)? == b"original",
        "failed staging changed destination"
    );
    no_temporary_files(directory.path())
}

#[test]
fn staging_failure_for_new_export_never_leaves_partial_destination() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("not-published");
    let mut original = FailingOriginal {
        delivered_prefix: false,
    };
    let error = export_reader(&destination, &mut original, ExportMode::CreateNew, ())
        .err()
        .ok_or("partial source was published")?;
    assert_eq!(error.publication(), Publication::NotPublished);
    assert!(!destination.exists());
    no_temporary_files(directory.path())
}

#[test]
fn simultaneous_create_new_has_one_winner_without_overwrite() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("one-winner");
    let barrier = std::sync::Barrier::new(2);
    let (left, right) = std::thread::scope(|workers| -> Result<_, io::Error> {
        let left = workers.spawn(|| {
            barrier.wait();
            export_original(&destination, b"left", ExportMode::CreateNew, "left")
        });
        let right = workers.spawn(|| {
            barrier.wait();
            export_original(&destination, b"right", ExportMode::CreateNew, "right")
        });
        let left = left.join();
        let right = right.join();
        Ok((
            left.map_err(|payload| worker_panic(payload.as_ref()))?,
            right.map_err(|payload| worker_panic(payload.as_ref()))?,
        ))
    })?;
    let (winner, refused) = match (left, right) {
        (Ok(winner), Err(refused)) | (Err(refused), Ok(winner)) => (winner, refused),
        other => return Err(format!("expected one exclusive export winner, got {other:?}").into()),
    };
    assert!(
        matches!(refused, ExportError::Io { stage:ExportStage::Publish, source, .. } if source.kind() == io::ErrorKind::AlreadyExists)
    );
    assert!(
        fs::read(&destination)? == winner.scope.as_bytes(),
        "winning bytes were overwritten"
    );
    no_temporary_files(directory.path())
}

fn worker_panic(payload: &(dyn std::any::Any + Send)) -> io::Error {
    let message = if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else {
        format!("non-string panic payload {:?}", payload.type_id())
    };
    io::Error::other(format!("export fixture worker panicked: {message}"))
}

#[test]
fn publication_failure_cleans_temp_without_removing_destination_directory() -> TestResult {
    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("directory-target");
    fs::create_dir(&destination)?;
    let error = export_original(&destination, b"data", ExportMode::ReplaceExplicit, ())
        .err()
        .ok_or("directory was replaced by an export")?;
    assert!(matches!(
        &error,
        ExportError::Io {
            stage: ExportStage::Publish,
            ..
        }
    ));
    assert_eq!(error.publication(), Publication::NotPublished);
    assert!(destination.is_dir());
    no_temporary_files(directory.path())
}

#[test]
fn cleanup_failure_retains_primary_error_and_names_unremoved_path() -> TestResult {
    let directory = tempfile::tempdir()?;
    let temporary = directory.path().join("cannot-remove-as-file");
    fs::create_dir(&temporary)?;
    let destination = directory.path().join("destination");
    let primary = operation_error(
        &destination,
        Some(&temporary),
        ExportStage::Write,
        Publication::NotPublished,
        io::Error::other("fixture primary write failure"),
    );
    let error = cleanup_after_failure(primary, &temporary);
    assert_eq!(error.publication(), Publication::NotPublished);
    let rendered = error.to_string();
    assert!(rendered.contains("fixture primary write failure"));
    assert!(rendered.contains("cannot-remove-as-file"));
    assert!(matches!(error, ExportError::Cleanup { .. }));
    assert!(temporary.is_dir());
    Ok(())
}

#[cfg(unix)]
#[test]
fn create_new_refuses_live_and_dangling_symlinks_and_explicit_replace_preserves_target()
-> TestResult {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let target = directory.path().join("symlink-target");
    fs::write(&target, b"target original")?;
    for name in ["live-link", "dangling-link"] {
        let destination = directory.path().join(name);
        let link_target = if name == "live-link" {
            target.clone()
        } else {
            directory.path().join("absent-target")
        };
        symlink(&link_target, &destination)?;
        let error = export_original(&destination, b"new", ExportMode::CreateNew, ())
            .err()
            .ok_or("create-new followed a symlink")?;
        assert_eq!(error.publication(), Publication::NotPublished);
        assert_eq!(fs::read_link(&destination)?, link_target);
        let receipt = export_original(
            &destination,
            b"replacement",
            ExportMode::ReplaceExplicit,
            (),
        )?;
        assert_eq!(receipt.bytes_written, 11);
        assert!(fs::symlink_metadata(&destination)?.is_file());
    }
    assert!(
        fs::read(&target)? == b"target original",
        "symlink target was altered"
    );
    assert!(!directory.path().join("absent-target").exists());
    no_temporary_files(directory.path())
}

#[cfg(unix)]
#[test]
fn unix_export_is_private_and_reports_file_and_parent_sync() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let destination = directory.path().join("private");
    let receipt = export_original(&destination, b"data", ExportMode::CreateNew, ())?;
    assert_eq!(fs::metadata(&destination)?.permissions().mode() & 0o177, 0);
    assert_eq!(receipt.synchronization, super::ExportSync::FileAndParent);
    no_temporary_files(directory.path())
}
