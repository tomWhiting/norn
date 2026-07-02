//! Integration tests driving [`WorkspaceLspBackend`] against a scripted
//! stub language server that speaks real JSON-RPC over stdio.
//!
//! The stub (a small Python script) implements the `initialize` handshake,
//! document-sync notifications, `experimental/runnables`, and the
//! `shutdown`/`exit` teardown handshake, logging what it receives to a
//! sidecar file the tests assert against. Tests gate on `python3` being
//! available at runtime and skip (with a tracing line) when it is not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use lsp::server::config::ServerConfig;
use lsp::workspace::LspWorkspace;

use super::super::backend::{LspBackend, TestRunnableKind};
use super::adapter::WorkspaceLspBackend;

/// Stub language server: Content-Length framed JSON-RPC over stdio.
///
/// - `initialize` → full-sync capabilities.
/// - `experimental/runnables` → the JSON stored at `$NORN_STUB_RUNNABLES`
///   (empty array when unset).
/// - `shutdown` / `exit` → logged, acknowledged, process exits.
/// - Document-sync notifications are logged to `$NORN_STUB_LOG`.
/// - Unknown requests get a JSON-RPC `-32601` method-not-found error.
///
/// [`write_stub_script`] prepends a shebang pointing at the interpreter
/// [`find_python3`] located, so a broken `python3` on `$PATH` cannot
/// hijack the script.
const STUB_SERVER_SOURCE: &str = r#"import json, os, sys


def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        stripped = line.strip()
        if not stripped:
            break
        if stripped.lower().startswith(b"content-length:"):
            length = int(stripped.split(b":", 1)[1])
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))


def send(payload):
    data = json.dumps(payload).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def log(line):
    path = os.environ.get("NORN_STUB_LOG")
    if path:
        with open(path, "a") as f:
            f.write(line + "\n")


while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid,
              "result": {"capabilities": {"textDocumentSync": 1}}})
    elif method == "shutdown":
        log("shutdown")
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "exit":
        log("exit")
        break
    elif method == "experimental/runnables":
        runnables_path = os.environ.get("NORN_STUB_RUNNABLES")
        result = []
        if runnables_path and os.path.exists(runnables_path):
            with open(runnables_path) as f:
                result = json.load(f)
        send({"jsonrpc": "2.0", "id": mid, "result": result})
    elif method == "textDocument/didOpen":
        text = msg["params"]["textDocument"]["text"]
        log("didOpen:" + text.replace("\n", "\\n"))
    elif method == "textDocument/didChange":
        log("didChange")
    elif method == "textDocument/didClose":
        log("didClose")
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid,
              "error": {"code": -32601, "message": "method not found"}})
"#;

/// Test-only server config pointing the registry at the stub script.
///
/// Named `rust-analyzer` so the adapter's rust-analyzer extension paths
/// (`experimental/runnables`) engage.
struct StubServerConfig {
    binary: &'static str,
    env: Vec<(String, String)>,
}

impl ServerConfig for StubServerConfig {
    fn name(&self) -> &'static str {
        "rust-analyzer"
    }
    fn binary(&self) -> &'static str {
        self.binary
    }
    fn language_ids(&self) -> Vec<String> {
        vec!["rust".to_owned()]
    }
    fn file_patterns(&self) -> Vec<String> {
        vec!["*.rs".to_owned()]
    }
    fn root_markers(&self) -> Vec<String> {
        vec!["Cargo.toml".to_owned()]
    }
    fn env(&self) -> Vec<(String, String)> {
        self.env.clone()
    }
}

