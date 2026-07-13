//! Session variable system — declarative, scriptable, with shell evaluation
//! at prompt-construction time.
//!
//! A [`SessionVariable`] has one of three sources: a static string, a shell
//! command (with optional TTL cache), or an in-process closure. Variables
//! are resolved through a [`VariableStore`], which caches shell results to
//! avoid re-executing commands within a TTL window. [`expand`] substitutes
//! `{{name}}` placeholders in arbitrary text using the resolved values.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::process::Command;
use uuid::Uuid;

use crate::error::IntegrationError;

/// Source that produces a variable's value at resolution time.
#[derive(Clone)]
pub enum VariableSource {
    /// A literal value, returned as-is.
    Static {
        /// The fixed value.
        value: String,
    },
    /// A shell command executed via `sh -c`; trimmed stdout is the value.
    Shell {
        /// The command string.
        command: String,
        /// Optional TTL during which a successful resolve is cached. `None`
        /// re-executes the command on every resolve.
        cache_ttl: Option<Duration>,
    },
    /// An in-process closure invoked each resolve.
    Computed {
        /// The producer closure.
        func: Arc<dyn Fn() -> String + Send + Sync>,
    },
}

impl std::fmt::Debug for VariableSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static { value } => f.debug_struct("Static").field("value", value).finish(),
            Self::Shell { command, cache_ttl } => f
                .debug_struct("Shell")
                .field("command", command)
                .field("cache_ttl", cache_ttl)
                .finish(),
            Self::Computed { .. } => f.debug_struct("Computed").finish_non_exhaustive(),
        }
    }
}

/// A named session variable.
#[derive(Clone, Debug)]
pub struct SessionVariable {
    /// Variable name (used for `{{name}}` substitution).
    pub name: String,
    /// Source that produces the value.
    pub source: VariableSource,
}

/// Default wall-clock budget for shell-variable execution.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Built-in variable: a fresh per-process session ID.
const BUILTIN_SESSION_ID: &str = "session_id";
/// Built-in variable: process working directory at resolve time.
const BUILTIN_WORKING_DIR: &str = "working_dir";
/// Built-in variable: process `$HOME` at resolve time.
const BUILTIN_HOME_DIR: &str = "home_dir";

#[derive(Clone)]
struct CachedShell {
    value: String,
    expires_at: Option<Instant>,
}

/// Stores variables and resolves them on demand, caching shell results.
pub struct VariableStore {
    variables: Mutex<HashMap<String, SessionVariable>>,
    shell_cache: Mutex<HashMap<String, CachedShell>>,
    session_id: String,
    /// Per-agent working directory used by shell-source variables and the
    /// `working_dir` builtin. Shared interior slot (rather than a plain
    /// field) so the `working_dir` builtin closure registered by
    /// [`Self::with_builtins`] observes a handle installed *later* via
    /// [`Self::with_working_dir`] — and tracks every subsequent
    /// [`crate::tool::context::SharedWorkingDir::set`] (e.g. a `bash` `cd`)
    /// live. When no handle is installed, shell variables inherit the
    /// process CWD — legacy behaviour.
    working_dir: Arc<Mutex<Option<crate::tool::context::SharedWorkingDir>>>,
}

