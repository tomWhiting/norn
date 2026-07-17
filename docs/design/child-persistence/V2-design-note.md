# V2 — Child persistence, storage layout, path addressing — DESIGN NOTE

> **Historical design note (superseded for local session persistence).** This
> document preserves the July 2026 child-persistence design record. Its tolerant
> reader and flat-layout compatibility assumptions predate Responses D2. The
> active runtime uses strict format-2 files under the versionless
> `~/.norn/session-store/` namespace, and legacy `~/.norn/sessions/` data enters
> only through the explicit offline migration contract in
> `docs/RESPONSES-API-REMEDIATION-PLAN.md`. The historical "V2" label is not a
> runtime directory or current format name.

Author: Dr. Spaceman (V2 owner) · 2026-07-04 · **Status: for Sable's review before any code**

Scope: the four deliverables Sable named — (a) branch/session-mint primitive API,
(b) `spawn.rs`/`fork_pipeline.rs` split plan, (c) per-parent name registry durable form,
(d) how existing flat-layout sessions stay readable. Plus the cross-cutting decisions the
briefs left implicit (id-space reconciliation, R3 event schema, R7 verdict, rhai fidelity).

Rulings already in from Sable are treated as fixed inputs and cited as **[R-Q1..Q3]**.

## 0. Evidence base — every claim below is re-derived from source

Four read-only recons over `session/`, the mint sites, `persistence/`, and the
rhai + `signal_agent` + registry paths, plus my own verification of the load-bearing
claims. File:line anchors are given inline. Where a brief premise disagrees with the
code, the code wins and the disagreement is surfaced in §1 first — per our doctrine that
an internal contradiction is the first bug to fix, before any implementation.

---

## 1. Corrections to the brief's premises (surface these before building on them)

Six premises in Brief V2 do not survive contact with the source. None are fatal; each
changes a design decision, and two need an owner ruling.

### C1 — The 500-LOC split premise rests on `wc -l`, not the CLAUDE.md rule. **[DECISION]**
Acceptance criterion says "500-LOC compliance maintained (spawn.rs and fork_pipeline.rs
are already large — expect module splits)." Measured by CLAUDE.md's *own* definition
(exclude tests, comments, blank lines):

| File | `wc -l` total | `#[cfg(test)]` at | real code-LOC before it | fn/impl after boundary |
|---|---|---|---|---|
| `spawn.rs` | 5828 | 651 | **430** | 0 (all test-internal) |
| `fork_pipeline.rs` | 1141 | 534 | **364** | 0 (all test-internal) |

Independently verified: `awk 'NR<boundary'` then strip comment-only + blank lines.
**Both files already comply.** The raw totals are ~92% `#[cfg(test)]`. So a split is a
*cohesion* choice, not a CO5 requirement. See §3 for the decision I'm putting to Sable.

### C2 — "Index self-heal already tolerates layout drift" is false against the code. **[core of deliverable (d)]**
There is **no directory glob or walk anywhere in the production session layer.** Discovery
is 100% manifest-driven: `read_index` reads `{data_dir}/index.jsonl` line by line
(`session/persistence/index.rs:42-67`); `SessionManager::list()` *is* `read_index`
(`manager.rs:442-444`). The file path is always derived flat:
`session_file_path(data_dir, id) = data_dir/{id}.jsonl` — no subdir component ever
(`persistence/io.rs:22-25`).

The actual "self-heal" — `reconcile_index_entry` (`manager.rs:641-682`) — is a **per-entry
numeric reconciler**: it repairs a resolved entry's `event_count`/token totals when they
drift from its file. It does not discover sessions, does not reconcile the row-set against
disk, does not know directories exist. A separate tolerance layer, the JSONL reader
(`io.rs:245-326`), skips torn/unknown/duplicate lines — *content* drift within one file,
not *layout* drift.

**Consequence:** the nested `<root>/children/<slug>.jsonl` + `spool/` layout is not
"tolerated" — it is **invisible**, because nothing enumerates the directory and
`session_file_path` cannot produce a nested path. This is *good news*, not bad: it means
the fix is a small, robust change to how the index locates a file, not a filesystem
crawler. See §5.

