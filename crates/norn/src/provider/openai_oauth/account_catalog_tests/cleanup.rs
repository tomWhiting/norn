use super::*;

#[cfg(unix)]
#[test]
fn lost_reservation_with_unsupported_residue_still_scrubs_credentials() -> TestResult {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let options = OAuthHttpOptions::default();
    let prepared = prepare_named_login(&base, "orphan", options)?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    let staged = reserved_root.as_path().join("staged");
    std::fs::create_dir(&staged)?;
    std::fs::write(staged.join("credential.tmp"), b"nested-secret")?;
    symlink(
        directory.path(),
        reserved_root.as_path().join("a-unsupported"),
    )?;
    io_layer::mutate_catalog(&base, options.credential_lock_timing()?, |catalog| {
        catalog.records.clear();
        Ok(())
    })?;

    let result = reservation.commit();

    assert!(matches!(result, Err(AccountCatalogError::ReservationLost)));
    assert!(
        !reserved_root.as_path().join("auth.json").exists(),
        "lost reservation cleanup retained a live credential",
    );
    assert!(
        !staged.join("credential.tmp").exists(),
        "unsupported residue blocked nested credential cleanup",
    );
    assert!(reserved_root.as_path().join("a-unsupported").is_symlink());
    assert!(io_layer::load_catalog(&base)?.records.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_residue_removal_cannot_shield_a_lost_reservation_credential() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let options = OAuthHttpOptions::default();
    let prepared = prepare_named_login(&base, "unreadable", options)?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    let blocker = reserved_root.as_path().join("a-blocker");
    std::fs::write(&blocker, b"not-a-credential")?;
    std::fs::set_permissions(&blocker, std::fs::Permissions::from_mode(0o0))?;
    io_layer::mutate_catalog(&base, options.credential_lock_timing()?, |catalog| {
        catalog.records.clear();
        Ok(())
    })?;

    let result = reservation.commit();

    assert!(matches!(result, Err(AccountCatalogError::ReservationLost)));
    assert!(blocker.exists(), "fixture did not exercise removal failure");
    assert!(
        !reserved_root.as_path().join("auth.json").exists(),
        "failed residue removal shielded a live credential",
    );
    assert!(io_layer::load_catalog(&base)?.records.is_empty());
    Ok(())
}