impl VariableStore {
    /// Construct an empty store with no built-in variables registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            variables: Mutex::new(HashMap::new()),
            shell_cache: Mutex::new(HashMap::new()),
            session_id: Uuid::new_v4().to_string(),
            working_dir: Arc::new(Mutex::new(None)),
        }
    }

    /// Install the agent's shared working directory. Shell-source variables
    /// then run with this as their child's CWD; the `working_dir` builtin
    /// returns its current value rather than the process CWD. Effective
    /// regardless of whether it is called before or after
    /// [`Self::with_builtins`] registered the builtin closures.
    #[must_use]
    pub fn with_working_dir(self, working_dir: crate::tool::context::SharedWorkingDir) -> Self {
        *self.working_dir.lock() = Some(working_dir);
        self
    }

    /// Construct a store pre-populated with built-in variables:
    /// `session_id`, `working_dir`, and `home_dir`.
    ///
    /// The `working_dir` builtin reads the live value of this store's
    /// installed [`crate::tool::context::SharedWorkingDir`] at resolve time
    /// when present; otherwise it falls back to the process CWD at resolve
    /// time.
    #[must_use]
    pub fn with_builtins() -> Self {
        let store = Self::new();
        let session_id = store.session_id.clone();
        store.set(SessionVariable {
            name: BUILTIN_SESSION_ID.to_owned(),
            source: VariableSource::Static { value: session_id },
        });
        let wd_slot = Arc::clone(&store.working_dir);
        store.set(SessionVariable {
            name: BUILTIN_WORKING_DIR.to_owned(),
            source: VariableSource::Computed {
                func: Arc::new(move || {
                    let handle = wd_slot.lock().clone();
                    match handle {
                        Some(handle) => handle.get().to_string_lossy().into_owned(),
                        None => std::env::current_dir()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    }
                }),
            },
        });
        store.set(SessionVariable {
            name: BUILTIN_HOME_DIR.to_owned(),
            source: VariableSource::Computed {
                func: Arc::new(|| std::env::var("HOME").unwrap_or_default()),
            },
        });
        store
    }

    /// Returns the stable ID assigned to this variable store's session.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Replace the store's session ID with a caller-supplied one (e.g. the
    /// persisted session's index-entry ID), updating the `session_id`
    /// builtin variable when it is registered so `{{session_id}}`
    /// substitution and [`Self::session_id`] always agree.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        if self.variables.lock().contains_key(BUILTIN_SESSION_ID) {
            self.set(SessionVariable {
                name: BUILTIN_SESSION_ID.to_owned(),
                source: VariableSource::Static {
                    value: self.session_id.clone(),
                },
            });
        }
        self
    }

    /// Insert or replace a variable.
    pub fn set(&self, variable: SessionVariable) {
        let name = variable.name.clone();
        // Replacing the definition invalidates any cached shell value.
        self.shell_cache.lock().remove(&name);
        self.variables.lock().insert(name, variable);
    }

    /// Remove a variable. Returns the prior definition if any.
    pub fn remove(&self, name: &str) -> Option<SessionVariable> {
        self.shell_cache.lock().remove(name);
        self.variables.lock().remove(name)
    }

    /// True when no variables are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.variables.lock().is_empty()
    }

    /// Number of variables registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.variables.lock().len()
    }

    /// Resolve a variable by name. Returns the current value (executing the
    /// shell command or computed closure if applicable). Cached shell values
    /// within their TTL are returned without re-execution.
    ///
    /// # Errors
    ///
    /// Returns [`IntegrationError::HookError`] when the variable is unknown
    /// or when shell execution fails (timeout, non-zero exit, spawn error).
    pub async fn resolve(&self, name: &str) -> Result<String, IntegrationError> {
        let variable = {
            let guard = self.variables.lock();
            guard
                .get(name)
                .cloned()
                .ok_or_else(|| IntegrationError::HookError {
                    reason: format!("variable not registered: {name}"),
                })?
        };

        match &variable.source {
            VariableSource::Static { value } => Ok(value.clone()),
            VariableSource::Computed { func } => Ok(func()),
            VariableSource::Shell { command, cache_ttl } => {
                if let Some(cached) = self.cached_value(name) {
                    return Ok(cached);
                }
                let working_dir = self.working_dir.lock().clone();
                let output = run_shell(command, working_dir.as_ref()).await?;
                if let Some(ttl) = cache_ttl {
                    self.cache_value(name.to_owned(), output.clone(), Some(*ttl));
                }
                Ok(output)
            }
        }
    }

    /// Resolve every registered variable, returning a snapshot map. Shell
    /// failures propagate as the first error encountered.
    ///
    /// # Errors
    ///
    /// Returns the first [`IntegrationError`] encountered while resolving
    /// any variable.
    pub async fn resolve_all(&self) -> Result<HashMap<String, String>, IntegrationError> {
        let names: Vec<String> = self.variables.lock().keys().cloned().collect();
        let mut out = HashMap::with_capacity(names.len());
        for name in names {
            let value = self.resolve(&name).await?;
            out.insert(name, value);
        }
        Ok(out)
    }

    fn cached_value(&self, name: &str) -> Option<String> {
        let guard = self.shell_cache.lock();
        let entry = guard.get(name)?;
        match entry.expires_at {
            Some(deadline) if deadline <= Instant::now() => None,
            _ => Some(entry.value.clone()),
        }
    }

    fn cache_value(&self, name: String, value: String, ttl: Option<Duration>) {
        let expires_at = ttl.map(|d| Instant::now() + d);
        self.shell_cache
            .lock()
            .insert(name, CachedShell { value, expires_at });
    }
}