### C3 — `SessionTree` is production-dead (settles R7 empirically).
Every `SessionTree::new` and every `insert_extension(SharedSessionTree)` is inside
`#[cfg(test)]` (`tree.rs` tests; `spawn.rs:2562/4376/5361`; `fork_tool.rs:1405` — all past
their test boundaries). The two *production* functions that mention `SharedSessionTree`
(`resolve_child_store` `spawn.rs:171-173`, `resolve_fork_store` `fork_pipeline.rs:224-226`)
only *derive* a child handle from an already-published parent tree, and nothing publishes
one. So `get_extension::<SharedSessionTree>()` is always `None` in production and both
paths always take the standalone `Arc::new(EventStore::new())` arm. `tree.rs:10-11`
documents the tree as "purely in-memory: no persistence, replay, or merging." **R7 verdict
in §6.4: delete.**

### C4 — `DurabilityPolicy` is fsync-cadence, not persist-vs-ephemeral.
Its variants (`store.rs:96-114`) are `Flush` / `FsyncPerEvent` / `FsyncEveryEvents(n)` —
all assume a sink exists; they tune *when* to fsync, never *whether* to persist. So it is
**not** the vehicle for `--no-session` propagation **[R-Q1]**. Child-persistence (persist
vs ephemeral) is a separate axis, modelled on the sink's presence (`sink: Option<...>`,
`store.rs:30-33`). The design threads it as an explicit `ChildDurability` choice (§2).

### C5 — The mint sites cannot reach `data_dir`/`SessionManager` today.
`AgentToolInfra` (`infra.rs:33-72`) carries the parent's `event_store`, `registry`,
`provider`, `agent_id`, `grant`, `tool_registry` — **no `data_dir`, no `SessionManager`.**
To create a child session *file* the primitive needs a session-creation handle that is not
currently threaded to spawn/fork/rhai. Threading it (recursively) is part of deliverable
(a), §2.3.

### C6 — Two id spaces the branch-point event must reconcile.
`SessionEvent::Fork.forked_session_id` is the SessionTree `SessionId` = `Uuid::new_v4()`
(`tree.rs:220`); `subagent.started.child_id` is the **registry agent `Uuid`**
(`provider/agent_event.rs:120-129`). They are different id spaces. The campaign vision
fixes storage identity as **UUIDv7**. §6.1 unifies these: the child's *session identity*
(index key + file identity) is one UUIDv7, carried alongside the path address in a single
branch-point event.

---

## 2. Deliverable (a) — the branch/session-mint primitive

### 2.1 The gap it closes
Every child-store mint in production is sink-less:
- spawn: `resolve_child_store` → `Arc::new(EventStore::new())` (`spawn.rs:171-172`),
- fork: `resolve_fork_store` → `EventStore::new()` (`fork_pipeline.rs:224-226`),
- rhai: `let child_store = EventStore::new();` (`integration/rhai/agent_ops.rs:364`), and
  the rhai path is *worse* — `event_tx: None` and no `LifecycleEmitter`, so it emits no
  `subagent.started/completed` and no branch event at all.

`EventStore::new()` sets `sink: None`; `append()` on a sink-less store pushes to memory
and silently persists nothing (`store.rs:460-468`). So children vanish on drop. This is
V2-R2, and it must be fixed on all three paths *and* recursively (grandchildren).

### 2.2 The primitive — `SessionManager::branch_child`
A new method on `SessionManager` (the existing session-creation authority;
`manager.rs:146-205` shows it already owns `create`/`create_with_id`/`fork` and the index
writes). Proposed signature:

