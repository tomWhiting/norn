use std::io::Write as _;
use std::process::Command;

const RECOVERY_CHILD_MODE_ENV: &str = "NORN_D2_MIGRATION_RECOVERY_CHILD";
const RECOVERY_CHILD_ROOT_ENV: &str = "NORN_D2_MIGRATION_RECOVERY_ROOT";
const RECOVERY_CHILD_CHECKPOINT_ENV: &str = "NORN_D2_MIGRATION_RECOVERY_CHECKPOINT";
const RECOVERY_CHILD_EXIT_CODE: i32 = 86;
const RECOVERY_SENTINEL_PREFIX: &str = "D2_RECOVERY_EVENT=";
const RECOVERY_CHILD_TEST: &str = "migration_recovery_process_child";

macro_rules! migration_recovery_test {
    ($name:ident, $checkpoint:ident) => {
        #[test]
        fn $name() -> Result<(), Box<dyn Error>> {
            assert_migration_recovers_from(MigrationCheckpoint::$checkpoint)
        }
    };
}

migration_recovery_test!(migration_recovers_after_backup_prepared, BackupPrepared);
migration_recovery_test!(migration_recovers_after_backup_published, BackupPublished);
migration_recovery_test!(migration_recovers_after_backup_durable, BackupDurable);
migration_recovery_test!(
    migration_recovers_after_strict_store_prepared,
    StrictStorePrepared
);
migration_recovery_test!(
    migration_recovers_after_strict_store_published,
    StrictStorePublished
);
migration_recovery_test!(
    migration_recovers_after_strict_store_durable,
    StrictStoreDurable
);

#[test]
fn migration_recovery_process_child() -> Result<(), Box<dyn Error>> {
    if std::env::var_os(RECOVERY_CHILD_MODE_ENV).is_none() {
        return Ok(());
    }

    let root = std::env::var_os(RECOVERY_CHILD_ROOT_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("migration recovery child has no source root"))?;
    let checkpoint_name = std::env::var(RECOVERY_CHILD_CHECKPOINT_ENV)?;
    let checkpoint = recovery_checkpoint(&checkpoint_name)?;
    let result = migrate_legacy_sessions_with_hook(&root, &mut |current| {
        if current == checkpoint {
            emit_recovery_sentinel(current)?;
            std::process::exit(RECOVERY_CHILD_EXIT_CODE);
        }
        Ok(())
    });

    match result {
        Ok(_) => Err(io::Error::other(format!(
            "migration completed without reaching checkpoint {checkpoint_name}"
        ))
        .into()),
        Err(error) => Err(error.into()),
    }
}

fn assert_migration_recovers_from(checkpoint: MigrationCheckpoint) -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_legacy_fixture(&root, b"{\"norn_session_format\":1}\n", 0)?;
    let source_before = legacy_source_snapshot(&root)?;

    run_interrupted_migration(&root, checkpoint)?;
    assert_eq!(legacy_source_snapshot(&root)?, source_before);

    let outcome = migrate_legacy_sessions(&root)?;
    let (digest, destination, backup) = outcome_paths(outcome);
    assert_eq!(destination, root.join("session-store"));
    assert_eq!(
        backup,
        root.join("session-migration-backups")
            .join(&digest)
            .join("sessions")
    );
    assert_no_migration_stage(&root);
    assert_eq!(legacy_source_snapshot(&root)?, source_before);
    let manifest = super::verify_legacy_session_migration(&root)?;
    assert_eq!(manifest.source_tree_sha256, digest);

    let converged = migrate_legacy_sessions(&root)?;
    let SessionMigrationOutcome::AlreadyMigrated {
        source_tree_sha256,
        destination: converged_destination,
        backup: converged_backup,
        ..
    } = converged
    else {
        return Err(io::Error::other("recovered migration did not converge").into());
    };
    assert_eq!(source_tree_sha256, digest);
    assert_eq!(converged_destination, destination);
    assert_eq!(converged_backup, backup);
    assert_no_migration_stage(&root);
    assert_eq!(legacy_source_snapshot(&root)?, source_before);
    assert_eq!(super::verify_legacy_session_migration(&root)?, manifest);
    Ok(())
}

