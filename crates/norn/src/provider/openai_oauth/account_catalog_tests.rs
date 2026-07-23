use std::cell::Cell;
use std::error::Error;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serial_test::serial;

use super::*;
use crate::provider::openai_oauth::storage::save_auth_dot_json;
use crate::provider::openai_oauth::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use crate::provider::openai_oauth::{
    AuthCredentialsStoreMode, prepare_local_logout, resolve_norn_auth_root,
};

#[path = "account_catalog_tests/cleanup.rs"]
mod cleanup;

type TestResult = Result<(), Box<dyn Error>>;

const ABORT_CRASH_CHILD_ENV: &str = "NORN_ACCOUNT_ABORT_CRASH_ROOT";
const ABORT_CRASH_CHILD_TEST: &str =
    "provider::openai_oauth::account_catalog::tests::abort_crash_child_stops_after_slot_scrub";
const ABORT_CRASH_EXIT: i32 = 86;

fn base_root(directory: &tempfile::TempDir) -> Result<NornAuthRoot, Box<dyn Error>> {
    NornAuthRoot::try_from(directory.path().join("auth")).map_err(Into::into)
}

fn save_fixture(root: &NornAuthRoot) -> TestResult {
    let account_id = root
        .as_path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("fixture root has no UTF-8 final component"))?;
    save_fixture_identity(root, account_id)
}

fn save_fixture_identity(root: &NornAuthRoot, account_id: &str) -> TestResult {
    let auth = AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing(account_id),
        access_token: format!("access-{account_id}"),
        refresh_token: String::new(),
        account_id: Some(account_id.to_owned()),
        additional_fields: std::collections::BTreeMap::new(),
    });
    save_auth_dot_json(root.as_path(), &auth)?;
    Ok(())
}

fn publish(base: &NornAuthRoot, alias: &str) -> TestResult {
    let prepared = prepare_named_login(base, alias, OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    save_fixture(reservation.auth_root())?;
    reservation.commit()?;
    Ok(())
}

#[test]
fn aliases_reject_paths_and_collide_case_insensitively() -> TestResult {
    for invalid in [
        "",
        ".hidden",
        "../work",
        "work/home",
        "work home",
        "default",
    ] {
        assert!(AccountAlias::parse(invalid).is_err(), "accepted {invalid}");
    }
    let long_alias = format!("a{}", "b".repeat(2_048));
    assert_eq!(AccountAlias::parse(&long_alias)?.as_str(), long_alias);

    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    publish(&base, "Work")?;
    assert!(matches!(
        prepare_named_login(&base, "wOrK", OAuthHttpOptions::default()),
        Err(AccountCatalogError::AliasExists)
    ));
    Ok(())
}

#[test]
fn duplicate_remote_identities_are_rejected_across_every_account_slot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    save_fixture_identity(&base, "shared-identity")?;
    let prepared = prepare_named_login(&base, "work", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let named_root = reservation.auth_root().clone();
    save_fixture_identity(&named_root, "shared-identity")?;
    assert!(matches!(
        reservation.commit(),
        Err(AccountCatalogError::DuplicateIdentity)
    ));
    assert!(!named_root.as_path().exists());

    publish(&base, "personal")?;
    let personal_root = resolve_account_root(&base, Some("personal"))?;
    let personal = CredentialTransaction::inspect(&personal_root)?;
    let CredentialDocument::Parsed(personal_auth) = personal.document else {
        return Err(std::io::Error::other("published fixture was not usable").into());
    };
    assert!(matches!(
        validate_default_login_identity(&base, &personal_auth),
        Err(AccountCatalogError::DuplicateIdentity)
    ));
    Ok(())
}

#[test]
fn explicit_default_and_invalid_alias_do_not_read_the_named_catalog() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    std::fs::create_dir_all(base.as_path())?;
    std::fs::write(base.as_path().join("accounts.json"), b"{malformed")?;

    assert_eq!(resolve_account_root(&base, Some("default"))?, base);
    assert!(matches!(
        resolve_account_root(&base, Some("../invalid")),
        Err(AccountCatalogError::InvalidAlias)
    ));
    Ok(())
}

#[test]
fn named_slots_coexist_and_switching_changes_only_future_resolution() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    save_fixture(&base)?;
    publish(&base, "work")?;
    let work_root = resolve_account_root(&base, None)?;
    publish(&base, "personal")?;
    let personal_root = resolve_account_root(&base, None)?;

    assert_ne!(work_root, personal_root);
    assert_ne!(work_root, base);
    assert_ne!(personal_root, base);
    use_account(&base, "work", OAuthHttpOptions::default())?;
    assert_eq!(resolve_account_root(&base, None)?, work_root);
    assert_eq!(
        resolve_account_root(&base, Some("personal"))?,
        personal_root
    );
    assert_eq!(resolve_account_root(&base, Some("default"))?, base);

    let accounts = list_accounts(&base)?;
    assert_eq!(accounts.len(), 3);
    assert!(
        accounts
            .iter()
            .any(|account| account.alias == "work" && account.active)
    );
    assert!(
        accounts
            .iter()
            .any(|account| account.alias == "default" && account.legacy_default)
    );
    Ok(())
}

