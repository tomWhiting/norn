//! End-to-end fences for the session index-lock acquisition deadline
//! (owner-ruled fix, 2026-07-06).
//!
//! Before this fix, `builder_from_cli` constructed its [`SessionManager`]
//! without a deadline, so `file.lock()` on `index.lock` blocked forever
//! behind a wedged sibling process — every new norn on the machine hung
//! silently before a session file even existed. These tests pin the whole
//! chain the CLI now rides:
//!
//! - `agent.index_lock_deadline_ms` deserialises, validates, and resolves
//!   through [`resolve_index_lock_deadline`];
//! - `-c index_lock_deadline_ms=N` outranks the settings value;
//! - an explicit `0` is a typed config error on both surfaces;
//! - a config-derived deadline applied to a [`SessionManager`] turns a
//!   held lock into the typed [`SessionPersistError::IndexLockTimeout`]
//!   naming the lock file, instead of an indefinite hang.
//!
//! The 50 ms deadlines below are legitimate test configuration (small so
//! the timeout fires fast), not production defaults — the compiled
//! default is the owner-ruled [`DEFAULT_INDEX_LOCK_DEADLINE_MS`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use norn::config::{AgentSettings, NornSettings, validate_settings};
use norn::session::store::DurabilityPolicy;
use norn::session::{CreateSessionOptions, SessionManager, SessionPersistError};

use norn_cli::cli::BuildError;
use norn_cli::config::{
    ConfigOverrides, DEFAULT_INDEX_LOCK_DEADLINE_MS, resolve_index_lock_deadline,
};

/// Merged settings carrying only `agent.index_lock_deadline_ms`.
fn settings_with_deadline_ms(ms: u64) -> NornSettings {
    NornSettings {
        agent: Some(AgentSettings {
            index_lock_deadline_ms: Some(ms),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    }
}

/// The settings key deserialises from JSON, passes semantic validation,
/// and resolves to the configured deadline.
#[test]
fn settings_key_parses_validates_and_resolves() {
    let json = r#"{"agent":{"index_lock_deadline_ms":50}}"#;
    let settings: NornSettings = serde_json::from_str(json).unwrap();
    validate_settings(&settings).unwrap();

    let deadline = resolve_index_lock_deadline(&settings, &ConfigOverrides::default()).unwrap();
    assert_eq!(deadline, Duration::from_millis(50));
}

/// `-c index_lock_deadline_ms=N` (parsed exactly as the CLI parses it)
/// outranks the settings value.
#[test]
fn c_override_outranks_settings() {
    let settings = settings_with_deadline_ms(60_000);
    let overrides = ConfigOverrides::parse(&["index_lock_deadline_ms=75".to_owned()]).unwrap();
    let deadline = resolve_index_lock_deadline(&settings, &overrides).unwrap();
    assert_eq!(deadline, Duration::from_millis(75));
}

/// Review A3 (2026-07-06): the merge layer carries
/// `agent.index_lock_deadline_ms` across settings tiers — local beats
/// project beats user — and the merged winner is what
/// [`resolve_index_lock_deadline`] applies. Deleting the field's
/// `pick_scalar` arm in `merge_agent` fails this fence.
#[test]
fn merged_tiers_local_beats_project_beats_user_through_resolution() {
    use norn::config::merge_settings;

    let mut user = settings_with_deadline_ms(1_000);
    let mut project = settings_with_deadline_ms(2_000);
    let mut local = settings_with_deadline_ms(3_000);
    let mut cli_layer = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli_layer);
    validate_settings(&merged).unwrap();

    let deadline = resolve_index_lock_deadline(&merged, &ConfigOverrides::default()).unwrap();
    assert_eq!(deadline, Duration::from_secs(3), "local tier wins");
}

/// Review A3 (2026-07-06): a value set only in the user tier survives
/// the merge and reaches [`resolve_index_lock_deadline`] — it must
/// resolve to the user's value, never fall through to the compiled
/// default as if the setting had been dropped.
#[test]
fn user_tier_only_deadline_survives_merge_through_resolution() {
    use norn::config::merge_settings;

    let mut user = settings_with_deadline_ms(1_234);
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli_layer = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli_layer);
    validate_settings(&merged).unwrap();
    assert_eq!(
        merged
            .agent
            .as_ref()
            .and_then(|agent| agent.index_lock_deadline_ms),
        Some(1_234),
        "the merge must not drop a user-tier-only value",
    );

    let deadline = resolve_index_lock_deadline(&merged, &ConfigOverrides::default()).unwrap();
    assert_eq!(deadline, Duration::from_millis(1_234));
    assert_ne!(
        deadline,
        Duration::from_millis(DEFAULT_INDEX_LOCK_DEADLINE_MS),
        "the user-tier value must win over the compiled default",
    );
}

