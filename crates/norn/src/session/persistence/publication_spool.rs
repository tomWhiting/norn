//! Exclusive, journal-recovered publication of private inherited-spool authority.

use std::ffi::OsStr;
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use crate::session::persistence::{SessionIndexEntry, SessionPersistError};
use crate::session::spool::{SpoolInheritance, inheritance_path};
use crate::util::{PrivateEntryKind, PrivateRoot};

use super::publication_conflict::conflict;

#[derive(Debug, thiserror::Error)]
#[error(
    "spool-inheritance sidecar for session {session} has unrepresentable byte length {bytes}: {source}"
)]
struct SidecarLengthError {
    session: String,
    bytes: u64,
    #[source]
    source: std::num::TryFromIntError,
}

pub(super) fn reject_unexpected_inheritance(
    root: &PrivateRoot,
    destination: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    if root.regular_file_exists(&inheritance_path(&destination.id))? {
        return Err(conflict(
            &destination.id,
            "publication has an undeclared spool-inheritance sidecar",
        ));
    }
    Ok(())
}

pub(super) fn ensure_inheritance_destination_unclaimed(
    root: &PrivateRoot,
    destination: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    if root
        .read_dir(Path::new(""))?
        .iter()
        .any(|entry| entry.name == std::ffi::OsStr::new(&destination.id))
    {
        return Err(conflict(
            &destination.id,
            "spool-inheritance destination directory is already occupied",
        ));
    }
    Ok(())
}

pub(super) fn remove_inheritance_temporary(
    root: &PrivateRoot,
    transaction_id: &str,
    destination: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    let directory = Path::new(&destination.id);
    let temporary = directory.join(format!("spool-inheritance.{transaction_id}.tmp"));
    if root.regular_file_exists(&temporary)? {
        root.remove_file(&temporary)?;
        root.sync_dir(directory)?;
    }
    Ok(())
}

pub(super) fn validate_spool_only_directory(
    root: &PrivateRoot,
    transaction_id: &str,
    destination: &SessionIndexEntry,
) -> Result<(), SessionPersistError> {
    let entries = match root.read_dir(Path::new(&destination.id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let temporary = format!("spool-inheritance.{transaction_id}.tmp");
    if entries.iter().any(|entry| {
        entry.kind != PrivateEntryKind::File
            || (entry.name != OsStr::new("spool-inheritance.json")
                && entry.name != OsStr::new(&temporary))
    }) {
        return Err(conflict(
            &destination.id,
            "the spool-inheritance directory shape disagrees with its journal",
        ));
    }
    Ok(())
}

pub(super) fn recover_inheritance(
    root: &PrivateRoot,
    transaction_id: &str,
    destination: &SessionIndexEntry,
    manifest: &SpoolInheritance,
) -> Result<(), SessionPersistError> {
    manifest.validate_destination(destination)?;
    let bytes = serde_json::to_vec(manifest)?;
    let directory = Path::new(&destination.id);
    let final_path = inheritance_path(&destination.id);
    let temporary = directory.join(format!("spool-inheritance.{transaction_id}.tmp"));
    root.create_dir_all(directory)?;
    if root.regular_file_exists(&final_path)? {
        let mut file = root.open_read(&final_path)?;
        let length = file.metadata()?.len();
        let size = usize::try_from(length).map_err(|source| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                SidecarLengthError {
                    session: destination.id.clone(),
                    bytes: length,
                    source,
                },
            )
        })?;
        if size != bytes.len() {
            return Err(conflict(
                &destination.id,
                "spool-inheritance sidecar differs from its journal",
            ));
        }
        let mut actual = vec![0; size];
        file.read_exact(&mut actual)?;
        let mut extra = [0];
        if actual != bytes || file.read(&mut extra)? != 0 {
            return Err(conflict(
                &destination.id,
                "spool-inheritance sidecar differs from its journal",
            ));
        }
    } else {
        // Only this exact journal transaction owns this temporary name. An interrupted
        // temporary is replaceable; a published sidecar is never overwritten.
        if root.regular_file_exists(&temporary)? {
            root.remove_file(&temporary)?;
        }
        let mut file = root.create_new(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        root.publish_new(&temporary, &final_path)?;
    }
    if root.regular_file_exists(&temporary)? {
        root.remove_file(&temporary)?;
    }
    root.sync_dir(directory)?;
    root.sync_dir(Path::new(""))?;
    Ok(())
}
