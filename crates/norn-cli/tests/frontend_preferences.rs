//! Real-process settings preservation; these helper barriers are not CLI restart proof.

#[path = "support/frontend_preferences.rs"]
mod fixture;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fixture::{Barrier, Mutation, Observation, Scope, TestResult, Worker};
use serde_json::{Value, json};

#[test]
fn frontend_preferences_child_entrypoint() -> TestResult {
    fixture::child_entrypoint()
}

#[tokio::test]
async fn user_mcp_and_frontend_overlap_preserves_both_publication_orders() -> TestResult {
    preserve_both_orders(Scope::User).await
}

#[tokio::test]
async fn local_mcp_and_frontend_overlap_preserves_both_publication_orders() -> TestResult {
    preserve_both_orders(Scope::Local).await
}

#[tokio::test]
async fn user_stale_owned_key_refuses_without_overwriting_other_process() -> TestResult {
    stale_conflict(Scope::User).await
}

#[tokio::test]
async fn local_stale_owned_key_refuses_without_overwriting_other_process() -> TestResult {
    stale_conflict(Scope::Local).await
}

#[tokio::test]
async fn child_configuration_failure_reports_exit_and_stderr() -> TestResult {
    let sandbox = Sandbox::new(Scope::User)?;
    let mut invalid = sandbox.initial.clone();
    invalid["mcp_servers"]["existing"]["future"] = json!("invalid-owned-field");
    std::fs::write(&sandbox.path, serde_json::to_vec(&invalid)?)?;
    let mut worker =
        Worker::spawn(&sandbox.home, &sandbox.root, Scope::User, Mutation::Mcp).await?;
    let error = match worker.receive().await {
        Err(error) => error.to_string(),
        Ok(observation) => {
            return Err(format!("invalid MCP definition emitted {observation:?}").into());
        }
    };
    assert!(error.contains("closed its barrier"), "{error}");
    assert!(error.contains("exit status: 101"), "{error}");
    assert!(error.contains("stdout:"), "{error}");
    assert!(error.contains("stderr: Error:"), "{error}");
    assert!(error.contains("unknown field `future`"), "{error}");
    assert!(
        error.contains(&sandbox.path.display().to_string()),
        "{error}"
    );
    Ok(())
}

async fn preserve_both_orders(scope: Scope) -> TestResult {
    for mcp_first in [true, false] {
        let sandbox = Sandbox::new(scope)?;
        let mut frontend = Worker::spawn(
            &sandbox.home,
            &sandbox.root,
            scope,
            frontend(&json!({"changes_open":true}))?,
        )
        .await?;
        let mut mcp = Worker::spawn(&sandbox.home, &sandbox.root, scope, Mutation::Mcp).await?;
        let frontend_pid = ready(&mut frontend, &sandbox).await?;
        let mcp_pid = ready(&mut mcp, &sandbox).await?;
        assert_ne!(frontend_pid, mcp_pid);
        assert_ne!(frontend_pid, std::process::id());
        assert_ne!(mcp_pid, std::process::id());
        // Both processes have captured the same initial settings before either
        // publishes. This is deterministic stale-snapshot overlap, not a claim
        // that an internal writer lock acquisition was observed.
        let lock = sandbox.lock(scope)?;
        lock.try_lock()?;
        for worker in [&mut frontend, &mut mcp] {
            worker.send(Barrier::ObserveContendedLock).await?;
            match worker.receive().await? {
                Observation::Contended { path } => {
                    assert_eq!(path, scope.lock_path(&sandbox.home, &sandbox.root));
                }
                other => {
                    return Err(format!(
                        "expected physical settings lock contention, got {other:?}"
                    )
                    .into());
                }
            }
        }
        assert_eq!(sandbox.document()?, sandbox.initial);
        lock.unlock()?;
        let (first, second) = if mcp_first {
            (&mut mcp, &mut frontend)
        } else {
            (&mut frontend, &mut mcp)
        };
        publish(first, &sandbox.path, true).await?;
        publish(second, &sandbox.path, true).await?;
        let mut expected = sandbox.initial.clone();
        expected["tui"]["view"] = json!({"changes_open":true});
        expected["mcp_servers"]["parallel-fixture"] = json!({
            "command":"fixture-server-not-launched", "enabled":false
        });
        assert_eq!(
            sandbox.document()?,
            expected,
            "lost an owned or unrelated key; MCP first: {mcp_first}"
        );
        // Returned frontend authority advances its baseline; an identical save
        // is unchanged even after the other process's independent MCP patch.
        let bytes = std::fs::read(&sandbox.path)?;
        publish(&mut frontend, &sandbox.path, false).await?;
        assert_eq!(std::fs::read(&sandbox.path)?, bytes);
        frontend.finish().await?;
        mcp.finish().await?;
    }
    Ok(())
}

