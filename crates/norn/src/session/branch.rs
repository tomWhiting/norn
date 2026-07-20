//! Child-session branching: the single allocation authority that mints
//! persistent (or honestly-ephemeral) child timelines under a root
//! session.
//!
//! # Layout
//!
//! A persisted root lives at `{data_dir}/{root-id}.jsonl`. Its children —
//! recursively, grandchildren too — live in a sibling directory keyed by
//! the root id:
//!
//! ```text
//! {data_dir}/
//! +-- {root-id}.jsonl
//! +-- {root-id}/
//!     +-- children/
//!         +-- fork-1a2b3c4d.jsonl
//!         +-- fork-1a2b3c4d--spawn-9e8f7a6b.jsonl   (grandchild)
//! ```
//!
//! The file name is the **full path slug**: segments after `root` are joined
//! with `--` (sibling names alone would collide across parents). Discovery stays
//! manifest-driven: each child gets an index row carrying its
//! [`rel_path`](crate::session::persistence::SessionIndexEntry::rel_path)
//! — nothing ever crawls the directory.
//!
//! # Write ordering (PARENT-FIRST — review §7 ruling)
//!
//! [`SessionBinding::branch_child`] performs, under ONE per-parent lock:
//!
//! 1. mint a fresh name against the parent's ever-used set,
//! 2. **append the [`SessionEvent::ChildBranch`] reservation to the
//!    parent's store** (durable before anything keyed by the name
//!    exists),
//! 3. stage the strict child timeline and durably journal its exact digest
//!    plus index row,
//! 4. publish the child file without replacement, then atomically add the
//!    index row and clear the journal.
//!
//! A crash between 2 and 3 leaves a burned name plus a dangling child
//! reference — exactly the state resume paths already tolerate (the
//! `ForkComplete`-`Option` honesty machinery). Once the journal is durable,
//! every index read or mutation converges the no-clobber timeline-first
//! publication before exposing the row. An unjournaled staged file is inert;
//! a foreign file at the final path is never adopted or replaced. If one is
//! found on disk (external tampering or old residue), the mint refuses with the typed
//! [`SessionPersistError::ChildPathOccupied`] rather than truncating or
//! appending to a foreign history.
//!
//! # For-all-time name uniqueness (ruling Q2)
//!
//! The parent's timeline IS the name registry: every mint appends a
//! `ChildBranch` event carrying the allocated name, and the ever-used
//! set is replayed from those events on resume. A terminated child's
//! name stays reserved forever within its parent. Ephemeral children
//! reserve their name through the same parent-store append — **that
//! append is the only durable trace an ephemeral child leaves, and it is
//! an INVARIANT, not an optimization target** (review §6).

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::session::events::EventBase;
use crate::session::events::{ChildBranchKind, SessionEvent};
use crate::session::manager::SessionManager;
#[cfg(test)]
use crate::session::persistence::index::insert_child_index_entry;
use crate::session::persistence::types::SessionIndexEntry;
#[cfg(test)]
use crate::session::persistence::types::SessionPersistError;
#[cfg(test)]
use crate::session::persistence::types::{SESSION_FORMAT_VERSION, SessionStatus};
use crate::session::store::{DurabilityPolicy, EventStore};
#[path = "branch_child.rs"]
mod child;
#[path = "branch_materialize.rs"]
mod materialize;
#[cfg(test)]
use child::mint_child_name;
#[cfg(test)]
use materialize::materialize_child;

/// The canonical path address of a primary line (a root session). Child
/// addresses nest under it: `root/fork-1a2b3c4d/spawn-9e8f7a6b`.
pub const ROOT_PATH_ADDRESS: &str = "root";

/// Persist-vs-ephemeral axis for child timelines.
///
/// Deliberately distinct from [`DurabilityPolicy`], which is fsync
/// *cadence* and always assumes a sink exists — it can never express
/// "no sink". `Ephemeral` is the explicit `--no-session` opt-out and
/// propagates down the subtree: an ephemeral agent's children are
/// ephemeral too.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildDurability {
    /// The child gets a sink-equipped store: a real on-disk timeline
    /// under the root's `children/` directory plus an index row.
    Persist,
    /// The child runs memory-only. Its name is still durably reserved on
    /// the parent's timeline (when the parent persists), with the honest
    /// `child_session_id: None` on the branch event.
    Ephemeral,
}

