//! End-to-end disclosure fences for public session JSON output.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use norn::provider::ProviderStateIdentity;
use norn::session::{CreateSessionOptions, DurabilityPolicy, SessionManager};
use serde_json::Value;

fn norn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_norn")
}

fn run_norn(home: &Path, args: &[&str]) -> Result<Output, std::io::Error> {
    Command::new(norn_bin())
        .args(args)
        .env("NORN_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
}

fn contains_value(candidate: &Value, needle: &Value) -> bool {
    candidate == needle
        || match candidate {
            Value::Array(values) => values.iter().any(|value| contains_value(value, needle)),
            Value::Object(values) => values.values().any(|value| contains_value(value, needle)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
        }
}

#[test]
fn list_and_export_json_omit_durable_provider_identity() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let data_dir = home.path().join("session-store");
    let opened = SessionManager::new(&data_dir).create(
        CreateSessionOptions {
            model: "sentinel-model".to_owned(),
            working_dir: "/sentinel/worktree".to_owned(),
            name: Some("sentinel-session".to_owned()),
        },
        DurabilityPolicy::Flush,
    )?;
    let session_id = opened.entry.id.clone();
    let identity = ProviderStateIdentity::derive(
        "sentinel-provider-authority",
        b"sentinel-private-credential-identity",
    );
    opened
        .store
        .validate_or_bind_provider_state_identity(Some(identity))?;
    drop(opened);

    let durable = SessionManager::new(&data_dir).resolve(&session_id)?;
    assert_eq!(durable.provider_state_identity, Some(identity));
    let identity_json = serde_json::to_value(identity)?;

    let list = run_norn(
        home.path(),
        &["session", "list", "--all", "--format", "json"],
    )?;
    assert!(
        list.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_json: Value = serde_json::from_slice(&list.stdout)?;
    assert_eq!(list_json[0]["id"], Value::String(session_id.clone()));
    assert!(list_json[0].get("provider_state_identity").is_none());
    assert!(!contains_value(&list_json, &identity_json));

    let export = run_norn(
        home.path(),
        &["session", "export", &session_id, "--format", "json"],
    )?;
    assert!(
        export.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let export_json: Value = serde_json::from_slice(&export.stdout)?;
    assert_eq!(export_json["session"]["id"], Value::String(session_id));
    assert!(
        export_json["session"]
            .get("provider_state_identity")
            .is_none()
    );
    assert!(!contains_value(&export_json, &identity_json));
    Ok(())
}