/// Locate a runnable `python3`, preferring well-known absolute paths.
///
/// The bare `python3` on `$PATH` is probed last because developer PATHs
/// can shadow it with non-native binaries; every candidate is verified by
/// actually executing `--version`.
fn find_python3() -> Option<PathBuf> {
    for candidate in [
        "/usr/bin/python3",
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "python3",
    ] {
        let works = std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success());
        if works {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

/// Everything one stub-backed test needs: the workspace root, the script
/// path (deletable/restorable to simulate relaunch failure), the resolved
/// interpreter, the sidecar log, the runnables fixture path, and the
/// wired backend.
struct StubFixture {
    dir: tempfile::TempDir,
    script: PathBuf,
    python: PathBuf,
    log: PathBuf,
    runnables: PathBuf,
    backend: Arc<WorkspaceLspBackend>,
}

fn write_stub_script(path: &Path, python: &Path) {
    let source = format!("#!{}\n{STUB_SERVER_SOURCE}", python.display());
    std::fs::write(path, source).expect("write stub script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub script");
    }
}

fn stub_fixture(python: &Path) -> StubFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("stub-lsp.py");
    let log = dir.path().join("stub.log");
    let runnables = dir.path().join("runnables.json");
    write_stub_script(&script, python);
    // Root marker so the registry detects the tempdir as workspace root.
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"stub\"\n",
    )
    .expect("write marker");

    let leaked: &'static str = Box::leak(script.display().to_string().into_boxed_str());
    let config = StubServerConfig {
        binary: leaked,
        env: vec![
            ("NORN_STUB_LOG".to_owned(), log.display().to_string()),
            (
                "NORN_STUB_RUNNABLES".to_owned(),
                runnables.display().to_string(),
            ),
        ],
    };

    let mut workspace = LspWorkspace::new();
    workspace.register_server(Box::new(config));
    let backend = Arc::new(WorkspaceLspBackend::new(Arc::new(workspace)));

    StubFixture {
        dir,
        script,
        python: python.to_path_buf(),
        log,
        runnables,
        backend,
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Poll the stub's sidecar log until `needle` appears.
///
/// Notifications (didOpen/didChange) have no response, so nothing in the
/// client API orders the stub's log write with respect to the test body —
/// assertions on notification receipt must poll.
async fn wait_for_log(path: &Path, needle: &str) -> bool {
    for _ in 0..200 {
        if read_log(path).contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Bump a file's mtime far enough forward that `ensure_synced` must see it
/// as stale regardless of filesystem timestamp granularity.
fn advance_mtime(path: &Path, by: Duration) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime bump");
    file.set_modified(SystemTime::now() + by)
        .expect("set_modified");
}

const SKIP_MSG: &str = "skipping stub LSP test: python3 not available";

// ─── test_runnables via experimental/runnables (R2) ────────────────────

#[tokio::test]
async fn stub_test_runnables_discovers_and_classifies_scopes() {
    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file = fx.dir.path().join("lib.rs");
    std::fs::write(&file, "fn covered() {}\n").expect("write source");

    let file_uri = url::Url::from_file_path(&file).expect("file uri");
    let runnables = serde_json::json!([
        {
            "label": "test tests::unit_case",
            "kind": "cargo",
            "location": {
                "targetUri": file_uri.as_str(),
                "targetSelectionRange": {
                    "start": { "line": 4, "character": 7 },
                    "end": { "line": 4, "character": 16 }
                }
            },
            "args": {
                "workspaceRoot": fx.dir.path().display().to_string(),
                "cwd": fx.dir.path().display().to_string(),
                "cargoArgs": ["test", "--package", "stub"],
                "executableArgs": ["tests::unit_case", "--exact"]
            }
        },
        {
            "label": "test-mod tests",
            "kind": "cargo",
            "args": { "cargoArgs": ["test", "--package", "stub"], "executableArgs": ["tests"] }
        },
        {
            "label": "doctest covered",
            "kind": "cargo",
            "args": { "cargoArgs": ["test", "--doc", "--package", "stub"], "executableArgs": [] }
        },
        {
            "label": "run stub",
            "kind": "cargo",
            "args": { "cargoArgs": ["run", "--package", "stub"], "executableArgs": [] }
        }
    ]);
    std::fs::write(
        &fx.runnables,
        serde_json::to_vec(&runnables).expect("serialize"),
    )
    .expect("write runnables fixture");

    let discovered = fx
        .backend
        .test_runnables(&file)
        .await
        .expect("test_runnables succeeds against stub");

    assert_eq!(
        discovered.len(),
        3,
        "run-flavoured runnable must be filtered out: {discovered:?}"
    );
    assert_eq!(discovered[0].kind, TestRunnableKind::Test);
    assert_eq!(discovered[0].label, "test tests::unit_case");
    assert_eq!(
        discovered[0].cargo_args,
        vec!["test", "--package", "stub"],
        "cargo args must survive for the executor"
    );
    assert_eq!(
        discovered[0].executable_args,
        vec!["tests::unit_case", "--exact"]
    );
    let loc = discovered[0].location.as_ref().expect("location");
    assert_eq!(
        (loc.line, loc.column),
        (5, 8),
        "locations must be one-based"
    );
    assert_eq!(discovered[1].kind, TestRunnableKind::TestModule);
    assert_eq!(discovered[2].kind, TestRunnableKind::DocTest);

    // The document was really opened on the server before discovery.
    assert!(
        read_log(&fx.log).contains("didOpen:fn covered() {}"),
        "stub must have received didOpen before experimental/runnables"
    );

    fx.backend.shutdown().await;
}

#[tokio::test]
async fn stub_test_runnables_empty_response_reports_no_tests() {
    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file = fx.dir.path().join("lib.rs");
    std::fs::write(&file, "fn nothing() {}\n").expect("write source");
    // No runnables fixture written — the stub answers with an empty array.

    let discovered = fx
        .backend
        .test_runnables(&file)
        .await
        .expect("test_runnables succeeds");
    assert!(discovered.is_empty());

    fx.backend.shutdown().await;
}

// ─── Sync bookkeeping commits only after didChange succeeds (R3) ───────

#[tokio::test]
async fn failed_did_change_never_commits_bookkeeping_and_recovers_on_next_access() {
    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file = fx.dir.path().join("main.rs");
    std::fs::write(&file, "fn v1() {}\n").expect("write v1");

    fx.backend
        .diagnostics(&file)
        .await
        .expect("initial sync opens the document");
    let (version, initial_mtime) = fx
        .backend
        .tracked_state(&file)
        .await
        .expect("file tracked after open");
    assert_eq!(version, 1);

    // Edit the file on disk so the next sync must push a didChange…
    std::fs::write(&file, "fn v2() {}\n").expect("write v2");
    advance_mtime(&file, Duration::from_secs(5));

    // …then take the server down and make relaunch fail: graceful-stop the
    // running process and delete the stub binary so crash recovery cannot
    // respawn it.
    let server = fx
        .backend
        .workspace()
        .registry()
        .server_for_file(&file)
        .await
        .expect("server running");
    server
        .write()
        .await
        .shutdown()
        .await
        .expect("stub shuts down");
    std::fs::remove_file(&fx.script).expect("delete stub script");

    let err = fx
        .backend
        .diagnostics(&file)
        .await
        .expect_err("didChange against a dead, unrestartable server must fail");
    tracing::info!(error = %err, "expected sync failure");

    // The original bug: version/mtime were bumped BEFORE update_document,
    // so a failed didChange marked the server view fresh forever
    // (version 2, new mtime, stale server). The entry must instead be
    // evicted with nothing committed, so the next access re-opens from
    // disk.
    let stale_committed = fx
        .backend
        .tracked_state(&file)
        .await
        .is_some_and(|(v, m)| v > 1 || m > initial_mtime);
    assert!(
        !stale_committed,
        "bookkeeping advanced past a failed didChange"
    );
    assert!(
        fx.backend.tracked_state(&file).await.is_none(),
        "failed didChange must evict, never commit bookkeeping"
    );

    // Transient failure clears: restore the stub and access the file
    // again. The document must be re-opened with the CURRENT disk content
    // — the server is not left permanently stale.
    write_stub_script(&fx.script, &fx.python);
    fx.backend
        .diagnostics(&file)
        .await
        .expect("sync succeeds once the server is restartable");
    let (version, _) = fx
        .backend
        .tracked_state(&file)
        .await
        .expect("file re-tracked");
    assert_eq!(version, 1, "re-opened fresh, not resumed from stale state");
    assert!(
        wait_for_log(&fx.log, "didOpen:fn v2() {}").await,
        "server must have received the v2 content after recovery"
    );

    fx.backend.shutdown().await;
}

// ─── One broken tracked file must not poison other calls (R4) ──────────

#[cfg(unix)]
#[tokio::test]
async fn unreadable_tracked_file_is_evicted_without_poisoning_other_calls() {
    use std::os::unix::fs::PermissionsExt;

    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file_a = fx.dir.path().join("a.rs");
    let file_b = fx.dir.path().join("b.rs");
    std::fs::write(&file_a, "fn a() {}\n").expect("write a");
    std::fs::write(&file_b, "fn b() {}\n").expect("write b");

    fx.backend.diagnostics(&file_a).await.expect("track a");
    fx.backend.diagnostics(&file_b).await.expect("track b");
    assert_eq!(fx.backend.tracked_count().await, 2);

    // Make b stale AND unreadable: stat succeeds, read fails.
    advance_mtime(&file_b, Duration::from_secs(5));
    std::fs::set_permissions(&file_b, std::fs::Permissions::from_mode(0o000)).expect("chmod 000 b");
    if std::fs::read_to_string(&file_b).is_ok() {
        tracing::info!("skipping: running as a user that bypasses file permissions");
        return;
    }

    // Before the fix this ?-propagated and failed EVERY subsequent call.
    fx.backend
        .diagnostics(&file_a)
        .await
        .expect("call for a healthy file must not fail because b is unreadable");

    assert!(
        fx.backend.tracked_state(&file_b).await.is_none(),
        "unreadable file must be evicted from tracking"
    );
    assert!(
        fx.backend.tracked_state(&file_a).await.is_some(),
        "healthy file stays tracked"
    );

    // Restore permissions: the file becomes usable again on next access.
    std::fs::set_permissions(&file_b, std::fs::Permissions::from_mode(0o644))
        .expect("restore perms");
    fx.backend
        .diagnostics(&file_b)
        .await
        .expect("evicted file re-opens once readable again");
    assert!(fx.backend.tracked_state(&file_b).await.is_some());

    fx.backend.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn unstattable_tracked_file_is_evicted_without_poisoning_other_calls() {
    use std::os::unix::fs::PermissionsExt;

    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let subdir = fx.dir.path().join("locked");
    std::fs::create_dir(&subdir).expect("mkdir");
    let file_a = fx.dir.path().join("a.rs");
    let file_c = subdir.join("c.rs");
    std::fs::write(&file_a, "fn a() {}\n").expect("write a");
    std::fs::write(&file_c, "fn c() {}\n").expect("write c");

    fx.backend.diagnostics(&file_a).await.expect("track a");
    fx.backend.diagnostics(&file_c).await.expect("track c");

    // Remove traversal rights on the directory: stat on c now fails
    // (EACCES), which is neither "fresh" nor "deleted".
    std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000 dir");
    let stat_fails = std::fs::metadata(&file_c).is_err();
    if !stat_fails {
        std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755))
            .expect("restore perms");
        tracing::info!("skipping: running as a user that bypasses directory permissions");
        return;
    }

    let result = fx.backend.diagnostics(&file_a).await;
    // Restore before asserting so the tempdir can always be cleaned up.
    std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755))
        .expect("restore perms");

    result.expect("call for a healthy file must not fail because c is unstattable");
    assert!(
        fx.backend.tracked_state(&file_c).await.is_none(),
        "unstattable file must be evicted from tracking"
    );
    assert!(fx.backend.tracked_state(&file_a).await.is_some());

    fx.backend.shutdown().await;
}