/// The shared branching authority for one persistent root: the
/// [`SessionManager`] (data dir + lock deadline), the root's session id
/// (which keys the `children/` directory), and the fsync cadence child
/// sinks inherit.
#[derive(Debug)]
pub struct SessionBrancher {
    manager: SessionManager,
    root_session_id: String,
    durability: DurabilityPolicy,
}

impl SessionBrancher {
    /// Build the authority for the root session `root_session_id`.
    /// `durability` is the fsync cadence every child sink runs with —
    /// inherited from how the root itself was opened, never invented
    /// here.
    #[must_use]
    pub fn new(
        manager: SessionManager,
        root_session_id: String,
        durability: DurabilityPolicy,
    ) -> Self {
        Self {
            manager,
            root_session_id,
            durability,
        }
    }

    /// The index-relative path a child at `path_address` persists to.
    fn child_rel_path(&self, path_address: &str) -> String {
        format!(
            "{}/children/{}.jsonl",
            self.root_session_id,
            child_path_slug(path_address),
        )
    }
}

/// The full-path slug for a child's file name: every segment after the
/// leading `root` joined with `--`. Injective because minted name
/// segments never contain `--` (the stem slug collapses repeated `-`
/// and the suffix is hex).
#[must_use]
pub fn child_path_slug(path_address: &str) -> String {
    path_address
        .split('/')
        .skip(1)
        .collect::<Vec<_>>()
        .join("--")
}

/// Slugify a caller-supplied name stem (a role or variant label) into
/// the `[a-z0-9-]` alphabet used by path addresses: lowercased,
/// non-alphanumeric runs collapsed to single `-`, trimmed. Returns
/// `fallback` (the mint kind's own label — grounded, not invented) when
/// nothing survives.
#[must_use]
pub fn slugify_name_stem(raw: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true; // suppress leading dash
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out
    }
}

/// What a launch site asks [`SessionBinding::branch_child`] for.
#[derive(Clone, Debug)]
pub struct ChildBranchRequest {
    /// The child's session id — the SAME id as its agent-registry entry
    /// (one identity across registry, index row, file, and branch
    /// events; ruling D-idspace, `UUIDv4` per R8).
    pub child_session_id: String,
    /// Slugified name stem (`fork`, `spawn`, a role/variant label). The
    /// minted per-parent name is `{stem}-{8-hex}` — the 8-character
    /// suffix follows R8's short-id display convention.
    pub name_stem: String,
    /// Fork (history-seeded) or spawn (fresh).
    pub kind: ChildBranchKind,
    /// Requested persistence. [`ChildDurability::Persist`] under an
    /// ephemeral parent is the typed
    /// [`SessionPersistError::EphemeralParent`] refusal.
    pub durability: ChildDurability,
    /// Model recorded on the child's index row.
    pub model: String,
    /// Working directory recorded on the child's index row.
    pub working_dir: String,
}

/// What [`SessionBinding::branch_child`] hands back.
pub struct BranchedChild {
    // NOTE: `Debug` is implemented manually below (`EventStore` is not
    // `Debug`); keep the fields and the impl in step.
    /// The child's event store — sink-equipped (write-through to its
    /// nested file) for persistent children, sink-less for ephemeral
    /// ones.
    pub store: Arc<EventStore>,
    /// The child's own binding, to be carried on the child's infra so
    /// grandchild mints route through the same machinery (depth
    /// recursion is structural, not per-call).
    pub binding: Arc<SessionBinding>,
    /// The child's full coordination path address.
    pub path_address: String,
    /// The child's session id — `None` for ephemeral children (honest
    /// absence, never a stand-in id).
    pub session_id: Option<String>,
    /// The parent's last event id at branch time, exactly as recorded on
    /// the reservation's `parent_event_anchor` — captured INSIDE the
    /// allocation lock. Fork seeding truncates its parent-history copy
    /// at this anchor so the seed matches the recorded branch point even
    /// when concurrent tasks append to the parent store between the mint
    /// and the snapshot. `None` = the parent log was empty at branch.
    pub parent_event_anchor: Option<crate::session::events::EventId>,
}

impl std::fmt::Debug for BranchedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchedChild")
            .field("path_address", &self.path_address)
            .field("session_id", &self.session_id)
            .field("parent_event_anchor", &self.parent_event_anchor)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

