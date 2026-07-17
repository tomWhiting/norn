use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::util::PrivateRootReader;

use super::classify::{decode_relative_path, hash_file};
use super::error::SessionMigrationError;
use super::types::{LegacySessionMigrationRecord, SessionMigrationManifest};

pub(super) fn validate_manifest_sources(
    manifest: &SessionMigrationManifest,
    backup: &PrivateRootReader,
) -> Result<(), SessionMigrationError> {
    validate_source_claims(&manifest.sessions, |path, expected| {
        let actual = hash_file(backup, path)?;
        if actual == expected {
            Ok(())
        } else {
            Err(SessionMigrationError::BackupConflict {
                path: path.to_path_buf(),
                existing_sha256: actual,
                source_sha256: expected.to_owned(),
            })
        }
    })
}

fn validate_source_claims(
    records: &[LegacySessionMigrationRecord],
    mut verify: impl FnMut(&Path, &str) -> Result<(), SessionMigrationError>,
) -> Result<(), SessionMigrationError> {
    let mut expected_by_path = BTreeMap::<PathBuf, String>::new();
    let mut verified = BTreeSet::<(PathBuf, String)>::new();
    for record in records {
        let Some(source_path) = record.source_path.as_deref() else {
            return Err(SessionMigrationError::UnrepresentableSource {
                reason: "migration manifest record lacks an immutable source selector".to_owned(),
            });
        };
        let Some(expected) = record.source_sha256.as_deref() else {
            return Err(SessionMigrationError::UnrepresentableSource {
                reason: format!("migration manifest source '{source_path}' lacks a digest"),
            });
        };
        let path = decode_relative_path(source_path)?;
        match expected_by_path.entry(path.clone()) {
            Entry::Occupied(prior) if prior.get().as_str() != expected => {
                return Err(SessionMigrationError::UnrepresentableSource {
                    reason: format!(
                        "migration manifest source '{source_path}' claims contradictory digests"
                    ),
                });
            }
            Entry::Vacant(slot) => {
                slot.insert(expected.to_owned());
            }
            Entry::Occupied(_) => {}
        }
        if verified.insert((path.clone(), expected.to_owned())) {
            verify(&path, expected)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::migration::LegacyClassificationReason;
    use crate::session::persistence::ResumeFidelity;

    fn record(path: &str, digest: &str) -> LegacySessionMigrationRecord {
        LegacySessionMigrationRecord {
            catalog_id: None,
            session_id: None,
            source_index_line: None,
            source_path: Some(path.to_owned()),
            source_sha256: Some(digest.to_owned()),
            source_format: None,
            fidelity: ResumeFidelity::InspectOnly,
            reasons: vec![LegacyClassificationReason::OrphanTimeline],
            destination_path: None,
        }
    }

    #[test]
    fn duplicate_source_claim_is_verified_once() -> Result<(), SessionMigrationError> {
        let digest = "a".repeat(64);
        let records = vec![record("one.jsonl", &digest), record("one.jsonl", &digest)];
        let mut visits = 0_u64;
        validate_source_claims(&records, |_path, _expected| {
            visits += 1;
            Ok(())
        })?;
        assert_eq!(visits, 1);
        Ok(())
    }

    #[test]
    fn contradictory_source_claim_fails_before_second_hash() {
        let records = vec![
            record("one.jsonl", &"a".repeat(64)),
            record("one.jsonl", &"b".repeat(64)),
        ];
        let mut visits = 0_u64;
        let error = validate_source_claims(&records, |_path, _expected| {
            visits += 1;
            Ok(())
        })
        .err();
        assert!(matches!(
            error,
            Some(SessionMigrationError::UnrepresentableSource { .. })
        ));
        assert_eq!(visits, 1);
    }
}