```rust
/// Mint a persistent child session branched from `parent`, writing its file under the
/// parent root's `children/` dir and recording a durable branch-point on the parent.
pub fn branch_child(
    &self,
    parent_store: &EventStore,     // parent's live sink-equipped store (append anchor + branch event)
    branch: ChildBranchSpec,       // see below
    durability: DurabilityPolicy,  // fsync cadence for the CHILD file (inherited from parent unless overridden)
) -> Result<OpenSession, SessionPersistError>;

pub struct ChildBranchSpec {
    pub child_session_id: SessionId,      // UUIDv7 storage identity (== registry agent id; see §6.1)
    pub root_session_id: SessionId,       // which root dir the child file lives under
    pub path_address: String,            // full coordination path, e.g. "root/reviewer-kestrel"
    pub path_slug: String,               // full-path slug for the filename [R-Q2]
    pub parent_event_anchor: Option<EventId>, // parent_store.last_event_id() at branch time
    pub seed: ChildSeed,                 // Fork = full parent-history copy; Spawn = fresh (task seed only)
    pub persistence: ChildDurability,    // Persist | Ephemeral  (the honest --no-session axis; §2.4)
}
```

**What it does, in order:**
1. If `persistence == Ephemeral`: return a sink-less `OpenSession` (`EventStore::new()`),
   append the branch-point event to `parent_store` with `session: None` **[R-Q1]**, and
   allocate the name (§4) — but write no child file, no index row. Ephemeral is a first-
   class, honestly-recorded outcome, not the absence of a mint.
2. Otherwise compute the child file path `<root-uuid>/children/<path_slug>.jsonl`, creating
   the nested dir (and `<root-uuid>/spool/`) if absent.
3. Build a `JsonlSink` at that path (respecting `durability`), wrapped by
   `EventStore::with_sink_and_events(sink, seed_events)` — **sink-equipped**
   (`store.rs:396-420`). `seed_events` is the parent-history copy for forks, empty for
   spawns.
4. Write the child header event (session header carrying `path_address`, `parent_id`,
   `parent_event_anchor`).
5. Insert a `SessionIndexEntry` for the child — **carrying its relative on-disk path and
   parent linkage** (§5.2), so manifest-driven discovery finds it without a glob.
6. Append the **branch-point event** to `parent_store` (§6.2) carrying
   `{ child_session_id, path_address, parent_event_anchor, session: Some(child_session_id) }`.
   This single event satisfies R3 (durable linkage) **and** is the name-allocation record
   for (c) — one event, two requirements.
7. Return `OpenSession { store, id: child_session_id }`.

Steps 5 and 6 are the durable linkage; step 6's parent append and step 4/5's child writes
should be ordered child-first so a crash between them leaves an orphan child file that the
next boot's reconcile can still index from its own header (fail-safe, not fail-lost).

### 2.3 Who mints, and how the three paths route through it
All three current sink-less mints are replaced by a call to `branch_child`:
- `resolve_child_store` (`spawn.rs:166-201`) and `resolve_fork_store`
  (`fork_pipeline.rs:220-253`) call it, passing `ChildSeed::Fresh` / `ChildSeed::History`
  respectively.
- rhai `agent_ops.rs:364` calls it too — and additionally gains a `LifecycleEmitter` and a
  real `event_tx` so grandchild audits reach disk (Gap 11) and `subagent.started/completed`
  fire (closing the broader rhai fidelity hole flagged in recon).

**Reachability (C5):** `SessionManager` (or a narrower `SessionBranchService` wrapping
`data_dir` + `index_lock_deadline`) is added as a field on `AgentToolInfra`
(`infra.rs:33-72`), populated where the root infra is built, and forwarded through
`build_fork_context` (`fork_pipeline.rs:99-204`) and the spawn `build_child_context` path
so **every** child — and recursively every grandchild — inherits the handle. A child that
holds the handle can itself mint persistent children; that is how depth-recursion (R2) is
satisfied structurally rather than per-call.

### 2.4 `ChildDurability` and the `--no-session` propagation **[R-Q1]**
`Persist` is the default (the whole point of V2). `Ephemeral` is the explicit opt-out that
propagates down the subtree: a child minted `Ephemeral` threads `Ephemeral` into its own
infra so its descendants are ephemeral too ("explicit choice propagates down", R2). Because
`DurabilityPolicy` cannot express this (C4), `ChildDurability` is a new two-variant enum
carried on `ChildBranchSpec` and on the child's infra. It is orthogonal to the fsync
cadence, which continues to be `DurabilityPolicy` and is inherited from the parent unless
the embedder overrides it.

