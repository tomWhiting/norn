//! Orchestrator context flags and runtime-supplied tool arguments.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::lifecycle::{RuntimeOnSuccessAction, RuntimePostValidateCheck, RuntimePreValidateCheck};
use crate::error::ToolError;

/// Stable identifier for the current agent session, published through the
/// [`ToolContext`] extension map for tools that need per-session storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionId(pub String);

/// Orchestrator context flags.
///
/// A closed enum — adding a new flag requires a code change with review.
/// Flags are set only by orchestrator code (Rhai scripts, workflow definitions),
/// never by the model. They are not part of any tool's input schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolFlag {
    /// Allows overwriting a file without reading it first.
    AllowOverwrite,
    /// Overrides gate post-validate to report semantics.
    AllowBrokenAst,
    /// Overrides report post-validate to gate semantics — AST-specific.
    RejectBrokenAst,
    /// Promotes any tool's post-validate mode from `Report` to `Gate`.
    ///
    /// Generic counterpart to `RejectBrokenAst` — applies whatever the
    /// post-validate check is (AST, length, runtime check, custom).
    ForceGate,
}

/// A flag with its source attribution.
#[derive(Clone, Debug)]
pub struct FlagEntry {
    /// The flag that was set.
    pub flag: ToolFlag,
    /// Which orchestrator code set this flag (e.g. "workflow:generate-templates step 3").
    pub source: String,
}

/// Shared per-agent working directory.
///
/// Wraps a [`PathBuf`] in `Arc<Mutex<…>>` so that [`ToolContext`] and
/// [`crate::agent_loop::loop_context::LoopContext`] can clone the handle and
/// share a single source of truth for the agent's current directory.
/// Bash's `cd` parsing updates this through `set`; subsequent tool calls
/// and loop-level command executions read it through `get`.
///
/// Each fork creates a fresh handle initialised from the parent's current
/// value — child mutations do not propagate back to the parent.
#[derive(Clone, Debug)]
pub struct SharedWorkingDir(Arc<Mutex<PathBuf>>);

impl SharedWorkingDir {
    /// Construct a new handle initialised with `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(dir)))
    }

    /// Returns the current working directory as a clone.
    #[must_use]
    pub fn get(&self) -> PathBuf {
        self.0.lock().clone()
    }

    /// Sets the working directory.
    pub fn set(&self, dir: PathBuf) {
        *self.0.lock() = dir;
    }
}

impl Default for SharedWorkingDir {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

/// Runtime-supplied environment variables for subprocess-spawning tools.
///
/// Embedders publish this through [`ToolContext::insert_extension`]. Tools such
/// as `bash` merge it into child process environments without mutating the
/// process-wide environment.
///
/// Construct with [`ProcessEnv::new`] from any iterator of key/value
/// pairs and compose with [`ProcessEnv::merged`] — embedders never need
/// to hand-assemble the inner map.
#[derive(Clone, Debug, Default)]
pub struct ProcessEnv(pub HashMap<OsString, OsString>);

impl ProcessEnv {
    /// Build a process environment from key/value pairs. Later duplicate
    /// keys overwrite earlier ones, matching map-insert semantics.
    pub fn new<I, K, V>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }

    /// Return a new environment with `entries` merged over this one:
    /// keys in `entries` win conflicts with existing keys.
    #[must_use]
    pub fn merged<I, K, V>(&self, entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let mut env = self.0.clone();
        env.extend(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        Self(env)
    }

    /// Look up a variable by key.
    #[must_use]
    pub fn get(&self, key: impl AsRef<std::ffi::OsStr>) -> Option<&OsString> {
        self.0.get(key.as_ref())
    }

    /// Iterate over all key/value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.0.iter()
    }