impl Default for VariableStore {
    fn default() -> Self {
        Self::new()
    }
}

async fn run_shell(
    command: &str,
    working_dir: Option<&crate::tool::context::SharedWorkingDir>,
) -> Result<String, IntegrationError> {
    let governor = crate::resource::DescriptorGovernor::global().map_err(|error| {
        IntegrationError::HookError {
            reason: format!("shell variable descriptor admission unavailable: {error}"),
        }
    })?;
    let _permit = governor
        .try_acquire(crate::resource::TWO_PIPE_SPAWN_PEAK)
        .map_err(|error| IntegrationError::HookError {
            reason: format!("shell variable descriptor admission failed: {error}"),
        })?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).kill_on_drop(true);
    if let Some(wd) = working_dir {
        cmd.current_dir(wd.get());
    }
    let result = tokio::time::timeout(SHELL_TIMEOUT, cmd.output()).await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_owned())
        }
        Ok(Ok(output)) => {
            let exit = output
                .status
                .code()
                .map_or("signal".to_owned(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(IntegrationError::HookError {
                reason: format!("shell command exited {exit}: {stderr}"),
            })
        }
        Ok(Err(e)) => Err(IntegrationError::HookError {
            reason: format!("failed to spawn shell command: {e}"),
        }),
        Err(_) => Err(IntegrationError::HookError {
            reason: format!("shell command timed out after {}s", SHELL_TIMEOUT.as_secs()),
        }),
    }
}