// ─── Graceful shutdown handshake (R6) ──────────────────────────────────

#[tokio::test]
async fn explicit_shutdown_performs_lsp_handshake() {
    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file = fx.dir.path().join("lib.rs");
    std::fs::write(&file, "fn x() {}\n").expect("write source");
    fx.backend.diagnostics(&file).await.expect("server starts");

    fx.backend.shutdown().await;

    // The `shutdown` request is awaited by the handshake, so by the time
    // `shutdown()` returns the stub has logged it. (`exit` delivery races
    // the final process kill, so only the request is asserted.)
    assert!(
        read_log(&fx.log).contains("shutdown"),
        "server must receive the LSP shutdown request, not just SIGKILL"
    );
}

#[tokio::test]
async fn dropping_last_backend_handle_triggers_graceful_shutdown() {
    let Some(python) = find_python3() else {
        tracing::info!("{SKIP_MSG}");
        return;
    };
    let fx = stub_fixture(&python);
    let file = fx.dir.path().join("lib.rs");
    std::fs::write(&file, "fn x() {}\n").expect("write source");
    fx.backend.diagnostics(&file).await.expect("server starts");

    let StubFixture {
        dir, log, backend, ..
    } = fx;
    drop(backend);

    // Drop spawns the handshake as a detached task on this runtime; poll
    // the stub log until it lands.
    assert!(
        wait_for_log(&log, "shutdown").await,
        "dropping the last backend handle must run the shutdown/exit handshake"
    );
    drop(dir);
}
