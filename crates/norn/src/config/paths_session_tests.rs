use std::cell::Cell;
use std::ffi::OsString;

use super::*;

#[test]
fn absolute_override_is_authoritative_without_a_home_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let consulted_home = Cell::new(false);
    let root = resolve_session_norn_root_from(Some(OsString::from("/trusted/norn")), || {
        consulted_home.set(true);
        None
    })?;
    assert_eq!(root, PathBuf::from("/trusted/norn"));
    assert!(!consulted_home.get());
    Ok(())
}

#[test]
fn relative_override_is_rejected_instead_of_falling_back() {
    let result = resolve_session_norn_root_from(Some(OsString::from("relative-norn")), || {
        Some(PathBuf::from("/trusted/home"))
    });
    assert!(result.is_err());
}

#[test]
fn missing_or_relative_home_is_rejected_without_an_override() {
    assert!(resolve_session_norn_root_from(None, || None).is_err());
    assert!(resolve_session_norn_root_from(None, || Some(PathBuf::from("relative-home"))).is_err());
}

#[test]
fn standard_store_without_legacy_namespace_needs_no_cutover_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    assert_eq!(
        resolve_standard_session_data_dir_at(&root)?,
        root.join("session-store")
    );
    Ok(())
}

#[test]
fn legacy_namespace_without_cutover_proof_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir(root.join("sessions"))?;
    std::fs::create_dir(root.join("session-store"))?;
    std::fs::write(
        root.join("session-store/index.jsonl"),
        b"{\"norn_session_format\":2}\n",
    )?;

    assert!(resolve_standard_session_data_dir_at(&root).is_err());
    Ok(())
}

#[test]
#[cfg(unix)]
fn standard_runtime_guard_never_decodes_legacy_content() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir(root.join("sessions"))?;
    std::fs::write(root.join("sessions/index.jsonl"), b"")?;
    let _outcome = crate::session::migrate_legacy_sessions(&root)?;
    std::fs::remove_file(root.join("sessions/index.jsonl"))?;
    std::os::unix::fs::symlink(
        root.join("missing-legacy-index"),
        root.join("sessions/index.jsonl"),
    )?;

    assert_eq!(
        resolve_standard_session_data_dir_at(&root)?,
        root.join("session-store")
    );
    Ok(())
}