#[test]
fn removing_active_account_clears_selection_without_touching_other_slots() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    save_fixture(&base)?;
    publish(&base, "one")?;
    let one = resolve_account_root(&base, Some("one"))?;
    publish(&base, "two")?;
    let two = resolve_account_root(&base, Some("two"))?;

    let options = OAuthHttpOptions::default();
    let timing = options.credential_lock_timing()?;
    let reservation = prepare_named_account_logout(&base, "two", options)?;
    let local = prepare_local_logout(
        reservation.auth_root(),
        AuthCredentialsStoreMode::File,
        timing,
    );
    let _prepared = reservation.finish(local);

    assert_eq!(resolve_account_root(&base, None)?, base);
    assert!(base.as_path().join("auth.json").is_file());
    assert!(one.as_path().join("auth.json").is_file());
    assert!(!two.as_path().exists());
    assert!(matches!(
        resolve_account_root(&base, Some("two")),
        Err(AccountCatalogError::AliasNotFound)
    ));
    Ok(())
}

#[test]
fn interrupted_post_save_login_is_published_without_replaying_login() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "recover", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    drop(reservation);

    assert!(matches!(
        prepare_named_login(&base, "RECOVER", OAuthHttpOptions::default())?,
        NamedLoginPreparation::Recovered
    ));
    assert_eq!(resolve_account_root(&base, None)?, reserved_root);
    Ok(())
}

#[test]
fn abort_scrubs_credentials_before_retiring_catalog_reservation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "ordered", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    let credential_lock = reserved_root
        .as_path()
        .join(super::super::credential_transaction::CREDENTIAL_LOCK_FILE);
    std::fs::write(&credential_lock, b"")?;
    let residue = reserved_root.as_path().join("staged");
    std::fs::create_dir(&residue)?;
    std::fs::write(residue.join("credential.tmp"), b"secret")?;
    let observed = Cell::new(false);

    reservation.abort_after_slot_scrub(|| {
        assert!(!reserved_root.as_path().join("auth.json").exists());
        assert!(!residue.exists());
        assert!(credential_lock.is_file());
        assert!(reserved_root.as_path().join(LOGIN_LOCK_FILE).is_file());
        let catalog = io_layer::load_catalog(&base)?;
        let record = catalog
            .record("ordered")
            .ok_or(AccountCatalogError::ReservationLost)?;
        assert_eq!(record.state, AccountState::Pending);
        observed.set(true);
        Ok(())
    })?;

    assert!(observed.get());
    assert!(!reserved_root.as_path().exists());
    assert!(matches!(
        resolve_account_root(&base, Some("ordered")),
        Err(AccountCatalogError::AliasNotFound)
    ));
    Ok(())
}

#[test]
fn catalog_failure_after_slot_scrub_self_heals_without_old_credentials() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let options = OAuthHttpOptions {
        credential_lock_timeout: Duration::from_millis(20),
        credential_lock_poll_interval: Duration::from_millis(1),
        ..OAuthHttpOptions::default()
    };
    let prepared = prepare_named_login(&base, "retry", options)?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    let catalog_lock = CredentialTransaction::acquire(
        &base,
        OAuthHttpOptions::default().credential_lock_timing()?,
    )?;

    let result = reservation.abort();

    assert!(matches!(result, Err(AccountCatalogError::Coordination)));
    assert!(!reserved_root.as_path().join("auth.json").exists());
    let catalog = io_layer::load_catalog(&base)?;
    let record = catalog
        .record("retry")
        .ok_or(AccountCatalogError::ReservationLost)?;
    assert_eq!(record.state, AccountState::Pending);
    drop(catalog_lock);
    let prepared = prepare_named_login(&base, "RETRY", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(replacement) = prepared else {
        return Err(std::io::Error::other("credential-free slot unexpectedly recovered").into());
    };
    assert_eq!(replacement.auth_root(), &reserved_root);
    let snapshot = CredentialTransaction::inspect(replacement.auth_root())?;
    assert!(matches!(snapshot.document, CredentialDocument::Missing));
    save_fixture(replacement.auth_root())?;
    replacement.commit()?;
    assert_eq!(resolve_account_root(&base, Some("retry"))?, reserved_root);
    Ok(())
}

#[test]
fn abort_crash_child_stops_after_slot_scrub() -> TestResult {
    let Some(base_path) = std::env::var_os(ABORT_CRASH_CHILD_ENV) else {
        return Ok(());
    };
    let base = NornAuthRoot::try_from(std::path::PathBuf::from(base_path))?;
    let prepared = prepare_named_login(&base, "crash", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("crash fixture unexpectedly recovered").into());
    };
    save_fixture(reservation.auth_root())?;
    reservation.abort_after_slot_scrub(|| std::process::exit(ABORT_CRASH_EXIT))?;
    Ok(())
}

