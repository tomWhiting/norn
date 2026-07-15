use std::error::Error;

use norn_policy::{EntryKind, OwnedSnapshot, RepositoryPath, SnapshotEntry};

pub(super) type TestResult = Result<(), Box<dyn Error>>;

pub(super) fn snapshot(entries: &[(&str, &str)]) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::regular(contents.as_bytes().to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn snapshot_with_kind(
    entries: &[(&str, EntryKind, &str)],
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, kind, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::new(*kind, contents.as_bytes().to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

pub(super) fn snapshot_bytes(entries: &[(&str, &[u8])]) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::regular(contents.to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}