---

## 3. Deliverable (b) — the split plan (reframed by C1)

Because both files already meet the real-code rule (§C1), I am **not** proposing a split to
chase CO5. I am putting a scoped, cohesion-only split to Sable as **[DECISION D-b]** with a
default of *do the fork one, skip the spawn one*:

**`fork_pipeline.rs` — recommend splitting (clean seam already latent).** Two orthogonal
clusters, linked only by the `ForkOutcome` type:
- `fork_context.rs` ← `build_fork_context` + `ForkStoreResolution` + `resolve_fork_store`
  (99–253). This is where the `branch_child` call lands, so it will *grow*; giving it its
  own file keeps the change local.
- `fork_outcome.rs` ← `ForkOutcome` + `project_fork_outcome` + `mark_fork_terminal` +
  `panicked_fork_outcome` + `classify_step_result` + `append_fork_complete` (257–532).
  `classify_step_result` is the only file-private item and travels cleanly with this
  cluster. `mod.rs` moves the `pub(crate) use ...ForkOutcome` re-export and `fork_tool.rs`
  updates its `use super::fork_pipeline::…` paths.

**`spawn.rs` — recommend NOT splitting.** At 430 real-LOC it is comfortably compliant, and
its helpers (`resolve_child_store`, `build_child_loop_context`, `build_tool_definitions`,
`SpawnAgentArgs`) are all file-private and used only by `execute`; extracting them buys
~110 LOC of headroom we don't need and adds a module boundary for no invariant. The V2 edit
to `resolve_child_store` is contained.

If Sable wants uniform module hygiene regardless of the count, the spawn seam is
`spawn_prepare.rs` ← the three helpers + `SpawnAgentArgs`; I'll do it, but I'd rather not
add structure the rule doesn't require. **Default: split fork, leave spawn.**

Either way, the seam Nigel and I share is `resolve_child_store`/`resolve_fork_store` — the
exact functions V1-R3/R6 (variant tools) and V2-R2 (sinks) both touch. §7 sequences that.

---

## 4. Deliverable (c) — the per-parent name registry, durable form **[R-Q2]**

