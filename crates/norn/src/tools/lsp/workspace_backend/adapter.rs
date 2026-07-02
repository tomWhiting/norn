//! [`WorkspaceLspBackend`]: the bridge from norn's [`LspBackend`] trait to
//! the workspace-scoped `lsp` crate API.
//!
//! Before every operation the adapter calls
//! `WorkspaceLspBackend::ensure_synced` to keep the language server's
//! view of open files current: any tracked file whose mtime has advanced
//! since the last sync is re-read from disk and pushed via `didChange`,
//! and any file the adapter has not seen before is opened with `didOpen`.
//! This catches edits made by other tools (`Edit`, `Bash`, `ApplyPatch`)
//! or external processes without requiring explicit notification.
//!
//! # Shared-workspace contract (LD-015 R2 / C64)
//!
//! When a single [`WorkspaceLspBackend`] is shared across multiple
//! workflow steps in the same worktree — the LD-015 wiring — the same
//! mtime-driven `ensure_synced` cascade fires automatically at the top
//! of each operation in step N+1. Edits step N made on disk
//! (via `Edit`, `Write`, `ApplyPatch`, or `Bash`) become visible to the
//! language server before step N+1's first LSP query, with no
//! per-step sync invocation from the executor. Deletions trigger a
//! `didClose`; newly-touched files are opened on first reference with
//! `version = 1` and their mtime captured for future diffs. Tracked files
//! whose stat/read fails, and files whose `didChange` push is rejected by
//! the server, are evicted (warn-logged, best-effort `didClose`) and
//! re-opened from disk on next access — sync bookkeeping only ever
//! advances after the server accepted the corresponding operation.
//! Stale diagnostic flushing across servers (flycheck control,
//! quiescence) is a separate LD-013 concern and is not handled here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use lsp::server::process::ServerProcess;
use lsp::workspace::LspWorkspace;

use super::super::backend::{
    LspBackend, LspBackendError, LspDiagnostic, LspHover, LspLocation, LspSymbol, TestRunnable,
};
use super::mapping::{
    METHOD_NOT_FOUND_CODE, MtimeResult, file_mtime, file_mtime_or_deleted, map_diagnostic,
    map_document_symbols, map_goto_response, map_hover, map_location, map_lsp_error, path_to_uri,
    read_file, retry_on_content_modified,
};
use super::runnables::{
    fallback_related_tests_via_callhierarchy, parse_experimental_runnables_response,
    parse_related_tests_response,
};

/// Timeout for rust-analyzer extension requests (`rust-analyzer/relatedTests`,
/// `experimental/runnables`). Documented default carried over from the
/// original `relatedTests` wiring; overridable timeouts for these extension
/// calls are not currently plumbed through the backend surface.
const RA_EXTENSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Mtime and version state for a tracked file.
struct TrackedFile {
    mtime: SystemTime,
    version: i32,
}

/// Freshness probe result for one tracked file, computed before any
/// bookkeeping or server state is mutated.
enum TrackedProbe {
    /// Mtime advanced: `mtime` was captured *before* `content` was read, so
    /// a write landing between the two produces content at least as new as
    /// the recorded mtime and the next sync re-pushes it.
    Stale {
        path: PathBuf,
        mtime: SystemTime,
        content: String,
    },
    /// The file no longer exists on disk.
    Deleted(PathBuf),
    /// Stat or read failed (permissions, I/O). The file is evicted so one
    /// broken file cannot poison every subsequent LSP call.
    Errored { path: PathBuf, reason: String },
}

/// Adapter that bridges [`LspWorkspace`] to norn's [`LspBackend`] trait.
///
/// Created once at runtime startup via [`build_lsp_backend`] and shared
/// behind an `Arc` so it can be injected into [`super::super::LspTool`].
pub struct WorkspaceLspBackend {
    workspace: Arc<LspWorkspace>,
    tracked: Mutex<HashMap<PathBuf, TrackedFile>>,
}