    /// Whether the environment carries no variables.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of variables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Context provided by the orchestrator for a tool invocation.
///
/// Carries flags, runtime-supplied arguments, and runtime-configured
/// lifecycle checks. Not part of any tool's input schema.
pub struct ToolContext {
    /// Orchestrator context flags with source attribution.
    pub flags: Vec<FlagEntry>,
    /// Policy-injected arguments not part of the model's tool call.
    pub runtime_args: serde_json::Value,
    /// Runtime pre-validate checks configured by profile/policy.
    pub pre_checks: Vec<Box<dyn RuntimePreValidateCheck>>,
    /// Runtime post-validate checks configured by profile/policy.
    pub post_checks: Vec<Box<dyn RuntimePostValidateCheck>>,
    /// Runtime on-success actions configured by profile/policy.
    pub on_success_actions: Vec<Box<dyn RuntimeOnSuccessAction>>,
    /// Paths the agent has successfully read during this session.
    ///
    /// Wrapped in a `Mutex` for interior mutability so that
    /// `Tool::on_success` (which receives `&ToolContext`) can record reads.
    read_files: Mutex<HashSet<PathBuf>>,
    /// Typed extension map keyed by `TypeId`.
    ///
    /// Orchestrators publish shared infrastructure here (registries, routers,
    /// stores, catalogues, search paths) so tools that need cross-cutting
    /// state can retrieve it without depending on a globally-named field.
    /// Behind a `Mutex` so insertion is possible through `&self`.
    extensions: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    /// Per-agent working directory used to resolve relative paths.
    ///
    /// Shared (cloned `Arc`) with [`crate::agent_loop::loop_context::LoopContext`]
    /// so that bash's `cd` parsing and the loop's prompt commands / hooks /
    /// rules all see the same value.
    working_dir: SharedWorkingDir,
    /// Optional workspace-confinement root for the file tools.
    ///
    /// When set (via [`Self::confine_to_workspace`]) the read/write/edit/
    /// patch tools refuse any resolved path that escapes this directory
    /// after symlink-aware canonicalization, including escapes through
    /// `..` traversal, absolute paths, model-supplied `working_dir`
    /// arguments, and symlinks pointing outside the root. Unset (the
    /// default) preserves unconfined behaviour for embedders that operate
    /// across arbitrary directories.
    workspace_root: Option<PathBuf>,
}

impl ToolContext {
    /// Creates an empty context with no flags, no runtime args, and no checks.
    ///
    /// The working directory defaults to [`std::env::current_dir`] at
    /// construction time, falling back to `.` if the process CWD is
    /// unavailable. Use [`Self::with_working_dir`] to seed from a shared
    /// handle owned by an orchestrator.
    pub fn empty() -> Self {
        Self {
            flags: Vec::new(),
            runtime_args: serde_json::Value::Null,
            pre_checks: Vec::new(),
            post_checks: Vec::new(),
            on_success_actions: Vec::new(),
            read_files: Mutex::new(HashSet::new()),
            extensions: Mutex::new(HashMap::new()),
            working_dir: SharedWorkingDir::default(),
            workspace_root: None,
        }
    }

    /// Construct an empty context that shares the given working-dir handle.
    ///
    /// Updates through this context's `set_working_dir` (or another holder
    /// of the same `SharedWorkingDir` clone) are visible to all sharers.
    #[must_use]
    pub fn with_working_dir(working_dir: SharedWorkingDir) -> Self {
        Self {
            flags: Vec::new(),
            runtime_args: serde_json::Value::Null,
            pre_checks: Vec::new(),
            post_checks: Vec::new(),
            on_success_actions: Vec::new(),
            read_files: Mutex::new(HashSet::new()),
            extensions: Mutex::new(HashMap::new()),
            working_dir,
            workspace_root: None,
        }
    }

    /// Confine the file tools (read/write/edit/patch) to `root`.
    ///
    /// Opt-in: when set, any path that resolves outside `root` after
    /// symlink-aware canonicalization is refused by those tools, and
    /// `apply_patch`'s model-supplied `working_dir` must itself live inside
    /// the root. When never called, path resolution is unconfined (the
    /// historical behaviour for embedders working in arbitrary
    /// directories).
    pub fn confine_to_workspace(&mut self, root: PathBuf) {
        self.workspace_root = Some(root);
    }

    /// Returns the workspace-confinement root, if one was set via
    /// [`Self::confine_to_workspace`].
    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Returns a clone of the shared working-dir handle.
    ///
    /// Use this to share the same underlying value with another component
    /// (e.g. [`crate::agent_loop::loop_context::LoopContext`]) so updates from
    /// one site are visible at the other.
    #[must_use]
    pub fn shared_working_dir(&self) -> SharedWorkingDir {
        self.working_dir.clone()
    }

    /// Returns the current working directory.
    #[must_use]
    pub fn working_dir(&self) -> PathBuf {
        self.working_dir.get()
    }

    /// Updates the working directory.
    ///
    /// Visible immediately to every holder of the shared handle (including
    /// any [`crate::agent_loop::loop_context::LoopContext`] cloned from this
    /// context).
    pub fn set_working_dir(&self, dir: PathBuf) {
        self.working_dir.set(dir);
    }

