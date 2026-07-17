//! Read-only inspection of migration records that cannot enter active replay.

use std::io::Write as _;

use norn::session::{
    LegacySessionMigrationRecord, read_legacy_migration_manifest, verify_legacy_session_migration,
};

use crate::cli::{ExitCode, LegacySessionCmd, SessionListFormat};

pub(super) fn run(command: &LegacySessionCmd) -> ExitCode {
    let norn_root = match crate::config::paths::session_norn_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("norn: legacy session inspection failed: {error}");
            return ExitCode::AgentError;
        }
    };
    match command {
        LegacySessionCmd::Verify => verify(&norn_root),
        LegacySessionCmd::List { format } => list(&norn_root, *format),
        LegacySessionCmd::Show { catalog_id } => show(&norn_root, catalog_id),
        LegacySessionCmd::Export { catalog_id } => export(&norn_root, catalog_id),
    }
}

fn verify(norn_root: &std::path::Path) -> ExitCode {
    match verify_legacy_session_migration(norn_root) {
        Ok(manifest) => {
            println!(
                "legacy session migration verified: source_tree_sha256={}; records={}",
                manifest.source_tree_sha256,
                manifest.sessions.len(),
            );
            ExitCode::Success
        }
        Err(error) => report(&error),
    }
}

fn list(norn_root: &std::path::Path, format: Option<SessionListFormat>) -> ExitCode {
    let manifest = match read_legacy_migration_manifest(norn_root) {
        Ok(manifest) => manifest,
        Err(error) => return report(&error),
    };
    let records = manifest
        .sessions
        .iter()
        .filter(|record| record.catalog_id.is_some())
        .collect::<Vec<_>>();
    match format.unwrap_or(SessionListFormat::Table) {
        SessionListFormat::Table => {
            if records.is_empty() {
                println!("No inspect-only legacy sessions found.");
            } else {
                println!("CATALOG_ID\tSESSION_ID\tSOURCE\tREASONS");
                for record in records {
                    println!(
                        "{}\t{}\t{}\t{}",
                        record.catalog_id.as_deref().unwrap_or("-"),
                        record.session_id.as_deref().unwrap_or("-"),
                        record.source_path.as_deref().unwrap_or("-"),
                        record.reasons.len(),
                    );
                }
            }
            ExitCode::Success
        }
        SessionListFormat::Json => print_json(&records),
    }
}

fn show(norn_root: &std::path::Path, catalog_id: &str) -> ExitCode {
    let manifest = match read_legacy_migration_manifest(norn_root) {
        Ok(manifest) => manifest,
        Err(error) => return report(&error),
    };
    let Some(record) = find_record(&manifest.sessions, catalog_id) else {
        eprintln!("norn: no inspect-only legacy session matches catalog id '{catalog_id}'");
        return ExitCode::AgentError;
    };
    print_json(record)
}

fn export(norn_root: &std::path::Path, catalog_id: &str) -> ExitCode {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    match norn::session::export_legacy_session_raw(norn_root, catalog_id, &mut output) {
        Ok(_) => match output.flush() {
            Ok(()) => ExitCode::Success,
            Err(error) => {
                eprintln!("norn: could not flush legacy session export: {error}");
                ExitCode::AgentError
            }
        },
        Err(error) => report(&error),
    }
}

fn find_record<'a>(
    records: &'a [LegacySessionMigrationRecord],
    catalog_id: &str,
) -> Option<&'a LegacySessionMigrationRecord> {
    records
        .iter()
        .find(|record| record.catalog_id.as_deref() == Some(catalog_id))
}

fn print_json(value: &impl serde::Serialize) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(encoded) => {
            println!("{encoded}");
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("norn: could not encode legacy session record: {error}");
            ExitCode::AgentError
        }
    }
}

fn report(error: &norn::session::migration::SessionMigrationError) -> ExitCode {
    eprintln!("norn: legacy session inspection failed: {error}");
    ExitCode::AgentError
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn explicit_verify_audits_a_published_migration() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        std::fs::create_dir(home.path().join("sessions"))?;
        std::fs::write(home.path().join("sessions/index.jsonl"), b"")?;
        let _ = norn::session::migrate_legacy_sessions(home.path())?;
        let code = temp_env::with_var("NORN_HOME", Some(home.path().as_os_str()), || {
            run(&LegacySessionCmd::Verify)
        });
        assert_eq!(code, ExitCode::Success);
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn legacy_inspection_rejects_relative_norn_home_before_fallback() {
        temp_env::with_var(
            "NORN_HOME",
            Some(std::ffi::OsStr::new("relative-home")),
            || {
                let command = LegacySessionCmd::List { format: None };
                assert_eq!(run(&command), ExitCode::AgentError);
            },
        );
    }
}