**Requirement (Sable's ruling):** names unique **for-all-time within a parent** via an
append-only per-parent registry; a dead child's name is never re-minted under the same
parent. The in-memory `AgentRegistry.path_index` (`registry.rs:163`) is **live-only** —
freed on terminal transition — so it *cannot* be the source of for-all-time uniqueness. The
durable source must be the parent's own event log.

**Design: the parent timeline IS the registry.** Each child mint appends exactly one
branch-point event to the parent's persisted store (§2.2 step 6, §6.2). That event carries
the child's allocated `path_address` (hence its short name). To allocate a new unique name
under a parent:
1. On parent load/resume, replay the parent's events and collect every name ever recorded
   in a branch-point event into an in-memory `HashSet<String>` — the **ever-used set** for
   that parent. Seeded once; cheap (the parent already replays its events on resume).
2. Mint `<variant-or-role>-<short-random>` **[R-Q2]**, reject on collision with the
   ever-used set, retry. On success, append the branch-point event (which *is* the durable
   record) and add the name to the in-memory set.

Properties: **durable** (survives restart — the set rebuilds from the log), **append-only**
(events are never deleted; `store.rs:19-23`), **for-all-time** (a terminated child's
branch-point event stays in the parent log forever, so its name stays reserved). No new
on-disk file, no schema beyond the branch-point event we already need for R3. This is the
minimal honest form: the registry is not a separate artifact that can drift from the
timeline — it *is* the timeline.

Ephemeral parents (no sink) hold the ever-used set in memory only, which is correct: an
ephemeral subtree has no cross-restart identity to protect.

---

## 5. Deliverable (d) — existing flat sessions stay readable (given C2)

### 5.1 Why legacy files are safe
Legacy flat sessions are `data_dir/{id}.jsonl` with an `index.jsonl` row keyed by `id`.
Discovery is manifest-driven and keyed on `id` (`index.rs:173,201,258`), and the path is
derived flat. As long as we do not change how a *row without a path field* resolves, every
existing session keeps resolving exactly as today. **Verified, not assumed:** the resolver
never touches the directory, so adding nested child dirs/files cannot perturb legacy
resolution (nothing enumerates them).

### 5.2 The one change discovery needs — path-carrying index entries
`SessionIndexEntry` (`persistence/types.rs:136-169`) has no path and no parent field; the
path is *derived* from the id. To make nested children discoverable **without** a
filesystem crawler, add two optional fields:
- `rel_path: Option<String>` — the file's path relative to `data_dir`
  (`<root>/children/<slug>.jsonl` for children; absent for legacy/root).
- `parent_id: Option<String>` — session lineage (also satisfies the R3 index-side linkage).

Resolution becomes: `entry.rel_path.map(|p| data_dir.join(p)).unwrap_or_else(|| session_file_path(data_dir, &entry.id))`.
Legacy entries (no `rel_path`) fall through to the existing flat derivation — **zero
migration, zero rewrite of old rows.** New children write `rel_path`. This keeps discovery
manifest-driven (more robust than a glob) and is a strictly additive schema change
(`serde(default)` on the two new fields; the tolerant reader already skips unknown fields,
`io.rs:289-313`, so a downgrade is safe too).

### 5.3 Reconcile extension (bounded)
`reconcile_index_entry` (`manager.rs:641-682`) stays a per-entry numeric reconciler; it
gains nothing beyond reading the child file at its `rel_path` instead of the derived flat
path. I am **not** adding a directory-crawling orphan-recovery pass — that would be new
arbitrary machinery the brief doesn't call for; if a child file exists with no index row
(crash between §2.2 step 4 and 5), its own header carries `parent_id` + `path_address`, so a
targeted reindex-on-parent-resume can re-add the row from known children. Scope that as a
follow-up only if the kill-9 acceptance test (V2 AC-1) demands it; I'll let the test decide,
per the playbook's "empirical proof beats plausible reasoning."

---

## 6. Cross-cutting decisions

### 6.1 Id-space reconciliation (resolves C6)
The child's **session identity** and its **registry agent id** are unified to one UUIDv7,
minted once at reserve time (`AgentRegistry::reserve`, `registry.rs:487-495`) and passed as
`ChildBranchSpec.child_session_id`. So: the file is `children/<slug>.jsonl`, the index row
is keyed by that UUIDv7, `subagent.started.child_id` carries it, and the branch-point event
carries it. One identity, three surfaces — no more v4/v7 split (`tree.rs:220`'s `new_v4`
dies with the tree, §6.4). Path address remains the *coordination* layer, an alias resolved
to the id (§6.3). This is exactly the vision's "UUIDv7 is storage identity; paths are the
human/coordination layer."

### 6.2 The R3 branch-point event
Reuse-or-new decision: `SessionEvent::Fork` (`events.rs:210-218`) already carries
`source_event_id` (the anchor) + `forked_session_id`, but lacks a path field and is
semantically fork-only. I propose a **new `SessionEvent::ChildBranch`** (not overloading
`Fork`, which the tree owns and which dies with it) carrying:
`{ base, parent_event_anchor: Option<EventId>, child_session_id: Option<SessionId>, path_address: String, kind: Spawn|Fork }`.
`child_session_id: None` is the honest sink-less/ephemeral case **[R-Q1]**. It is appended
to the parent's persisted store, so it lands durably in `{parent}.jsonl`. `ForkComplete`
(`events.rs:229-241`, appended by `append_fork_complete` `fork_pipeline.rs:505-532`) stays
as the *completion* reference but its `forked_session_id` fallback-to-`fork_id` lie
(`fork_pipeline.rs:513-514`) is killed: it becomes `Option<SessionId>`, `None` when there is
no child file **[R-Q1]** — matching the `ChildBranch` anchor.

### 6.3 Path addresses as aliases (R4) — both chokepoints
`resolve_agent` (`infra.rs:154-188`) already treats full paths as aliases via
`path_index`; the rhai path has a parallel `resolve_recipient` (`agent_ops.rs:139-171`).
R4's short per-parent names are a thin indirection *in front of* both: a
`name → full-path → id` lookup added to **both** chokepoints (or a shared resolver both
call) so they cannot desync. `signal_agent` then accepts `root/reviewer-kestrel` as it does
a UUID today. The registry grows a per-parent short-name index backing this (populated at
reserve; the durable truth is still the parent log, §4).

### 6.4 R7 verdict — delete `SessionTree`
It is production-dead (C3), and the in-memory tree view it would provide is already served
by `AgentRegistry` (`parent_id` + `path_index` + `children(parent_id)`,
`registry.rs:262`). Persistence now routes through `SessionManager::branch_child`, not the
tree. So the tree is redundant on both axes (persistence and in-memory index). **Delete
`tree.rs` and the dead `SharedSessionTree` arms** in `resolve_child_store`/
`resolve_fork_store`; no zombie code (CLAUDE.md: no backwards-compat, no dead machinery).
Flagging as **[DECISION D-r7]** for Sable's explicit sign-off since it deletes a module.

### 6.5 Spool (R5) and stop-reason events (R6) — brief unchanged, noted
Both are self-contained and unaffected by the corrections: spool writes
`<root>/spool/<event-id>.bin` (the `spool/` dir the primitive creates in §2.2 step 2), full
output persisted, capped projection at prompt-build (`tool/output_budget.rs`); stop-reason
Customs append at the exits (`loop/runner/entry.rs:306` for timeout). Detailed in the
implementation-order doc once the design is ruled; called out here only to confirm they
inherit the new layout cleanly.

---

## 7. The Nigel seam (V1 ↔ V2)
V1-R3/R6 rewrite the child's tool registry subset inside `resolve_child_store`/
`resolve_fork_store`; V2-R2 rewrites the *store* minted in the same two functions. Per the
dispatch note, these edits are **sequenced, not shared**. Proposal: V2 lands the
`branch_child` call + `ChildDurability` threading first (it changes the store line and the
infra struct); V1 then layers the variant `tools = allowlist ∩ policy` **[R-Q3]** onto the
registry line. I'll hand Nigel the exact post-V2 line numbers when V2's edit is staged, and
the shared `AgentToolInfra` field addition (C5) is coordinated so we don't both edit
`infra.rs:33-72` blind.

---

## 8. Decision points for Sable (need a ruling)
- **D-b (split):** default = split `fork_pipeline.rs` into `fork_context.rs` +
  `fork_outcome.rs`; **leave `spawn.rs` unsplit** (already CO5-compliant). Override if you
  want uniform module hygiene regardless of LOC.
- **D-r7 (delete SessionTree):** default = delete `tree.rs` + the dead tree arms.
- **D-event (new `ChildBranch` variant vs overloading `Fork`):** default = new variant;
  `Fork` dies with the tree.
- **D-idspace:** default = unify child session id and registry agent id to one UUIDv7.
- Confirm **`ChildDurability` as a new axis** (not `DurabilityPolicy`) is acceptable given
  C4 — it's the only honest way to model `--no-session` propagation.

## 9. Open verification items (before/during implementation, not blocking design review)
1. Where exactly the root `AgentToolInfra` is constructed, to place the `SessionManager`
   handle (C5) — grep the harness entry, not yet traced.
2. Confirm `AgentRegistry::reserve` mints (or can be made to mint) UUIDv7 so §6.1 holds
   without a second id.
3. The kill-9 mid-fork acceptance test (V2 AC-1) decides whether §5.3 needs the targeted
   reindex-on-resume or the crash window is already covered by child-first ordering.
4. `spool/` over-budget threshold reads from `tool/output_budget.rs` — confirm the cap is
   embedder-config, not an invented constant (NO-ARBITRARY-LIMITS).
</content>