    /// Resolves a path against the agent's working directory.
    ///
    /// - A `~` or `~/...` prefix expands to [`dirs::home_dir`]. If no home
    ///   directory is detected, the leading `~` is treated as a literal path
    ///   component and the result falls through to the relative branch.
    /// - An absolute path is returned unchanged.
    /// - A relative path is joined onto the current working directory.
    ///
    /// `~user` (a user-specific home prefix) is not expanded — bash-style
    /// user lookup is out of scope. Only `~` and `~/…` are recognised.
    #[must_use]
    pub fn resolve_path(&self, path: impl AsRef<Path>) -> PathBuf {
        let p = path.as_ref();
        let p_str = p.to_string_lossy();
        if let Some(stripped) = p_str.strip_prefix('~')
            && (stripped.is_empty() || stripped.starts_with('/'))
            && let Some(home) = dirs::home_dir()
        {
            let rest = stripped.strip_prefix('/').unwrap_or(stripped);
            return if rest.is_empty() {
                home
            } else {
                home.join(rest)
            };
        }
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.working_dir.get().join(p)
        }
    }

    /// Sets a flag with source attribution.
    pub fn set_flag(&mut self, flag: ToolFlag, source: impl Into<String>) {
        self.flags.push(FlagEntry {
            flag,
            source: source.into(),
        });
    }

    /// Returns true if the given flag is set.
    pub fn has_flag(&self, flag: &ToolFlag) -> bool {
        self.flags.iter().any(|entry| &entry.flag == flag)
    }

    /// Returns the source attribution for a flag, if set.
    pub fn flag_source(&self, flag: &ToolFlag) -> Option<&str> {
        self.flags
            .iter()
            .find(|entry| &entry.flag == flag)
            .map(|entry| entry.source.as_str())
    }

    /// Records that a file has been successfully read.
    ///
    /// Takes `&self` (interior mutability via `Mutex`) so that
    /// `Tool::on_success` — which receives a shared reference to the
    /// context — can register reads after a successful Read.
    ///
    /// Paths are normalized before storage so that equivalent paths with
    /// different string representations (e.g. `./src/../src/main.rs` vs
    /// `/abs/src/main.rs`) resolve to the same key.
    pub fn mark_file_read(&self, path: &Path) {
        self.read_files.lock().insert(self.normalize_path(path));
    }

    /// Returns true if the given path has been read during this session.
    pub fn has_read_file(&self, path: &Path) -> bool {
        self.read_files.lock().contains(&self.normalize_path(path))
    }