fn run_interrupted_migration(
    root: &Path,
    checkpoint: MigrationCheckpoint,
) -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let output = Command::new(executable)
        .arg("--exact")
        .arg(recovery_child_test_name()?)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(RECOVERY_CHILD_MODE_ENV, "1")
        .env(RECOVERY_CHILD_ROOT_ENV, root)
        .env(
            RECOVERY_CHILD_CHECKPOINT_ENV,
            checkpoint.evidence_name(),
        )
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    let expected_sentinel = format!(
        "{RECOVERY_SENTINEL_PREFIX}{}",
        checkpoint.evidence_name()
    );
    let observed_sentinels = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| line.starts_with(RECOVERY_SENTINEL_PREFIX))
        .collect::<Vec<_>>();

    if output.status.code() != Some(RECOVERY_CHILD_EXIT_CODE) {
        return Err(io::Error::other(format!(
            "migration recovery child exited as {:?}, expected {RECOVERY_CHILD_EXIT_CODE}; stdout={stdout:?}; stderr={stderr:?}",
            output.status.code()
        ))
        .into());
    }
    if observed_sentinels.as_slice() != [expected_sentinel.as_str()] {
        return Err(io::Error::other(format!(
            "migration recovery child emitted {observed_sentinels:?}, expected only {expected_sentinel:?}"
        ))
        .into());
    }
    // The child output stays captured so only this parent-validated event is
    // visible to the retained distribution runner.
    emit_recovery_sentinel(checkpoint)?;
    Ok(())
}

fn recovery_child_test_name() -> Result<String, io::Error> {
    let Some((_, module)) = module_path!().split_once("::") else {
        return Err(io::Error::other(format!(
            "test module has no crate prefix: {}",
            module_path!()
        )));
    };
    Ok(format!("{module}::{RECOVERY_CHILD_TEST}"))
}

fn recovery_checkpoint(name: &str) -> Result<MigrationCheckpoint, io::Error> {
    match name {
        "backup_prepared" => Ok(MigrationCheckpoint::BackupPrepared),
        "backup_published" => Ok(MigrationCheckpoint::BackupPublished),
        "backup_durable" => Ok(MigrationCheckpoint::BackupDurable),
        "strict_store_prepared" => Ok(MigrationCheckpoint::StrictStorePrepared),
        "strict_store_published" => Ok(MigrationCheckpoint::StrictStorePublished),
        "strict_store_durable" => Ok(MigrationCheckpoint::StrictStoreDurable),
        _ => Err(io::Error::other(format!(
            "unknown migration recovery checkpoint {name:?}"
        ))),
    }
}

fn emit_recovery_sentinel(
    checkpoint: MigrationCheckpoint,
) -> Result<(), super::SessionMigrationError> {
    let mut stderr = io::stderr().lock();
    writeln!(
        stderr,
        "{RECOVERY_SENTINEL_PREFIX}{}",
        checkpoint.evidence_name()
    )
    .and_then(|()| stderr.flush())
    .map_err(|error| {
        super::SessionMigrationError::mutation(
            "writing migration recovery sentinel",
            checkpoint.evidence_name(),
            error,
        )
    })
}

fn assert_no_migration_stage(root: &Path) {
    assert!(!root.join(BACKUP_STAGE).exists());
    assert!(!root.join(STRICT_STAGE).exists());
}

#[derive(Debug, Eq, PartialEq)]
struct LegacySourceSnapshot {
    digest: String,
    topology_and_modes: Vec<crate::util::PrivateTreeEntry>,
    #[cfg(unix)]
    root_mode: u32,
}

fn legacy_source_snapshot(root: &Path) -> Result<LegacySourceSnapshot, Box<dyn Error>> {
    let source_path = root.join("sessions");
    let source = crate::util::PrivateRootReader::open(&source_path)?;
    let topology_and_modes = source.read_tree()?;
    let digest = super::tree::digest_tree(&source, &topology_and_modes)?;
    #[cfg(unix)]
    let root_mode = {
        use std::os::unix::fs::PermissionsExt as _;

        fs::metadata(&source_path)?.permissions().mode()
    };
    Ok(LegacySourceSnapshot {
        digest,
        topology_and_modes,
        #[cfg(unix)]
        root_mode,
    })
}

fn outcome_paths(outcome: SessionMigrationOutcome) -> (String, PathBuf, PathBuf) {
    match outcome {
        SessionMigrationOutcome::Migrated {
            source_tree_sha256,
            destination,
            backup,
            ..
        }
        | SessionMigrationOutcome::AlreadyMigrated {
            source_tree_sha256,
            destination,
            backup,
            ..
        } => (source_tree_sha256, destination, backup),
    }
}