#[test]
fn crash_after_slot_scrub_restarts_from_credential_free_pending_record() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "crash", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    drop(reservation);

    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", ABORT_CRASH_CHILD_TEST, "--nocapture"])
        .env(ABORT_CRASH_CHILD_ENV, base.as_path())
        .output()?;

    assert_eq!(output.status.code(), Some(ABORT_CRASH_EXIT));
    assert!(!reserved_root.as_path().join("auth.json").exists());
    let catalog = io_layer::load_catalog(&base)?;
    let record = catalog
        .record("crash")
        .ok_or(AccountCatalogError::ReservationLost)?;
    assert_eq!(record.state, AccountState::Pending);
    let prepared = prepare_named_login(&base, "CRASH", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(replacement) = prepared else {
        return Err(std::io::Error::other("crashed slot unexpectedly recovered").into());
    };
    assert_eq!(replacement.auth_root(), &reserved_root);
    let snapshot = CredentialTransaction::inspect(replacement.auth_root())?;
    assert!(matches!(snapshot.document, CredentialDocument::Missing));
    replacement.abort()?;
    Ok(())
}

#[test]
fn failed_publication_scrubs_the_reserved_credential_slot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "lost", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    save_fixture(&reserved_root)?;
    let timing = OAuthHttpOptions::default().credential_lock_timing()?;
    io_layer::mutate_catalog(&base, timing, |catalog| {
        catalog.records.clear();
        Ok(())
    })?;

    assert!(matches!(
        reservation.commit(),
        Err(AccountCatalogError::ReservationLost)
    ));
    assert!(!reserved_root.as_path().exists());
    Ok(())
}

#[test]
fn all_account_logout_waits_for_publication_and_clears_pending_or_ready_slot() -> TestResult {
    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "racing", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let reserved_root = reservation.auth_root().clone();
    let all_accounts = prepare_all_account_logout(&base, OAuthHttpOptions::default())?;
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        Ok::<_, std::io::Error>(all_accounts.prepare_local_logouts_observed(
            AuthCredentialsStoreMode::File,
            move || {
                let _ = waiting_tx.send(());
            },
        ))
    });
    waiting_rx.recv()?;
    save_fixture(&reserved_root)?;
    reservation.commit()?;

    let _prepared = handle.join().map_err(|payload| {
        let payload_kind = if payload.is::<String>() || payload.is::<&'static str>() {
            "string"
        } else {
            "non-string"
        };
        std::io::Error::other(format!(
            "all-account logout thread failed ({payload_kind} panic payload)"
        ))
    })??;

    assert_eq!(resolve_account_root(&base, None)?, base);
    assert!(!reserved_root.as_path().exists());
    assert!(matches!(
        resolve_account_root(&base, Some("racing")),
        Err(AccountCatalogError::AliasNotFound)
    ));
    Ok(())
}

#[test]
fn debug_surfaces_do_not_echo_aliases_or_storage_paths() -> TestResult {
    let alias = AccountAlias::parse("private-work-account")?;
    let rendered = format!("{alias:?}");
    assert!(!rendered.contains("private-work-account"));

    let directory = tempfile::tempdir()?;
    let base = base_root(&directory)?;
    let prepared = prepare_named_login(&base, "private-work-account", OAuthHttpOptions::default())?;
    let NamedLoginPreparation::Pending(reservation) = prepared else {
        return Err(std::io::Error::other("fresh alias unexpectedly recovered").into());
    };
    let rendered = format!("{reservation:?}");
    assert!(!rendered.contains("private-work-account"));
    assert!(!rendered.contains(&directory.path().display().to_string()));
    reservation.abort()?;
    Ok(())
}

#[test]
#[serial]
fn named_account_operations_never_observe_or_mutate_codex_home() -> TestResult {
    let directory = tempfile::tempdir()?;
    let norn_home = directory.path().join("norn-home");
    let codex_home = directory.path().join("foreign-codex-home");
    std::fs::create_dir_all(&codex_home)?;
    let foreign_auth = codex_home.join("auth.json");
    let sentinel = b"foreign-secret-sentinel";
    std::fs::write(&foreign_auth, sentinel)?;

    temp_env::with_vars(
        [
            ("NORN_HOME", Some(norn_home.as_os_str())),
            ("CODEX_HOME", Some(codex_home.as_os_str())),
        ],
        || -> TestResult {
            let base = resolve_norn_auth_root(None)?;
            publish(&base, "isolated")?;
            use_account(&base, "isolated", OAuthHttpOptions::default())?;
            assert_eq!(std::fs::read(&foreign_auth)?, sentinel);
            assert_eq!(std::fs::read_dir(&codex_home)?.count(), 1);
            Ok(())
        },
    )?;
    assert!(Path::new(&foreign_auth).is_file());
    Ok(())
}