impl WorkspaceLspBackend {
    /// Wraps an existing [`LspWorkspace`] in the adapter.
    pub fn new(workspace: Arc<LspWorkspace>) -> Self {
        Self {
            workspace,
            tracked: Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the wrapped [`Arc<LspWorkspace>`] so callers that hold the
    /// concrete backend can recover the inner workspace handle.
    ///
    /// LD-015 R1 / R3: the TUI driver and workflow executor build a
    /// single `Arc<LspWorkspace>` per process / execution and wrap it in
    /// this adapter; downstream wiring (`build_diagnostic_infra`'s
    /// `lsp_workspace` slot, which feeds the `LspBridge`) needs to recover
    /// the same handle so the bridge observes the same diagnostic
    /// aggregator the backend keeps.
    #[must_use]
    pub fn workspace(&self) -> Arc<LspWorkspace> {
        Arc::clone(&self.workspace)
    }

    /// Ensure all tracked files are fresh and the target file is open.
    ///
    /// 1. Probes every previously tracked file (stat, then read for files
    ///    whose mtime advanced) without mutating any state.
    /// 2. Evicts deleted files (closed on the server) and files whose
    ///    stat/read failed (best-effort close, warn-logged) — one broken
    ///    file must not poison LSP calls for every other file.
    /// 3. Pushes stale content via `didChange`. Version and mtime
    ///    bookkeeping commits only *after* the server accepted the change;
    ///    on failure the entry is evicted (best-effort close) and the error
    ///    propagates, so the next call re-opens the document from disk
    ///    instead of treating the stale server view as fresh forever.
    /// 4. If the target `path` is not yet tracked, stats it, reads it from
    ///    disk, calls `open_document`, and starts tracking it.
    async fn ensure_synced(&self, path: &Path) -> Result<(), LspBackendError> {
        let mut tracked = self.tracked.lock().await;

        let mut probes: Vec<TrackedProbe> = Vec::new();
        for (tracked_path, entry) in tracked.iter() {
            match file_mtime_or_deleted(tracked_path) {
                Ok(MtimeResult::Ok(current_mtime)) => {
                    if current_mtime != entry.mtime {
                        match read_file(tracked_path).await {
                            Ok(content) => probes.push(TrackedProbe::Stale {
                                path: tracked_path.clone(),
                                mtime: current_mtime,
                                content,
                            }),
                            Err(e) => probes.push(TrackedProbe::Errored {
                                path: tracked_path.clone(),
                                reason: e.to_string(),
                            }),
                        }
                    }
                }
                Ok(MtimeResult::Deleted) => {
                    probes.push(TrackedProbe::Deleted(tracked_path.clone()));
                }
                Err(e) => probes.push(TrackedProbe::Errored {
                    path: tracked_path.clone(),
                    reason: e.to_string(),
                }),
            }
        }

        for probe in probes {
            match probe {
                TrackedProbe::Deleted(del_path) => {
                    tracked.remove(&del_path);
                    if let Err(e) = self.workspace.close_document(&del_path).await {
                        tracing::warn!(
                            path = %del_path.display(),
                            error = %e,
                            "failed to close deleted document on language server"
                        );
                    }
                }
                TrackedProbe::Errored {
                    path: err_path,
                    reason,
                } => {
                    tracing::warn!(
                        path = %err_path.display(),
                        reason = %reason,
                        "evicting tracked file after stat/read failure; it will be \
                         re-opened on next access"
                    );
                    tracked.remove(&err_path);
                    if let Err(e) = self.workspace.close_document(&err_path).await {
                        tracing::debug!(
                            path = %err_path.display(),
                            error = %e,
                            "best-effort close of evicted document failed"
                        );
                    }
                }
                TrackedProbe::Stale {
                    path: stale_path,
                    mtime,
                    content,
                } => {
                    if let Err(e) = self.workspace.update_document(&stale_path, &content).await {
                        tracing::warn!(
                            path = %stale_path.display(),
                            error = %e,
                            "didChange failed; evicting so the next access re-opens \
                             the document instead of leaving the server stale"
                        );
                        tracked.remove(&stale_path);
                        if let Err(close_err) = self.workspace.close_document(&stale_path).await {
                            tracing::debug!(
                                path = %stale_path.display(),
                                error = %close_err,
                                "best-effort close of evicted document failed"
                            );
                        }
                        return Err(map_lsp_error(e));
                    }
                    let entry = tracked.get_mut(&stale_path).ok_or_else(|| {
                        LspBackendError::ProtocolError {
                            reason: format!(
                                "tracked file disappeared during sync: {}",
                                stale_path.display()
                            ),
                        }
                    })?;
                    entry.version += 1;
                    entry.mtime = mtime;
                }
            }
        }

        if !tracked.contains_key(path) {
            // Stat before read: a write landing between the two yields
            // content at least as new as the recorded mtime, so the next
            // sync re-pushes it rather than mistaking it for fresh.
            let mtime = file_mtime(path)?;
            let content = read_file(path).await?;
            self.workspace
                .open_document(path, &content, 1)
                .await
                .map_err(map_lsp_error)?;
            tracked.insert(path.to_path_buf(), TrackedFile { mtime, version: 1 });
        }

        Ok(())
    }

    /// Resolve the registered language server for `path`, returning `None`
    /// when no configuration matches. Other LSP errors are mapped via
    /// [`map_lsp_error`] and returned as `Err`.
    async fn ra_server_for(
        &self,
        path: &Path,
    ) -> Result<Option<Arc<RwLock<ServerProcess>>>, LspBackendError> {
        match self.workspace.registry().server_for_file(path).await {
            Ok(server) => Ok(Some(server)),
            Err(lsp::error::LspError::Configuration(_)) => Ok(None),
            Err(e) => Err(map_lsp_error(e)),
        }
    }

    /// Gracefully shut down every language server the wrapped workspace
    /// manages, using the LSP `shutdown` request / `exit` notification
    /// handshake before the transport is closed.
    ///
    /// Embedders should call this at teardown. If the backend is instead
    /// dropped as the last holder of the workspace, [`Drop`] spawns the
    /// same handshake best-effort on the current runtime; without a
    /// runtime the transport's `kill_on_drop` reaps the server processes.
    pub async fn shutdown(&self) {
        self.workspace.shutdown().await;
    }
}

impl Drop for WorkspaceLspBackend {
    fn drop(&mut self) {
        // Other holders (e.g. the diagnostic bridge recovered via
        // `workspace()`) may still be using the servers; only auto-shutdown
        // as the last holder. A racing clone at this instant merely skips
        // the graceful path — kill_on_drop remains the backstop.
        if Arc::strong_count(&self.workspace) > 1 {
            tracing::debug!("LSP workspace still shared at backend drop; skipping auto-shutdown");
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::debug!(
                "no async runtime at backend drop; language servers reaped via kill_on_drop"
            );
            return;
        };
        let workspace = Arc::clone(&self.workspace);
        // Detach: the handshake is best-effort. If the runtime is already
        // winding down the task is dropped and kill_on_drop takes over.
        drop(handle.spawn(async move { workspace.shutdown().await }));
    }
}

#[async_trait]
impl LspBackend for WorkspaceLspBackend {
    async fn hover(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<LspHover>, LspBackendError> {
        self.ensure_synced(path).await?;
        let position = lsp_types::Position::new(line, column);
        let result = retry_on_content_modified(|| self.workspace.hover(path, position)).await?;
        Ok(result.map(|h| map_hover(h, path)))
    }

    async fn definition(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError> {
        self.ensure_synced(path).await?;
        let position = lsp_types::Position::new(line, column);
        let result =
            retry_on_content_modified(|| self.workspace.goto_definition(path, position)).await?;
        Ok(result.as_ref().map(map_goto_response).unwrap_or_default())
    }

    async fn references(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspBackendError> {
        self.ensure_synced(path).await?;
        let position = lsp_types::Position::new(line, column);
        let result =
            retry_on_content_modified(|| self.workspace.find_references(path, position, true))
                .await?;
        Ok(result
            .map(|locs| locs.iter().map(map_location).collect())
            .unwrap_or_default())
    }

    async fn symbols(&self, path: &Path) -> Result<Vec<LspSymbol>, LspBackendError> {
        self.ensure_synced(path).await?;
        let result = retry_on_content_modified(|| self.workspace.document_symbols(path)).await?;
        Ok(result
            .as_ref()
            .map(|r| map_document_symbols(r, path))
            .unwrap_or_default())
    }

    async fn diagnostics(&self, path: &Path) -> Result<Vec<LspDiagnostic>, LspBackendError> {
        self.ensure_synced(path).await?;
        let diags = self.workspace.diagnostics_for_file(path).await;
        Ok(diags.into_iter().map(map_diagnostic).collect())
    }

    async fn test_runnables(&self, path: &Path) -> Result<Vec<TestRunnable>, LspBackendError> {
        self.ensure_synced(path).await?;
        let Some(server) = self.ra_server_for(path).await? else {
            return Err(LspBackendError::NoServerForFile {
                path: path.display().to_string(),
            });
        };
        let guard = server.read().await;
        if guard.config().name() != "rust-analyzer" {
            tracing::debug!(
                server = guard.config().name(),
                path = %path.display(),
                "server exposes no test-runnable discovery source; reporting none"
            );
            return Ok(Vec::new());
        }
        let Some(client) = guard.client() else {
            return Err(LspBackendError::ProtocolError {
                reason: format!("server has no active client for {}", path.display()),
            });
        };
        let uri = path_to_uri(path)?;
        let params = serde_json::json!({ "textDocument": { "uri": uri.as_str() } });
        let resp = client
            .send_request(
                "experimental/runnables",
                Some(params),
                Some(RA_EXTENSION_REQUEST_TIMEOUT),
            )
            .await
            .map_err(map_lsp_error)?;
        if let Some(err) = &resp.error {
            if err.code == METHOD_NOT_FOUND_CODE {
                tracing::debug!(
                    path = %path.display(),
                    "server does not implement experimental/runnables; reporting none"
                );
                return Ok(Vec::new());
            }
            return Err(LspBackendError::ProtocolError {
                reason: format!("experimental/runnables error {}: {}", err.code, err.message),
            });
        }
        Ok(resp
            .result
            .as_ref()
            .map(parse_experimental_runnables_response)
            .unwrap_or_default())
    }

    async fn related_tests(
        &self,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<TestRunnable>, LspBackendError> {
        self.ensure_synced(path).await?;
        if let Some(server) = self.ra_server_for(path).await? {
            let guard = server.read().await;
            if guard.config().name() == "rust-analyzer"
                && let Some(client) = guard.client()
            {
                let uri = path_to_uri(path)?;
                let params = serde_json::json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": { "line": line, "character": column },
                });
                match client
                    .send_request(
                        "rust-analyzer/relatedTests",
                        Some(params),
                        Some(RA_EXTENSION_REQUEST_TIMEOUT),
                    )
                    .await
                {
                    Ok(resp) => {
                        if let Some(err) = &resp.error {
                            if err.code != METHOD_NOT_FOUND_CODE {
                                tracing::warn!(
                                    code = err.code,
                                    message = %err.message,
                                    "rust-analyzer/relatedTests returned error; falling back"
                                );
                            }
                        } else if let Some(val) = resp.result.as_ref() {
                            return Ok(parse_related_tests_response(val));
                        } else {
                            return Ok(Vec::new());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "rust-analyzer/relatedTests transport error; falling back"
                        );
                    }
                }
            }
        }
        fallback_related_tests_via_callhierarchy(&self.workspace, path, line, column).await
    }

    async fn run_flycheck(&self, path: &Path) -> Result<(), LspBackendError> {
        let Some(server) = self.ra_server_for(path).await? else {
            return Ok(());
        };
        let guard = server.read().await;
        if guard.config().name() != "rust-analyzer" {
            return Ok(());
        }
        let Some(client) = guard.client() else {
            return Ok(());
        };
        let uri = path_to_uri(path)?;
        let params = serde_json::json!({ "textDocument": { "uri": uri.as_str() } });
        if let Err(e) = client
            .send_notification("rust-analyzer/runFlycheck", Some(params))
            .await
        {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "rust-analyzer/runFlycheck notification failed"
            );
        }
        Ok(())
    }

    async fn clear_flycheck(&self) -> Result<(), LspBackendError> {
        let servers = self.workspace.registry().all_servers().await;
        for server in servers {
            let guard = server.read().await;
            if guard.config().name() != "rust-analyzer" {
                continue;
            }
            let Some(client) = guard.client() else {
                continue;
            };
            if let Err(e) = client
                .send_notification("rust-analyzer/clearFlycheck", Some(serde_json::json!({})))
                .await
            {
                tracing::warn!(
                    error = %e,
                    "rust-analyzer/clearFlycheck notification failed"
                );
            }
        }
        Ok(())
    }
}

/// Construct a [`WorkspaceLspBackend`] backed by an [`LspWorkspace`] with
/// all built-in language server configurations pre-registered.
pub fn build_lsp_backend() -> Arc<WorkspaceLspBackend> {
    let workspace = Arc::new(LspWorkspace::with_builtins());
    Arc::new(WorkspaceLspBackend::new(workspace))
}

#[cfg(test)]
impl WorkspaceLspBackend {
    /// Number of files currently tracked by [`Self::ensure_synced`].
    ///
    /// Test-only accessor used by the shared-workspace contract tests to
    /// observe bookkeeping mutations without exposing the underlying map.
    pub(crate) async fn tracked_count(&self) -> usize {
        self.tracked.lock().await.len()
    }

    /// Snapshot of the (version, mtime) pair currently tracked for `path`,
    /// or `None` when the file has not been ensured-synced yet.
    ///
    /// Test-only accessor used by the shared-workspace contract tests to
    /// verify that mtime-driven `didChange` advances the bookkeeping.
    pub(crate) async fn tracked_state(&self, path: &Path) -> Option<(i32, SystemTime)> {
        self.tracked
            .lock()
            .await
            .get(path)
            .map(|entry| (entry.version, entry.mtime))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn workspace_backend_is_send_sync_and_arc_upcasts_to_trait_object() {
        assert_send_sync::<WorkspaceLspBackend>();

        let workspace = Arc::new(LspWorkspace::new());
        let concrete = Arc::new(WorkspaceLspBackend::new(workspace));
        let _backend: Arc<dyn LspBackend> = concrete;
    }

    /// New backend starts with an empty tracked set — no files have been
    /// referenced yet so the shared-workspace contract has nothing to
    /// catch.
    #[tokio::test]
    async fn fresh_backend_tracks_no_files() {
        let workspace = Arc::new(LspWorkspace::new());
        let backend = WorkspaceLspBackend::new(workspace);
        assert_eq!(backend.tracked_count().await, 0);
    }

    /// The `workspace()` accessor returns an `Arc` that points at the
    /// same underlying allocation as the one passed to `new` — so the
    /// workflow executor / TUI driver can recover the inner handle from
    /// the wrapped backend (LD-015 R1 / R3).
    #[tokio::test]
    async fn workspace_accessor_returns_same_arc() {
        let workspace = Arc::new(LspWorkspace::new());
        let backend = WorkspaceLspBackend::new(Arc::clone(&workspace));
        let recovered = backend.workspace();
        assert!(
            Arc::ptr_eq(&workspace, &recovered),
            "workspace() must return the same Arc handed to new()"
        );
    }

    /// When `ensure_synced` cannot find a server config (extension not
    /// matched by any registered server, no rust-analyzer subprocess
    /// spawn-able), the call surfaces an error and the tracked set is
    /// left untouched. This guards the invariant that a half-tracked
    /// file never lingers in the bookkeeping when initial sync fails —
    /// step N+1 will re-attempt the open via the same code path.
    #[tokio::test]
    async fn ensure_synced_failure_leaves_tracked_set_empty() {
        let workspace = Arc::new(LspWorkspace::new());
        let backend = WorkspaceLspBackend::new(workspace);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("unrecognised.unknownext");
        std::fs::write(&path, "irrelevant").unwrap();

        let err = backend.diagnostics(&path).await;
        assert!(
            err.is_err(),
            "no server configured for .unknownext, ensure_synced must fail"
        );
        assert_eq!(
            backend.tracked_count().await,
            0,
            "failed ensure_synced must NOT leave a partially-tracked entry"
        );
        assert!(backend.tracked_state(&path).await.is_none());
    }

    #[tokio::test]
    async fn concurrent_ensure_synced_failures_do_not_race_tracking_state() {
        let workspace = Arc::new(LspWorkspace::new());
        let backend: Arc<dyn LspBackend> = Arc::new(WorkspaceLspBackend::new(workspace));

        let tmp = tempfile::tempdir().unwrap();
        let path = Arc::new(tmp.path().join("unrecognised.unknownext"));
        std::fs::write(&*path, "irrelevant").unwrap();

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let backend = Arc::clone(&backend);
            let path = Arc::clone(&path);
            tasks.push(tokio::spawn(
                async move { backend.diagnostics(&path).await },
            ));
        }

        for task in tasks {
            let result = task.await.expect("diagnostics task should not panic");
            assert!(
                result.is_err(),
                "no server configured for .unknownext, ensure_synced must fail"
            );
        }
    }
}