async fn stale_conflict(scope: Scope) -> TestResult {
    let sandbox = Sandbox::new(scope)?;
    let mut first = Worker::spawn(
        &sandbox.home,
        &sandbox.root,
        scope,
        frontend(&json!({"changes_open":true}))?,
    )
    .await?;
    let mut stale = Worker::spawn(
        &sandbox.home,
        &sandbox.root,
        scope,
        frontend(&json!({"changes_open":false,"expanded_tools":true}))?,
    )
    .await?;
    assert_ne!(
        ready(&mut first, &sandbox).await?,
        ready(&mut stale, &sandbox).await?
    );
    publish(&mut first, &sandbox.path, true).await?;
    let committed = std::fs::read(&sandbox.path)?;
    stale.send(Barrier::Publish).await?;
    match stale.receive().await? {
        Observation::Conflict { path, key } => {
            assert_eq!(path, sandbox.path);
            assert_eq!(key, "view");
        }
        other => return Err(format!("expected named stale-key refusal, got {other:?}").into()),
    }
    assert_eq!(std::fs::read(&sandbox.path)?, committed);
    let mut expected = sandbox.initial.clone();
    expected["tui"]["view"] = json!({"changes_open":true});
    assert_eq!(sandbox.document()?, expected);
    first.finish().await?;
    stale.finish().await
}

fn frontend(view: &Value) -> TestResult<Mutation> {
    let replacement = json!({"view":view})
        .as_object()
        .cloned()
        .ok_or("frontend fixture replacement is not an object")?;
    Ok(Mutation::Frontend {
        owned: vec!["view".to_owned()],
        replacement,
    })
}

async fn ready(worker: &mut Worker, sandbox: &Sandbox) -> TestResult<u32> {
    match worker.receive().await? {
        Observation::Ready {
            pid,
            path,
            original,
        } => {
            assert_eq!(path, sandbox.path);
            assert_eq!(original.as_ref(), sandbox.initial.get("tui"));
            Ok(pid)
        }
        other => Err(format!("expected snapshot barrier, got {other:?}").into()),
    }
}

async fn publish(worker: &mut Worker, path: &Path, expected_changed: bool) -> TestResult {
    worker.send(Barrier::Publish).await?;
    match worker.receive().await? {
        Observation::Published {
            path: actual,
            changed,
            ..
        } => {
            assert_eq!(actual, path);
            assert_eq!(changed, expected_changed);
            Ok(())
        }
        other => Err(format!("expected settings publication, got {other:?}").into()),
    }
}

struct Sandbox {
    directory: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    path: PathBuf,
    initial: Value,
}

impl Sandbox {
    fn new(scope: Scope) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let base = directory.path().canonicalize()?;
        let home = base.join("home");
        let root = base.join("workspace");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&root)?;
        std::fs::create_dir(root.join(".norn"))?;
        let path = scope.path(&home, &root);
        let initial = json!({
            "future_document":{"kept":[1,2,3]},
            "tui":{
                "view":{"changes_open":false},
                "composer":{"send_key":"fixture-original"},
                "future_frontend":{"kept":true},
                "input":{"submit_mode":"steer"}
            },
            "mcp_servers":{"existing":{
                "command":"never-started", "enabled":false,
                "env":{"NFP_EXISTING_SENTINEL":"retained"}
            }}
        });
        std::fs::write(&path, serde_json::to_vec(&initial)?)?;
        Ok(Self {
            directory,
            home,
            root,
            path,
            initial,
        })
    }

    fn document(&self) -> TestResult<Value> {
        assert!(self.directory.path().is_dir());
        Ok(serde_json::from_slice(&std::fs::read(&self.path)?)?)
    }

    fn lock(&self, scope: Scope) -> TestResult<File> {
        let path = scope.lock_path(&self.home, &self.root);
        match scope {
            Scope::User => Ok(OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?),
            Scope::Local => Ok(File::open(path)?),
        }
    }
}