/// With no settings and no `-c` override the owner-ruled compiled default
/// applies — the CLI never falls back to the library's indefinite wait.
#[test]
fn unset_everywhere_resolves_to_owner_ruled_default() {
    let deadline =
        resolve_index_lock_deadline(&NornSettings::default(), &ConfigOverrides::default()).unwrap();
    assert_eq!(
        deadline,
        Duration::from_millis(DEFAULT_INDEX_LOCK_DEADLINE_MS),
    );
}

/// An explicit `0` in settings fails semantic validation with the typed
/// config error.
#[test]
fn zero_settings_value_rejected_by_validation() {
    let json = r#"{"agent":{"index_lock_deadline_ms":0}}"#;
    let settings: NornSettings = serde_json::from_str(json).unwrap();
    let err = validate_settings(&settings).unwrap_err();
    let rendered = err.to_string();
    assert!(
        rendered.contains("agent.index_lock_deadline_ms"),
        "error names the key: {rendered}",
    );
    assert!(
        rendered.contains("a zero deadline can never acquire the lock"),
        "error explains the rejection: {rendered}",
    );
}

/// An explicit `-c index_lock_deadline_ms=0` is rejected at parse time
/// with the typed argument error.
#[test]
fn zero_c_override_rejected_at_parse() {
    let err = ConfigOverrides::parse(&["index_lock_deadline_ms=0".to_owned()])
        .expect_err("a zero deadline must be rejected");
    let BuildError::Argument(reason) = err else {
        panic!("expected Argument, got {err:?}");
    };
    assert!(
        reason.contains("index_lock_deadline_ms")
            && reason.contains("a zero deadline can never acquire the lock"),
        "error names the key and the rejection: {reason}",
    );
}

/// The core regression fence: a config-derived deadline applied to the
/// [`SessionManager`] (exactly as `builder_from_cli` applies it) turns a
/// lock held by a sibling into the typed [`IndexLockTimeout`] naming the
/// lock file — not an indefinite hang — and the same manager succeeds
/// once the holder releases.
#[test]
fn held_lock_times_out_typed_with_config_derived_deadline() {
    // 50ms via config — legitimate test configuration for a fast test.
    let settings = settings_with_deadline_ms(50);
    let deadline = resolve_index_lock_deadline(&settings, &ConfigOverrides::default()).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path()).with_index_lock_deadline(Some(deadline));

    // Hold the advisory lock the way a wedged sibling norn process would:
    // an independent file description holding the exclusive OS lock.
    let lock_path = tmp.path().join("index.lock");
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    holder.lock().unwrap();

    let options = || CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/w".to_owned(),
        name: None,
    };
    let err = manager
        .create(options(), DurabilityPolicy::Flush)
        .unwrap_err();

    match &err {
        SessionPersistError::IndexLockTimeout { path, waited } => {
            assert_eq!(*waited, Duration::from_millis(50));
            assert!(
                path.ends_with("index.lock"),
                "timeout must name the lock file, got {}",
                path.display(),
            );
        }
        other => panic!("expected IndexLockTimeout, got {other:?}"),
    }

    // The operator-facing message names the lock file, the elapsed
    // deadline, and the likely cause.
    let rendered = err.to_string();
    assert!(
        rendered.contains("index.lock"),
        "message names the lock file: {rendered}",
    );
    assert!(
        rendered.contains("50ms"),
        "message names the deadline: {rendered}",
    );
    assert!(
        rendered.contains("another norn process may be holding it"),
        "message points at the likely cause: {rendered}",
    );

    // Holder releases: the same config-derived deadline now acquires the
    // lock and the create succeeds — the deadline only fires on genuine
    // contention.
    holder.unlock().unwrap();
    drop(holder);
    let opened = manager.create(options(), DurabilityPolicy::Flush).unwrap();
    drop(opened);
}