/// Substitute `{{name}}` placeholders in `template` using values resolved
/// from `store`. Unknown variables propagate as a [`IntegrationError`].
///
/// # Errors
///
/// Returns the first [`IntegrationError`] encountered while resolving any
/// referenced variable.
pub async fn expand(template: &str, store: &VariableStore) -> Result<String, IntegrationError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len()
            && bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(close_rel) = template[i + 2..].find("}}")
        {
            let end = i + 2 + close_rel;
            let name = template[i + 2..end].trim();
            let value = store.resolve(name).await?;
            out.push_str(&value);
            i = end + 2;
            continue;
        }
        out.push(template[i..].chars().next().unwrap_or('?'));
        i += template[i..].chars().next().map_or(1, char::len_utf8);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_variable_resolves_literal() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "greeting".to_owned(),
            source: VariableSource::Static {
                value: "hello".to_owned(),
            },
        });
        assert_eq!(store.resolve("greeting").await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn unknown_variable_errors() {
        let store = VariableStore::new();
        let err = store.resolve("ghost").await.unwrap_err();
        match err {
            IntegrationError::HookError { reason } => assert!(reason.contains("ghost")),
            other => panic!("expected HookError, got {other:?}"),
        }
    }

    // R9 acceptance: shell variable 'pwd' resolves to current_dir
    #[tokio::test]
    async fn shell_variable_pwd_matches_current_dir() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "pwd".to_owned(),
            source: VariableSource::Shell {
                command: "pwd".to_owned(),
                cache_ttl: None,
            },
        });
        let resolved = store.resolve("pwd").await.unwrap();
        let expected = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(resolved, expected);
    }

    // R9 acceptance: a second resolve within TTL returns the same cached value.
    #[tokio::test]
    async fn shell_variable_ttl_caches_within_window() {
        let store = VariableStore::new();
        // date +%N is nanoseconds; subsequent calls would differ without cache.
        store.set(SessionVariable {
            name: "stamp".to_owned(),
            source: VariableSource::Shell {
                command: "date +%N || echo stable".to_owned(),
                cache_ttl: Some(Duration::from_secs(60)),
            },
        });
        let first = store.resolve("stamp").await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second = store.resolve("stamp").await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn shell_variable_without_ttl_does_not_cache() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "uniq".to_owned(),
            source: VariableSource::Shell {
                // Without TTL, each call re-executes; sleep gives the nanosecond
                // counter time to advance.
                command: "date +%N || echo a".to_owned(),
                cache_ttl: None,
            },
        });
        let first = store.resolve("uniq").await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let second = store.resolve("uniq").await.unwrap();
        // On systems where date +%N is unsupported the fallback `echo a` yields
        // the same string; otherwise the values differ.
        if first != "a" {
            assert_ne!(first, second, "no TTL should re-run the command");
        }
    }

    #[tokio::test]
    async fn shell_variable_failure_is_error() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "bad".to_owned(),
            source: VariableSource::Shell {
                command: "exit 7".to_owned(),
                cache_ttl: None,
            },
        });
        let err = store.resolve("bad").await.unwrap_err();
        match err {
            IntegrationError::HookError { reason } => assert!(reason.contains('7')),
            other => panic!("expected HookError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn builtins_register_session_id_working_dir_home_dir() {
        let store = VariableStore::with_builtins();
        let session = store.resolve("session_id").await.unwrap();
        assert!(!session.is_empty());

        let cwd = store.resolve("working_dir").await.unwrap();
        let expected = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(cwd, expected);

        // home_dir resolves to $HOME; just assert it resolves at all (may be
        // empty in some CI environments).
        let _home = store.resolve("home_dir").await.unwrap();
    }

    /// Fix 8 regression: `{{working_dir}}` must resolve from the live
    /// per-agent [`crate::tool::context::SharedWorkingDir`] — not from the
    /// process CWD captured at `with_builtins` time. The handle is installed
    /// *after* `with_builtins`, exactly as `AgentBuilder::build` does, and is
    /// mutated mid-session (a `bash` `cd`); the builtin must track both.
    #[tokio::test]
    async fn builtin_working_dir_tracks_live_shared_working_dir() {
        use crate::tool::context::SharedWorkingDir;

        let first = tempfile::tempdir().expect("tempdir");
        let second = tempfile::tempdir().expect("tempdir");
        let handle = SharedWorkingDir::new(first.path().to_path_buf());

        let store = VariableStore::with_builtins().with_working_dir(handle.clone());
        assert_eq!(
            store.resolve("working_dir").await.unwrap(),
            first.path().to_string_lossy().into_owned(),
            "the builtin must read the installed handle, not the process CWD",
        );

        // Mid-session `cd`: the builtin tracks the live value.
        handle.set(second.path().to_path_buf());
        assert_eq!(
            store.resolve("working_dir").await.unwrap(),
            second.path().to_string_lossy().into_owned(),
            "the builtin must observe working-dir updates live",
        );
    }

    /// Shell-source variables run in the installed working directory.
    #[tokio::test]
    async fn shell_variable_runs_in_installed_working_dir() {
        use crate::tool::context::SharedWorkingDir;

        let dir = tempfile::tempdir().expect("tempdir");
        let handle = SharedWorkingDir::new(dir.path().to_path_buf());
        let store = VariableStore::new().with_working_dir(handle);
        store.set(SessionVariable {
            name: "shell_pwd".to_owned(),
            source: VariableSource::Shell {
                command: "pwd".to_owned(),
                cache_ttl: None,
            },
        });
        let resolved = store.resolve("shell_pwd").await.unwrap();
        let canonical = dir.path().canonicalize().expect("canonicalize");
        assert_eq!(
            std::path::PathBuf::from(resolved)
                .canonicalize()
                .expect("canonicalize resolved"),
            canonical,
        );
    }

    #[tokio::test]
    async fn expand_substitutes_placeholders() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "name".to_owned(),
            source: VariableSource::Static {
                value: "world".to_owned(),
            },
        });
        let out = expand("hello {{name}}", &store).await.unwrap();
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn expand_leaves_text_with_no_placeholders() {
        let store = VariableStore::new();
        let out = expand("nothing to expand", &store).await.unwrap();
        assert_eq!(out, "nothing to expand");
    }

    #[tokio::test]
    async fn resolve_all_returns_every_variable() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "a".to_owned(),
            source: VariableSource::Static {
                value: "1".to_owned(),
            },
        });
        store.set(SessionVariable {
            name: "b".to_owned(),
            source: VariableSource::Static {
                value: "2".to_owned(),
            },
        });
        let map = store.resolve_all().await.unwrap();
        assert_eq!(map.get("a").map(String::as_str), Some("1"));
        assert_eq!(map.get("b").map(String::as_str), Some("2"));
    }

    #[tokio::test]
    async fn set_replaces_previous_and_invalidates_cache() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "v".to_owned(),
            source: VariableSource::Shell {
                command: "echo first".to_owned(),
                cache_ttl: Some(Duration::from_secs(60)),
            },
        });
        let first = store.resolve("v").await.unwrap();
        assert_eq!(first, "first");

        store.set(SessionVariable {
            name: "v".to_owned(),
            source: VariableSource::Static {
                value: "replaced".to_owned(),
            },
        });
        let second = store.resolve("v").await.unwrap();
        assert_eq!(second, "replaced");
    }
}