    /// Normalize a path for consistent lookup in the read-tracking set.
    ///
    /// Absolutises against [`Self::working_dir`] first, then tries
    /// filesystem canonicalization (resolves symlinks and `..`/`.`
    /// components). Falls back to manual component-cleaning for paths
    /// that don't yet exist on disk. Canonicalising the *absolute* form
    /// avoids the prior bug where a relative `path` resolved against the
    /// process CWD instead of the agent's working directory.
    fn normalize_path(&self, path: &Path) -> PathBuf {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.get().join(path)
        };
        if let Ok(canonical) = abs.canonicalize() {
            return canonical;
        }
        let mut cleaned = PathBuf::new();
        for component in abs.components() {
            match component {
                std::path::Component::ParentDir => {
                    cleaned.pop();
                }
                std::path::Component::CurDir => {}
                c => cleaned.push(c),
            }
        }
        cleaned
    }

    /// Stores a typed extension keyed by its runtime `TypeId`.
    ///
    /// A subsequent call for the same type replaces the previous entry.
    /// Used by orchestrators to publish shared infrastructure (registries,
    /// routers, stores) that individual tools then read via
    /// [`Self::get_extension`].
    pub fn insert_extension<T>(&self, value: Arc<T>)
    where
        T: Any + Send + Sync,
    {
        let erased: Arc<dyn Any + Send + Sync> = value;
        self.extensions.lock().insert(TypeId::of::<T>(), erased);
    }

    /// Retrieves a previously-inserted extension by type, cloning the
    /// shared `Arc`. Returns `None` if no extension of `T` is present.
    #[must_use]
    pub fn get_extension<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        let guard = self.extensions.lock();
        let any = guard.get(&TypeId::of::<T>())?;
        Arc::clone(any).downcast::<T>().ok()
    }

    /// Retrieves a required extension, failing with a typed
    /// [`ToolError::MissingExtension`] that names the absent type.
    ///
    /// This is the standard accessor for tools whose execution depends on
    /// embedder-published infrastructure (stores, catalogs, providers):
    /// use it instead of hand-rolling `get_extension(..).ok_or_else(..)`
    /// so every missing-extension failure carries the same typed error
    /// and the same model-facing message. Use [`Self::get_extension`]
    /// only when absence is a legitimate state.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::MissingExtension`] carrying
    /// [`std::any::type_name`] of `T` when no extension of type `T` has
    /// been inserted.
    pub fn require_extension<T>(&self) -> Result<Arc<T>, ToolError>
    where
        T: Any + Send + Sync,
    {
        self.get_extension::<T>()
            .ok_or_else(|| ToolError::MissingExtension {
                extension: std::any::type_name::<T>().to_string(),
            })
    }
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

    #[test]
    fn empty_context_has_no_flags() {
        let ctx = ToolContext::empty();
        assert!(!ctx.has_flag(&ToolFlag::AllowOverwrite));
        assert!(!ctx.has_flag(&ToolFlag::AllowBrokenAst));
        assert!(!ctx.has_flag(&ToolFlag::RejectBrokenAst));
        assert!(!ctx.has_flag(&ToolFlag::ForceGate));
        assert!(ctx.flag_source(&ToolFlag::AllowOverwrite).is_none());
    }

    #[test]
    fn set_flag_and_query() {
        let mut ctx = ToolContext::empty();
        ctx.set_flag(
            ToolFlag::AllowOverwrite,
            "workflow:generate-templates step 3",
        );
        assert!(ctx.has_flag(&ToolFlag::AllowOverwrite));
        assert!(!ctx.has_flag(&ToolFlag::AllowBrokenAst));
        assert_eq!(
            ctx.flag_source(&ToolFlag::AllowOverwrite),
            Some("workflow:generate-templates step 3")
        );
    }

    #[test]
    fn mark_file_read_via_shared_ref_records_path() {
        let ctx = ToolContext::empty();
        let path_a = Path::new("/tmp/a.txt");
        let path_b = Path::new("/tmp/b.txt");

        assert!(!ctx.has_read_file(path_a));
        assert!(!ctx.has_read_file(path_b));

        ctx.mark_file_read(path_a);
        assert!(ctx.has_read_file(path_a));
        assert!(!ctx.has_read_file(path_b));

        ctx.mark_file_read(path_b);
        ctx.mark_file_read(path_a);
        assert!(ctx.has_read_file(path_a));
        assert!(ctx.has_read_file(path_b));
    }

    #[test]
    fn has_read_file_matches_equivalent_paths_with_dot_components() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let file = sub.join("file.txt");
        std::fs::write(&file, "content").unwrap();

        let ctx = ToolContext::empty();
        ctx.mark_file_read(&file);

        let dotted = sub.join(".").join("file.txt");
        assert!(ctx.has_read_file(&dotted));

        let parent = sibling.join("..").join("sub").join("file.txt");
        assert!(ctx.has_read_file(&parent));
    }

    #[derive(Debug)]
    struct ExtA(u32);
    struct ExtB(&'static str);

    #[test]
    fn insert_and_get_extension_roundtrip() {
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(ExtA(42)));
        ctx.insert_extension(Arc::new(ExtB("hello")));

        let a = ctx.get_extension::<ExtA>().expect("ExtA present");
        assert_eq!(a.0, 42);
        let b = ctx.get_extension::<ExtB>().expect("ExtB present");
        assert_eq!(b.0, "hello");

        struct Missing;
        assert!(ctx.get_extension::<Missing>().is_none());
    }

    #[test]
    fn require_extension_returns_present_extension() {
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(ExtA(7)));
        let a = ctx.require_extension::<ExtA>().expect("ExtA present");
        assert_eq!(a.0, 7);
    }

    #[test]
    fn require_extension_missing_yields_typed_error_naming_type() {
        let ctx = ToolContext::empty();
        let err = ctx
            .require_extension::<ExtA>()
            .expect_err("ExtA not inserted");
        match err {
            ToolError::MissingExtension { extension } => {
                assert!(
                    extension.contains("ExtA"),
                    "error must name the missing type: {extension}",
                );
            }
            other => panic!("expected MissingExtension, got {other:?}"),
        }
    }

    #[test]
    fn insert_replaces_previous_entry_for_same_type() {
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(ExtA(1)));
        ctx.insert_extension(Arc::new(ExtA(2)));
        let a = ctx.get_extension::<ExtA>().expect("ExtA present");
        assert_eq!(a.0, 2);
    }

    #[test]
    fn multiple_flags_with_sources() {
        let mut ctx = ToolContext::empty();
        ctx.set_flag(ToolFlag::AllowBrokenAst, "workflow:draft step 1");
        ctx.set_flag(ToolFlag::AllowOverwrite, "profile:template-generator");

        assert!(ctx.has_flag(&ToolFlag::AllowBrokenAst));
        assert!(ctx.has_flag(&ToolFlag::AllowOverwrite));
        assert!(!ctx.has_flag(&ToolFlag::RejectBrokenAst));

        assert_eq!(
            ctx.flag_source(&ToolFlag::AllowBrokenAst),
            Some("workflow:draft step 1")
        );
        assert_eq!(
            ctx.flag_source(&ToolFlag::AllowOverwrite),
            Some("profile:template-generator")
        );
    }

    #[test]
    fn resolve_path_passes_absolute_through_unchanged() {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/wd")));
        let resolved = ctx.resolve_path("/absolute/path/file.rs");
        assert_eq!(resolved, PathBuf::from("/absolute/path/file.rs"));
    }

    #[test]
    fn resolve_path_joins_relative_against_working_dir() {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/wd")));
        let resolved = ctx.resolve_path("src/main.rs");
        assert_eq!(resolved, PathBuf::from("/tmp/wd/src/main.rs"));
    }

    #[test]
    fn resolve_path_expands_tilde_prefix() {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/wd")));
        let resolved = ctx.resolve_path("~/src/main.rs");
        let home = dirs::home_dir().expect("home dir available in test env");
        assert_eq!(resolved, home.join("src/main.rs"));
    }

    #[test]
    fn resolve_path_bare_tilde_returns_home_dir() {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/wd")));
        let resolved = ctx.resolve_path("~");
        let home = dirs::home_dir().expect("home dir available in test env");
        assert_eq!(resolved, home);
    }

    #[test]
    fn resolve_path_does_not_expand_tilde_user_prefix() {
        // ~user is a bash convention for user-specific homes; out of scope.
        // The leading `~user` falls through and is joined as a relative path.
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/wd")));
        let resolved = ctx.resolve_path("~root/file.txt");
        assert_eq!(resolved, PathBuf::from("/tmp/wd/~root/file.txt"));
    }

    #[test]
    fn set_working_dir_is_visible_to_subsequent_resolve_calls() {
        let ctx = ToolContext::with_working_dir(SharedWorkingDir::new(PathBuf::from("/tmp/a")));
        assert_eq!(ctx.resolve_path("foo"), PathBuf::from("/tmp/a/foo"));

        ctx.set_working_dir(PathBuf::from("/tmp/b"));
        assert_eq!(ctx.resolve_path("foo"), PathBuf::from("/tmp/b/foo"));
        assert_eq!(ctx.working_dir(), PathBuf::from("/tmp/b"));
    }

    #[test]
    fn shared_working_dir_propagates_updates_to_clones() {
        let shared = SharedWorkingDir::new(PathBuf::from("/tmp/seed"));
        let ctx_a = ToolContext::with_working_dir(shared.clone());
        let ctx_b = ToolContext::with_working_dir(shared.clone());

        // Update via ctx_a → ctx_b observes it.
        ctx_a.set_working_dir(PathBuf::from("/tmp/updated"));
        assert_eq!(ctx_b.working_dir(), PathBuf::from("/tmp/updated"));
        assert_eq!(shared.get(), PathBuf::from("/tmp/updated"));
    }

    #[test]
    fn empty_context_default_working_dir_is_process_cwd_or_dot() {
        let ctx = ToolContext::empty();
        let wd = ctx.working_dir();
        // Either matches process CWD (normal case) or is "." (when CWD
        // unavailable). Both are acceptable per the documented default.
        let expected = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        assert_eq!(wd, expected);
    }

    #[test]
    fn process_env_new_collects_pairs_with_last_key_winning() {
        let env = ProcessEnv::new([
            ("PATH", "/usr/bin"),
            ("HOME", "/home/a"),
            ("HOME", "/home/b"),
        ]);
        assert_eq!(env.len(), 2);
        assert_eq!(env.get("PATH"), Some(&OsString::from("/usr/bin")));
        assert_eq!(env.get("HOME"), Some(&OsString::from("/home/b")));
        assert!(!env.is_empty());
    }

    #[test]
    fn process_env_merged_overlays_and_preserves_original() {
        let base = ProcessEnv::new([("A", "1"), ("B", "2")]);
        let merged = base.merged([("B", "overridden"), ("C", "3")]);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.get("A"), Some(&OsString::from("1")));
        assert_eq!(merged.get("B"), Some(&OsString::from("overridden")));
        assert_eq!(merged.get("C"), Some(&OsString::from("3")));
        // The original is untouched — merged is a pure overlay.
        assert_eq!(base.get("B"), Some(&OsString::from("2")));
        assert!(base.get("C").is_none());
    }

    #[test]
    fn process_env_default_is_empty_and_iterable() {
        let env = ProcessEnv::default();
        assert!(env.is_empty());
        assert_eq!(env.len(), 0);
        assert_eq!(env.iter().count(), 0);
    }
}
