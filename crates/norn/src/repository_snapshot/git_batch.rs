//! Interactive `git cat-file --batch` blob acquisition.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::process::Stdio;
use std::sync::Arc;

use norn_policy::GitObjectId;

use super::error::{GitOperation, SnapshotAdapterError};
use super::git::{BaseTreeRecord, GitRunner};

/// Immutable bytes keyed by their verified Git object identity.
pub(super) struct BlobStore {
    blobs: BTreeMap<GitObjectId, Arc<[u8]>>,
}

impl BlobStore {
    pub(super) fn acquire(
        runner: &GitRunner,
        records: &[BaseTreeRecord],
    ) -> Result<Self, SnapshotAdapterError> {
        let object_ids = records
            .iter()
            .map(|record| record.object_id.clone())
            .collect::<BTreeSet<_>>();
        let blobs = read_batch(runner, &object_ids)?;
        Ok(Self { blobs })
    }

    pub(super) fn get(&self, object_id: &GitObjectId) -> Option<&Arc<[u8]>> {
        self.blobs.get(object_id)
    }
}

fn read_batch(
    runner: &GitRunner,
    object_ids: &BTreeSet<GitObjectId>,
) -> Result<BTreeMap<GitObjectId, Arc<[u8]>>, SnapshotAdapterError> {
    let permit = crate::resource::DescriptorGovernor::global()?
        .try_acquire(crate::resource::OUTPUT_SUBPROCESS_PEAK)?;
    let mut child = runner
        .command()
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| SnapshotAdapterError::git_spawn(GitOperation::ReadBaseBlobs, &error))?;
    let Some(stdin) = child.stdin.take() else {
        let status = child.wait().map_err(|error| {
            SnapshotAdapterError::git_spawn(GitOperation::ReadBaseBlobs, &error)
        })?;
        return if status.success() {
            Err(SnapshotAdapterError::GitProtocol(
                GitOperation::ReadBaseBlobs,
            ))
        } else {
            Err(SnapshotAdapterError::GitExit(GitOperation::ReadBaseBlobs))
        };
    };
    let Some(stdout) = child.stdout.take() else {
        drop(stdin);
        let status = child.wait().map_err(|error| {
            SnapshotAdapterError::git_spawn(GitOperation::ReadBaseBlobs, &error)
        })?;
        return if status.success() {
            Err(SnapshotAdapterError::GitProtocol(
                GitOperation::ReadBaseBlobs,
            ))
        } else {
            Err(SnapshotAdapterError::GitExit(GitOperation::ReadBaseBlobs))
        };
    };

    let mut writer = BufWriter::new(stdin);
    let mut reader = BufReader::new(stdout);
    let exchange = exchange_blobs(object_ids, &mut writer, &mut reader);
    drop(writer);
    let trailing = require_eof(&mut reader);
    drop(reader);
    let status = child
        .wait()
        .map_err(|error| SnapshotAdapterError::git_spawn(GitOperation::ReadBaseBlobs, &error))?;
    drop(permit);
    if !status.success() {
        return Err(SnapshotAdapterError::GitExit(GitOperation::ReadBaseBlobs));
    }
    let blobs = exchange?;
    trailing?;
    Ok(blobs)
}

fn exchange_blobs(
    object_ids: &BTreeSet<GitObjectId>,
    writer: &mut impl Write,
    reader: &mut impl BufRead,
) -> Result<BTreeMap<GitObjectId, Arc<[u8]>>, SnapshotAdapterError> {
    let mut blobs = BTreeMap::new();
    for requested in object_ids {
        writer
            .write_all(requested.as_str().as_bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|error| protocol_io_error(&error))?;
        let (returned, size) = read_header(reader)?;
        if returned.as_str() != requested.as_str() {
            return Err(protocol_error());
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(size).is_err() {
            return Err(SnapshotAdapterError::Capacity);
        }
        bytes.resize(size, 0);
        reader
            .read_exact(&mut bytes)
            .map_err(|error| protocol_io_error(&error))?;
        let mut delimiter = [0_u8; 1];
        reader
            .read_exact(&mut delimiter)
            .map_err(|error| protocol_io_error(&error))?;
        if delimiter != [b'\n'] || blobs.insert(returned, Arc::from(bytes)).is_some() {
            return Err(protocol_error());
        }
    }
    Ok(blobs)
}

fn read_header(reader: &mut impl BufRead) -> Result<(GitObjectId, usize), SnapshotAdapterError> {
    let mut header = Vec::new();
    let read = reader
        .read_until(b'\n', &mut header)
        .map_err(|error| protocol_io_error(&error))?;
    if read == 0 {
        return Err(protocol_error());
    }
    let Some(header) = header.strip_suffix(b"\n") else {
        return Err(protocol_error());
    };
    let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
    let [raw_object, raw_kind, raw_size] = fields.as_slice() else {
        return Err(protocol_error());
    };
    if *raw_kind != b"blob" {
        return Err(protocol_error());
    }
    let object = std::str::from_utf8(raw_object)
        .map_err(|error| SnapshotAdapterError::git_encoding(GitOperation::ReadBaseBlobs, error))
        .and_then(|value| GitObjectId::parse(value).map_err(protocol_object_error))?;
    let size = parse_size(raw_size)?;
    Ok((object, size))
}

fn require_eof(reader: &mut impl Read) -> Result<(), SnapshotAdapterError> {
    let mut extra = [0_u8; 1];
    let count = reader
        .read(&mut extra)
        .map_err(|error| protocol_io_error(&error))?;
    if count != 0 {
        return Err(protocol_error());
    }
    Ok(())
}

const fn protocol_error() -> SnapshotAdapterError {
    SnapshotAdapterError::GitProtocol(GitOperation::ReadBaseBlobs)
}

fn protocol_io_error(error: &std::io::Error) -> SnapshotAdapterError {
    SnapshotAdapterError::git_io(GitOperation::ReadBaseBlobs, error)
}

fn protocol_object_error(error: norn_policy::phase_lock::GitObjectIdError) -> SnapshotAdapterError {
    match error {
        norn_policy::phase_lock::GitObjectIdError::Length { .. }
        | norn_policy::phase_lock::GitObjectIdError::InvalidHex => protocol_error(),
    }
}

fn parse_size(raw: &[u8]) -> Result<usize, SnapshotAdapterError> {
    if raw.is_empty() || raw.iter().any(|byte| !byte.is_ascii_digit()) {
        return Err(protocol_error());
    }
    let mut value = 0_usize;
    for byte in raw {
        let digit = usize::from(*byte - b'0');
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(digit))
            .ok_or_else(protocol_error)?;
    }
    Ok(value)
}