/// How a binding persists — constructed only through
/// [`SessionBinding::persistent_root`] / [`SessionBinding::ephemeral_root`]
/// / [`SessionBinding::branch_child`], so "persistent without a brancher"
/// is unrepresentable.
enum Persistence {
    Persistent {
        brancher: Arc<SessionBrancher>,
        registered: Box<SessionIndexEntry>,
    },
    Ephemeral,
}

/// One agent's session-branching identity: its path address, its own
/// session id (when persisted), and the per-parent ever-used child-name
/// set whose mutex is the SINGLE allocation lock held across name-check,
/// parent-log append, index insert, and child-file creation (review §8).
pub struct SessionBinding {
    path_address: String,
    persistence: Persistence,
    /// Ever-used child names (last path segments) minted by THIS agent —
    /// seeded from replayed `ChildBranch` events whose
    /// `parent_session_id` matches this agent's session id, so a
    /// fork-seeded child never counts reservations it merely inherited.
    used_names: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for SessionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionBinding")
            .field("path_address", &self.path_address)
            .field("session_id", &self.session_id())
            .field("persistent", &self.is_persistent())
            .finish_non_exhaustive()
    }
}

impl SessionBinding {
    /// Binding for a root with **no** persisted session (`--no-session`,
    /// embedder-supplied stores, tests): every child it mints is
    /// ephemeral, and name reservations live in memory only — correct,
    /// because an ephemeral subtree has no cross-restart identity to
    /// protect.
    #[must_use]
    pub fn ephemeral_root() -> Self {
        Self {
            path_address: ROOT_PATH_ADDRESS.to_owned(),
            persistence: Persistence::Ephemeral,
            used_names: Mutex::new(HashSet::new()),
        }
    }

    /// Binding for a persisted root (or a resumed persisted session).
    ///
    /// `registered` pins the exact immutable index generation; a binding
    /// retained across delete-and-recreate refuses every later mint.
    /// `events` is the session's replayed history (empty for a fresh
    /// create): the ever-used name set is re-derived from `ChildBranch`
    /// events this session appended as a parent, and the session's own
    /// path address is recovered from a `ChildBranch` naming it as the
    /// child (a resumed child session keeps its address; a root stays
    /// `root`).
    #[must_use]
    pub fn persistent_root(
        brancher: Arc<SessionBrancher>,
        registered: &SessionIndexEntry,
        events: &[SessionEvent],
    ) -> Self {
        let session_id = &registered.id;
        let mut used = HashSet::new();
        let mut path_address = ROOT_PATH_ADDRESS.to_owned();
        for event in events {
            if let SessionEvent::ChildBranch {
                parent_session_id,
                child_session_id,
                path_address: event_path,
                ..
            } = event
            {
                if parent_session_id.as_deref() == Some(session_id.as_str())
                    && let Some(name) = event_path.rsplit('/').next()
                {
                    used.insert(name.to_owned());
                }
                if child_session_id.as_deref() == Some(session_id.as_str()) {
                    path_address.clone_from(event_path);
                }
            }
        }
        Self {
            path_address,
            persistence: Persistence::Persistent {
                brancher,
                registered: Box::new(registered.clone()),
            },
            used_names: Mutex::new(used),
        }
    }

    /// This agent's full coordination path address.
    #[must_use]
    pub fn path_address(&self) -> &str {
        &self.path_address
    }

    /// This agent's own session id (`None` = ephemeral).
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match &self.persistence {
            Persistence::Persistent { registered, .. } => Some(&registered.id),
            Persistence::Ephemeral => None,
        }
    }

    /// Whether this agent's own timeline persists.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        matches!(self.persistence, Persistence::Persistent { .. })
    }

    /// The durability children of this agent inherit by default:
    /// `Persist` under a persisted timeline, `Ephemeral` otherwise (the
    /// explicit-choice-propagates-down rule of R2).
    #[must_use]
    pub fn child_durability(&self) -> ChildDurability {
        if self.is_persistent() {
            ChildDurability::Persist
        } else {
            ChildDurability::Ephemeral
        }
    }

    /// Snapshot of the ever-used child-name set (observability + tests).
    #[must_use]
    pub fn ever_used_names(&self) -> HashSet<String> {
        self.used_names.lock().clone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::session::manager::CreateSessionOptions;
    use crate::session::persistence::index::read_index;
    use crate::session::persistence::io::read_session_events_for_entry;

    fn options() -> CreateSessionOptions {
        CreateSessionOptions {
            model: "test-model".to_owned(),
            working_dir: "/work".to_owned(),
            name: None,
        }
    }

    fn request(stem: &str, durability: ChildDurability) -> ChildBranchRequest {
        ChildBranchRequest {
            child_session_id: Uuid::new_v4().to_string(),
            name_stem: stem.to_owned(),
            kind: ChildBranchKind::Spawn,
            durability,
            model: "test-model".to_owned(),
            working_dir: "/work".to_owned(),
        }
    }

    struct Root {
        _tmp: tempfile::TempDir,
        manager: SessionManager,
        id: String,
        entry: SessionIndexEntry,
        store: Arc<EventStore>,
        binding: Arc<SessionBinding>,
    }

    fn persistent_root() -> Root {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path());
        let opened = manager.create(options(), DurabilityPolicy::Flush).unwrap();
        let root_id = opened.entry.id.clone();
        let brancher = Arc::new(SessionBrancher::new(
            manager.clone(),
            root_id.clone(),
            DurabilityPolicy::Flush,
        ));
        let binding = Arc::new(SessionBinding::persistent_root(
            brancher,
            &opened.entry,
            &[],
        ));
        Root {
            _tmp: tmp,
            manager,
            id: root_id,
            entry: opened.entry,
            store: Arc::new(opened.store),
            binding,
        }
    }

    #[test]
    fn slugify_stem_normalizes_and_falls_back() {
        assert_eq!(
            slugify_name_stem("Code Reviewer!", "spawn"),
            "code-reviewer"
        );
        assert_eq!(slugify_name_stem("fork/gpt-5.5", "fork"), "fork-gpt-5-5");
        assert_eq!(slugify_name_stem("!!!", "spawn"), "spawn");
        assert_eq!(slugify_name_stem("", "fork"), "fork");
    }

    #[test]
    fn child_path_slug_joins_segments_after_root() {
        assert_eq!(child_path_slug("root/fork-1a2b3c4d"), "fork-1a2b3c4d");
        assert_eq!(
            child_path_slug("root/fork-1a2b3c4d/spawn-9e8f7a6b"),
            "fork-1a2b3c4d--spawn-9e8f7a6b",
        );
    }

    #[test]
    fn mint_refuses_used_names() {
        let mut used = HashSet::new();
        let first = mint_child_name("fork", &used);
        assert!(first.starts_with("fork-") && first.len() == "fork-".len() + 8);
        used.insert(first.clone());
        let second = mint_child_name("fork", &used);
        assert_ne!(first, second, "a used name must never be re-minted");
    }

    /// The primitive end to end: persistent child gets a real on-disk
    /// timeline at the slugged path, an index row with `rel_path` +
    /// `parent_id`, and the parent's reservation event — with the
    /// PARENT-FIRST ordering observable in the produced artifacts.
    #[test]
    fn branch_child_persists_child_with_linkage() {
        let root = persistent_root();
        let req = request("reviewer", ChildDurability::Persist);
        let child = root.binding.branch_child(&root.store, &req).unwrap();

        assert_eq!(
            child.session_id.as_deref(),
            Some(req.child_session_id.as_str())
        );
        let name = child.path_address.rsplit('/').next().unwrap();
        assert!(name.starts_with("reviewer-"));

        // Parent reservation is durable in the parent's file.
        let parent_entry = root.manager.resolve(&root.id).unwrap();
        let parent_read =
            read_session_events_for_entry(root.manager.data_dir(), &parent_entry).unwrap();
        let reservation = parent_read
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ChildBranch {
                    parent_session_id,
                    child_session_id,
                    path_address,
                    kind,
                    ..
                } => Some((
                    parent_session_id.clone(),
                    child_session_id.clone(),
                    path_address.clone(),
                    *kind,
                )),
                _ => None,
            })
            .expect("the parent's file must carry the ChildBranch reservation");
        assert_eq!(reservation.0.as_deref(), Some(root.id.as_str()));
        assert_eq!(
            reservation.1.as_deref(),
            Some(req.child_session_id.as_str())
        );
        assert_eq!(reservation.2, child.path_address);
        assert_eq!(reservation.3, ChildBranchKind::Spawn);

        // Index row: rel_path + parent linkage.
        let rows = read_index(root.manager.data_dir()).unwrap();
        let row = rows.iter().find(|e| e.id == req.child_session_id).unwrap();
        let expected_rel = format!("{}/children/{name}.jsonl", root.id);
        assert_eq!(row.rel_path.as_deref(), Some(expected_rel.as_str()));
        assert_eq!(row.parent_id.as_deref(), Some(root.id.as_str()));

        // The child file exists at the nested path and write-through
        // persistence is live: its header event is already on disk, and
        // a fresh append lands too.
        let child_path = root.manager.data_dir().join(&expected_rel);
        assert!(child_path.exists(), "child timeline file must exist");
        child
            .store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "child work".to_owned(),
            })
            .unwrap();
        let child_read = read_session_events_for_entry(root.manager.data_dir(), row).unwrap();
        assert_eq!(child_read.events.len(), 2, "provenance header + append");
        assert!(matches!(
            &child_read.events[0],
            SessionEvent::ChildBranch { child_session_id, .. }
                if child_session_id.as_deref() == Some(req.child_session_id.as_str())
        ));

        // And the child resumes through the manager like any session.
        let resumed = root
            .manager
            .resume(&req.child_session_id, DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(resumed.replay.replayed_events, 2);
    }

    /// PARENT-FIRST ordering, observed BETWEEN the steps: after the
    /// reservation append the parent's file carries the `ChildBranch` on
    /// disk while no child file exists yet; materialization then creates
    /// it. This drives the split halves of `branch_child` directly.
    #[test]
    fn reservation_is_on_disk_before_child_file_exists() -> Result<(), Box<dyn std::error::Error>> {
        let root = persistent_root();
        let req = request("worker", ChildDurability::Persist);

        // Step 2 in isolation: the reservation append.
        let anchor = root.store.last_event_id();
        let path_address = format!("root/worker-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let reservation = SessionEvent::ChildBranch {
            base: EventBase::new(anchor.clone()),
            parent_session_id: Some(root.id.clone()),
            child_session_id: Some(req.child_session_id.clone()),
            path_address: path_address.clone(),
            parent_event_anchor: anchor,
            kind: ChildBranchKind::Spawn,
        };
        root.store.append(reservation.clone()).unwrap();

        // OBSERVE: reservation durable, child absent — the exact crash
        // residue parent-first ordering promises.
        let parent_entry = root.manager.resolve(&root.id).unwrap();
        let on_disk =
            read_session_events_for_entry(root.manager.data_dir(), &parent_entry).unwrap();
        assert!(
            on_disk
                .events
                .iter()
                .any(|e| matches!(e, SessionEvent::ChildBranch { .. })),
            "the reservation must be durable before any child artifact",
        );
        let rel = format!(
            "{}/children/{}.jsonl",
            root.id,
            child_path_slug(&path_address)
        );
        assert!(
            !root.manager.data_dir().join(&rel).exists(),
            "no child file may exist before materialization",
        );
        assert!(
            !read_index(root.manager.data_dir())
                .unwrap()
                .iter()
                .any(|e| e.id == req.child_session_id),
            "no index row may exist before materialization",
        );

        // Steps 3–4: materialize, then everything exists.
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id,
            DurabilityPolicy::Flush,
        ));
        let (child_store, _binding) =
            materialize_child(&brancher, &root.entry, &path_address, &req, &reservation)?;
        assert!(root.manager.data_dir().join(&rel).exists());
        assert_eq!(child_store.len(), 1, "provenance header appended");
        Ok(())
    }

    /// Mint-collision hard error: an orphan file already sitting at the
    /// minted slug path is refused typed — never truncated, never
    /// appended to — and the orphan's bytes are untouched.
    #[test]
    fn mint_collision_with_orphan_file_is_hard_typed_error() {
        let root = persistent_root();
        let req = request("worker", ChildDurability::Persist);
        let path_address = "root/worker-feedbeef".to_owned();
        let rel_path = format!(
            "{}/children/{}.jsonl",
            root.id,
            child_path_slug(&path_address)
        );
        let orphan_abs = root.manager.data_dir().join(&rel_path);
        std::fs::create_dir_all(orphan_abs.parent().unwrap()).unwrap();
        std::fs::write(&orphan_abs, b"{\"foreign\":true}\n").unwrap();

        let reservation = SessionEvent::ChildBranch {
            base: EventBase::new(None),
            parent_session_id: Some(root.id.clone()),
            child_session_id: Some(req.child_session_id.clone()),
            path_address: path_address.clone(),
            parent_event_anchor: None,
            kind: ChildBranchKind::Spawn,
        };
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id,
            DurabilityPolicy::Flush,
        ));
        let err = materialize_child(&brancher, &root.entry, &path_address, &req, &reservation)
            .expect_err("an occupied slug path must refuse hard");
        assert!(
            matches!(&err, SessionPersistError::ChildPathOccupied { rel_path: r } if *r == rel_path),
            "expected ChildPathOccupied, got {err:?}",
        );
        assert_eq!(
            std::fs::read(&orphan_abs).unwrap(),
            b"{\"foreign\":true}\n",
            "the orphan must be byte-identical — never truncated or appended to",
        );
        // F2: the refusal happens BEFORE the index insert, so no row can
        // be left pointing at the foreign file (a later resume of such a
        // row would replay another agent's history).
        let rows = read_index(root.manager.data_dir()).unwrap();
        assert!(
            !rows
                .iter()
                .any(|e| e.id == req.child_session_id || e.rel_path.as_deref() == Some(&*rel_path)),
            "a refused mint must leave no index row: {rows:?}",
        );
    }

    /// Mint-collision on the index row: a foreign row claiming the same
    /// `rel_path` refuses the insert typed.
    #[test]
    fn mint_collision_with_orphan_index_row_is_hard_typed_error() {
        let root = persistent_root();
        let req = request("worker", ChildDurability::Persist);
        let path_address = "root/worker-0badcafe".to_owned();
        let rel_path = format!(
            "{}/children/{}.jsonl",
            root.id,
            child_path_slug(&path_address)
        );
        // Foreign row claiming the same rel_path under a different id.
        let mut foreign = SessionIndexEntry {
            id: Uuid::new_v4().to_string(),
            generation: Uuid::new_v4(),
            name: None,
            model: "m".to_owned(),
            working_dir: "/w".to_owned(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            event_count: 0,
            status: SessionStatus::Active,
            format_version: SESSION_FORMAT_VERSION,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            rel_path: Some(rel_path),
            parent_id: Some(root.id.clone()),
            fidelity: crate::session::persistence::ResumeFidelity::Canonical,
            origin: crate::session::persistence::SessionRecordOrigin::Native,
            provider_state_identity: None,
        };
        insert_child_index_entry(root.manager.data_dir(), &foreign, None).unwrap();

        let reservation = SessionEvent::ChildBranch {
            base: EventBase::new(None),
            parent_session_id: Some(root.id.clone()),
            child_session_id: Some(req.child_session_id.clone()),
            path_address: path_address.clone(),
            parent_event_anchor: None,
            kind: ChildBranchKind::Spawn,
        };
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id.clone(),
            DurabilityPolicy::Flush,
        ));
        let err = materialize_child(&brancher, &root.entry, &path_address, &req, &reservation)
            .expect_err("a claimed rel_path must refuse hard");
        assert!(
            matches!(err, SessionPersistError::ChildPathOccupied { .. }),
            "expected ChildPathOccupied",
        );
        // And a same-ID duplicate refuses as IdExists.
        foreign.rel_path = Some(format!("{}/children/other.jsonl", root.id));
        let dup = insert_child_index_entry(root.manager.data_dir(), &foreign, None).unwrap_err();
        assert!(matches!(dup, SessionPersistError::IdExists { .. }));
    }

    /// Ephemeral child under a persistent parent: no file, no index row,
    /// but the name reservation IS durably on the parent's timeline with
    /// the honest `child_session_id: None` (the INVARIANT of review §6).
    #[test]
    fn ephemeral_child_reserves_name_durably_with_honest_none() {
        let root = persistent_root();
        let req = request("scout", ChildDurability::Ephemeral);
        let child = root.binding.branch_child(&root.store, &req).unwrap();
        assert!(child.session_id.is_none());
        assert!(!child.binding.is_persistent());
        assert_eq!(
            child.binding.child_durability(),
            ChildDurability::Ephemeral,
            "ephemeral propagates down",
        );

        let parent_entry = root.manager.resolve(&root.id).unwrap();
        let on_disk =
            read_session_events_for_entry(root.manager.data_dir(), &parent_entry).unwrap();
        let reservation = on_disk
            .events
            .iter()
            .find_map(|e| match e {
                SessionEvent::ChildBranch {
                    child_session_id,
                    path_address,
                    ..
                } => Some((child_session_id.clone(), path_address.clone())),
                _ => None,
            })
            .expect("ephemeral children still reserve durably");
        assert_eq!(
            reservation.0, None,
            "session: None is the honest ephemeral record",
        );
        assert_eq!(reservation.1, child.path_address);

        // No index row, no file.
        assert_eq!(read_index(root.manager.data_dir()).unwrap().len(), 1);
        assert!(
            !root
                .manager
                .data_dir()
                .join(&root.id)
                .join("children")
                .exists(),
        );
    }

    /// Persist requested under an ephemeral parent is the TYPED
    /// refusal — never a missing-directory I/O failure.
    #[test]
    fn persist_under_ephemeral_parent_is_typed_error() {
        let binding = SessionBinding::ephemeral_root();
        let store = EventStore::new();
        let err = binding
            .branch_child(&store, &request("worker", ChildDurability::Persist))
            .expect_err("persist under an ephemeral parent must refuse typed");
        assert!(
            matches!(&err, SessionPersistError::EphemeralParent { parent_path } if parent_path == ROOT_PATH_ADDRESS),
            "expected EphemeralParent, got {err:?}",
        );
        assert!(
            store.is_empty(),
            "the refusal must precede any reservation append",
        );
        // The honest ephemeral request still works and records session: None.
        let child = binding
            .branch_child(&store, &request("worker", ChildDurability::Ephemeral))
            .unwrap();
        assert!(child.session_id.is_none());
        assert!(matches!(
            &store.events()[0],
            SessionEvent::ChildBranch {
                child_session_id: None,
                parent_session_id: None,
                ..
            }
        ));
    }

    #[test]
    fn stale_binding_cannot_mint_under_recreated_parent_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manager = SessionManager::new(tmp.path());
        let opened = manager.create_with_id("parent-aba", options(), DurabilityPolicy::Flush)?;
        let original_generation = opened.entry.generation;
        let parent_store = Arc::new(opened.store);
        let binding = SessionBinding::persistent_root(
            Arc::new(SessionBrancher::new(
                manager.clone(),
                opened.entry.id.clone(),
                DurabilityPolicy::Flush,
            )),
            &opened.entry,
            &[],
        );

        manager.delete("parent-aba")?;
        let replacement =
            manager.create_with_id("parent-aba", options(), DurabilityPolicy::Flush)?;
        assert_ne!(original_generation, replacement.entry.generation);

        let child_id = Uuid::new_v4().to_string();
        let error = binding
            .branch_child(
                &parent_store,
                &ChildBranchRequest {
                    child_session_id: child_id.clone(),
                    name_stem: "worker".to_owned(),
                    kind: ChildBranchKind::Spawn,
                    durability: ChildDurability::Persist,
                    model: "test-model".to_owned(),
                    working_dir: "/work".to_owned(),
                },
            )
            .err()
            .ok_or_else(|| std::io::Error::other("stale child mint unexpectedly succeeded"))?;
        assert!(matches!(
            error,
            SessionPersistError::GenerationChanged { .. }
        ));
        assert!(manager.resolve(&child_id).is_err());
        assert!(!tmp.path().join("parent-aba/children").exists());
        Ok(())
    }

    /// For-all-time uniqueness across restart: the ever-used set is
    /// re-derived from the parent's replayed events, a historical name
    /// refuses reuse, and inherited (fork-seeded) reservations are NOT
    /// counted as the child's own.
    #[test]
    fn ever_used_set_rederives_after_restart_and_filters_inherited() {
        let root = persistent_root();
        let child = root
            .binding
            .branch_child(&root.store, &request("worker", ChildDurability::Persist))
            .unwrap();
        let minted_name = child.path_address.rsplit('/').next().unwrap().to_owned();
        drop(child);
        drop(root.binding);

        // "Restart": resume the root from disk and rebuild the binding
        // from the replayed history.
        drop(root.store);
        let resumed = root
            .manager
            .resume(&root.id, DurabilityPolicy::Flush)
            .unwrap();
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id,
            DurabilityPolicy::Flush,
        ));
        let rebuilt = SessionBinding::persistent_root(
            Arc::clone(&brancher),
            &resumed.entry,
            &resumed.store.events(),
        );
        assert!(
            rebuilt.ever_used_names().contains(&minted_name),
            "the replayed set must contain the historical name",
        );
        assert!(
            !mint_child_name("worker", &rebuilt.ever_used_names()).eq(&minted_name),
            "a burned name is never re-minted",
        );

        // A DIFFERENT session rebuilding from the same events (the
        // fork-seed inheritance shape) must NOT count them as its own.
        let mut other_entry = resumed.entry.clone();
        other_entry.id = Uuid::new_v4().to_string();
        other_entry.generation = Uuid::new_v4();
        let other =
            SessionBinding::persistent_root(brancher, &other_entry, &resumed.store.events());
        assert!(
            other.ever_used_names().is_empty(),
            "inherited reservations must not over-reserve a child's namespace",
        );
    }

    /// A resumed CHILD session recovers its own path address from its
    /// provenance header, so grandchild addresses keep nesting correctly
    /// across restart.
    #[test]
    fn resumed_child_recovers_its_path_address() {
        let root = persistent_root();
        let child = root
            .binding
            .branch_child(&root.store, &request("worker", ChildDurability::Persist))
            .unwrap();
        let child_id = child.session_id.clone().unwrap();
        let child_path = child.path_address.clone();
        drop(child);

        let resumed = root
            .manager
            .resume(&child_id, DurabilityPolicy::Flush)
            .unwrap();
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id,
            DurabilityPolicy::Flush,
        ));
        let rebuilt =
            SessionBinding::persistent_root(brancher, &resumed.entry, &resumed.store.events());
        assert_eq!(rebuilt.path_address(), child_path);
    }

    /// KILL-WINDOW SIMULATION (reservation present, child absent): the
    /// state a crash between the reservation append and materialization
    /// leaves behind. Resume tolerates the dangling reference, the
    /// burned name stays reserved, and new mints proceed normally.
    #[test]
    fn crash_between_reservation_and_child_file_is_tolerated_dangling() {
        let root = persistent_root();
        let ghost_name = "worker-deadbeef";
        root.store
            .append(SessionEvent::ChildBranch {
                base: EventBase::new(root.store.last_event_id()),
                parent_session_id: Some(root.id.clone()),
                child_session_id: Some(Uuid::new_v4().to_string()),
                path_address: format!("root/{ghost_name}"),
                parent_event_anchor: root.store.last_event_id(),
                kind: ChildBranchKind::Fork,
            })
            .unwrap();
        drop(root.store);
        drop(root.binding);

        // Resume: the dangling reference must not break replay.
        let resumed = root
            .manager
            .resume(&root.id, DurabilityPolicy::Flush)
            .unwrap();

        // The burned name is reserved; a fresh mint works and picks a
        // different name.
        let brancher = Arc::new(SessionBrancher::new(
            root.manager.clone(),
            root.id,
            DurabilityPolicy::Flush,
        ));
        let binding =
            SessionBinding::persistent_root(brancher, &resumed.entry, &resumed.store.events());
        assert!(binding.ever_used_names().contains(ghost_name));
        let store = Arc::new(resumed.store);
        let fresh = binding
            .branch_child(&store, &request("worker", ChildDurability::Persist))
            .unwrap();
        assert_ne!(
            fresh.path_address,
            format!("root/{ghost_name}"),
            "the dangling name must never be re-minted",
        );
    }

    /// Depth recursion: a grandchild minted through the child's binding
    /// lands in the SAME root-keyed children/ dir under the full-path
    /// slug, and its reservation goes to the CHILD's timeline.
    #[test]
    fn grandchild_mints_under_full_path_slug() {
        let root = persistent_root();
        let child = root
            .binding
            .branch_child(&root.store, &request("fork", ChildDurability::Persist))
            .unwrap();
        let grandchild = child
            .binding
            .branch_child(&child.store, &request("spawn", ChildDurability::Persist))
            .unwrap();

        let child_name = child.path_address.rsplit('/').next().unwrap();
        let grand_name = grandchild.path_address.rsplit('/').next().unwrap();
        let expected_rel = format!("{}/children/{child_name}--{grand_name}.jsonl", root.id);
        assert!(
            root.manager.data_dir().join(&expected_rel).exists(),
            "grandchild file must live at the full-path slug",
        );

        // The grandchild's reservation lives on the CHILD's timeline.
        let child_entry = root
            .manager
            .resolve(child.session_id.as_deref().unwrap())
            .unwrap();
        let child_read =
            read_session_events_for_entry(root.manager.data_dir(), &child_entry).unwrap();
        assert!(
            child_read.events.iter().any(|e| matches!(
                e,
                SessionEvent::ChildBranch { path_address, .. }
                    if path_address == &grandchild.path_address
            )),
            "the grandchild reservation must be durable on the child's timeline",
        );
    }
}
