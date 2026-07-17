# D2 strict session-store implementation handoff

**Status:** Draft implementation handoff; source freeze and evidence collection
are in progress. This document is not a Gate D request, a test verdict, or P3/P4
acceptance.

**Owner contract:** `docs/DECISIONS-2026-07.md` section 15 and D2 in
`docs/RESPONSES-API-REMEDIATION-PLAN.md`.

**Candidate base:** `07bf9c1` is the frozen pre-D2 transcript/streaming candidate.

**D2 source commit/range:** `PENDING - record after the source is frozen`.

## 1. Claimed implementation boundary

The candidate is intended to establish only these D2 claims:

1. Standard new sessions use strict format 2 under the versionless
   `~/.norn/session-store/` namespace.
2. `~/.norn/sessions/` is never upgraded in place, deleted, hardened, or used as
   a normal-runtime history reader. It remains the old-binary namespace and
   explicit offline migration source.
3. `norn session migrate` classifies the complete legacy snapshot, writes an
   immutable digest-addressed private backup, validates a staged strict store,
   rechecks the live source digest, and publishes the backup and destination
   with no-replace directory operations.
4. Migration results are exactly `Canonical`, `FreshEpochProjection`, or
   `InspectOnly`. No migration path invents absent Responses items, reasoning,
   ordering, phase, or provider continuity.
5. Normal startup performs bounded cutover verification only. Full digest,
   manifest, backup, timeline, index, and live-source verification is an
   explicit offline operation.
6. A migrated resumable session starts one recorded fresh provider epoch.
   Flattened-but-coherent history additionally requires explicit degraded-resume
   approval. Inspect-only history cannot resume and is available only through
   verified metadata and byte-exact export.
7. `SessionManager::standard()` and the shared checked resolver apply the same
   standard-store guard to CLI and library callers. `SessionManager::new` remains
   an explicit custom-store constructor whose directory authority belongs to the
   embedder.
8. Every active index row carries a UUID-v4 generation. Retained sinks,
   registered readers, bindings, manager mutations, child publications, spool
   writers, and fetched artifact writers revalidate that generation so
   delete-and-recreate ABA cannot attach stale authority to a replacement
   session. Registered reads, repair, sink binding, and append hold the index
   generation while acquiring the timeline lock; raw row insertion, ID-only
   append, and arbitrary row-update APIs are unavailable in production builds.
9. Timeline publication and subtree deletion use durable journals. A deletion
   atomically removes the complete transitive row set before descriptor-bounded
   sequential cleanup; post-rename durability ambiguity and post-commit cleanup
   failure have distinct typed outcomes.
10. Event and usage counters are checked exact projections of strict timeline
    content. Overflow fails typed before bytes or index state can be accepted;
    saturating arithmetic is excluded from the durable format-2 path.
11. Seeded child publication records and revalidates the exact parent ID and
   generation under `index.lock`, including crash recovery. Exact owned
   index/publication temporaries are reclaimed before authority reads; prefix
   lookalikes are never treated as cleanup authority.

This handoff does **not** claim response-scoped audio, the exhaustive
all-lifecycle media matrix, final retained Gate C, independent review, or P3/P4
acceptance.

## 2. Namespace and artifact inventory

This table enumerates the D2 session-store and migration families. It does not
claim to enumerate unchanged Norn-root families owned by other subsystems, such
as `outputs/**` or `tasks/**`.

| Relative to the trusted Norn root | Authority and lifecycle |
|---|---|
| `session-store/` | Active versionless strict store; atomically published by migration or created for native strict sessions. |
| `sessions/` | Untouched legacy/old-binary namespace; explicit offline source only. |
| `session-migration-backups/<source-tree-sha256>/sessions/` | Immutable private byte-for-byte backup used for verification and inspect-only export. |
| `.session-store-stage/` | Owned recoverable strict-store staging directory; never an active store. Before no-replace directory publication it mirrors the applicable `session-store/` marker, index, timeline, migration-evidence, and migrated auxiliary families enumerated below. |
| `.session-backup-stage/` | Owned recoverable backup staging directory; never an active backup. It contains the ownership marker and a byte-exact `sessions/**` subtree before no-replace directory publication. |
| `session-migration.lock` | Inter-process migration transaction lock. |
| `session-store/.norn-migration-stage-owner` | Exact versioned ownership and source-digest marker. |
| `session-migration-backups/<source-tree-sha256>/.norn-migration-stage-owner` | Exact versioned ownership marker for the published backup container. |
| `session-store/migration-cutover-receipt.json` | Fixed-size bounded startup proof. |
| `session-store/migration-manifest.json` | Versioned classification and immutable source-selector record; offline inspection input. |
| `session-store/migration-initial-index.jsonl` | Immutable publication-time index evidence; never the live index authority. |
| `session-store/index.jsonl` | Mutable strict runtime index authority. |
| `session-store/index.lock` | Retained inter-process lock for index recovery and mutation transactions. |
| `session-store/index.jsonl.tmp.<uuid>` | Exact canonical-UUID atomic-index temporary; discarded under `index.lock` before recovery or reads. Prefix lookalikes are foreign conflicts, never cleanup authority. |
| `session-store/.provider-epoch-locks/` | Directory for one-time migrated provider-epoch serialization. |
| `session-store/.provider-epoch-locks/<session-id>.lock` | Retained per-session lock file for the one-time migrated provider-epoch boundary. |
| `session-store/<session-id>.jsonl` | Strict root-session timeline. |
| `session-store/<root-id>/children/<path-slug>.jsonl` | Strict child timeline, located only through its index row. Descendants remain rooted under the ultimate root directory. |
| `session-store/<root-id>/artifacts/` | Eagerly created native artifact capability root; it may remain empty. |
| `session-store/<root-id>/spool/<event-id>.bin` | Native immutable verbatim oversized tool-result payload owned by the root session. |
| `session-store/<root-id>/artifacts/fetched/<uuid>.md` | Native immutable fetched-document artifact owned by the root session. |
| `session-store/<root-id>/spool/**` | Migrated legacy auxiliary subtree. Migration preserves every observed regular file below a resumable root's legacy `spool/` tree, not only native event-ID names. |
| `session-store/<root-id>/artifacts/**` | Migrated legacy auxiliary subtree. Migration preserves every observed regular file below a resumable root's legacy `artifacts/` tree, not only native fetched-document names. |
| `session-store/.timeline-locks/<sha256>.lock` | Central inter-process lock for one normalized timeline path; the digest uses a domain separator and length-framed path components. |
| `session-store/.norn-publication-<uuid>.json` | Durable crash-recovery journal for atomic timeline/index publication. |
| `session-store/.norn-publication-journal-<uuid>.tmp` | Pre-publication journal temporary file; an exact UUID artifact without its matching durable journal is reclaimed under `index.lock`. |
| `session-store/.norn-publication-timeline-<uuid>.stage` | Staged timeline paired with a publication journal; an exact UUID artifact without its matching durable journal is reclaimed under `index.lock`. |
| `session-store/.session-deletion.<uuid>.json` | Durable index-first deletion journal; removed after descriptor-bounded sequential registered-timeline cleanup and optional whole-root artifact-tree removal. |
| `session-store/.session-deletion.<uuid>.json.tmp` | Pre-publication deletion-journal temporary file. |

Deleting a child removes its timeline and the timelines/index rows of every
transitive descendant. Spool and fetched files are root-owned rather than
child-owned, so they remain until the root session is deleted. Deleting the root
removes its complete `<root-id>/` artifact tree after all subtree rows are
atomically removed from the index.

Deletion cleanup is descriptor-bounded, not time-bounded: it holds at most one
timeline lock at a time, but acquiring that lock has the persistence layer's
existing no-implicit-timeout policy. A wedged timeline holder can therefore
keep the global index transaction waiting after logical deletion. The durable
deletion journal makes a future deadline-aware cleanup policy possible without
re-exposing rows, but D2 does not claim that policy.

The final reviewer must verify this enumeration mechanically against every file
name created, opened, renamed, or removed by the candidate. A missing family
invalidates any "complete namespace coverage" claim.

## 3. Source inventory

The current logical implementation inventory is below. The final handoff must
replace this working inventory with the mechanically generated
`git diff --name-only <base>..<candidate>` list and explain every extra or missing
path.

### Strict format and runtime persistence

- `crates/norn/src/session/persistence/strict/`
- `crates/norn/src/session/persistence/strict_runtime.rs`
- `crates/norn/src/session/persistence/event_reader.rs`
- `crates/norn/src/session/persistence/index.rs`
- `crates/norn/src/session/persistence/index_artifacts.rs`
- `crates/norn/src/session/persistence/index_codec.rs`
- `crates/norn/src/session/persistence/index_deletion.rs`
- `crates/norn/src/session/persistence/index_deletion_recovery.rs`
- `crates/norn/src/session/persistence/index_resolve.rs`
- `crates/norn/src/session/persistence/index_timeline.rs`
- `crates/norn/src/session/persistence/counters.rs`
- `crates/norn/src/session/persistence/io.rs`
- `crates/norn/src/session/persistence/replay.rs`
- `crates/norn/src/session/persistence/types.rs`
- `crates/norn/src/session/jsonl_sink.rs`
- `crates/norn/src/session/artifacts.rs`
- `crates/norn/src/session/spool.rs`
- `crates/norn/src/session/store.rs`

### Transaction, publication, and concurrency

- `crates/norn/src/session/persistence/publication.rs`
- `crates/norn/src/session/persistence/publication_conflict.rs`
- `crates/norn/src/session/persistence/publication_hash.rs`
- `crates/norn/src/session/persistence/publication_names.rs`
- `crates/norn/src/session/persistence/publication_parent.rs`
- `crates/norn/src/session/persistence/publication_recovery.rs`
- `crates/norn/src/session/persistence/publication_timeline_error.rs`
- `crates/norn/src/session/persistence/timeline_file.rs`
- `crates/norn/src/session/persistence/timeline_lock.rs`
- `crates/norn/src/session/branch.rs`
- `crates/norn/src/session/branch_materialize.rs`
- `crates/norn/src/session/manager/fork.rs`
- `crates/norn/src/session/manager.rs`

### Offline migration and resume policy

- `crates/norn/src/session/migration/`
- `crates/norn/src/session/manager/open.rs`
- `crates/norn/src/session/manager/resume_policy.rs`
- `crates/norn/src/session/manager/standard.rs`
- `crates/norn/src/config/paths.rs`

### CLI and integration entry points

- `crates/norn-cli/src/cli/args.rs`
- `crates/norn-cli/src/cli/session_args.rs`
- `crates/norn-cli/src/commands/session.rs`
- `crates/norn-cli/src/commands/session_legacy.rs`
- `crates/norn-cli/src/config/paths.rs`
- `crates/norn-cli/src/runtime/from_cli.rs`
- `crates/norn-tui/src/app/slash.rs`
- `crates/norn/src/agent/resume.rs`
- `crates/norn/src/agent/session_open.rs`
- `crates/norn/src/agent/session_spec.rs`

### Candidate test inventory

- `crates/norn/src/session/persistence/strict/reader_tests.rs`
- `crates/norn/src/session/persistence/strict/validation_tests.rs`
- `crates/norn/src/session/persistence/strict/index_relationship_tests.rs`
- `crates/norn/src/session/persistence/index_strict_tests.rs`
- `crates/norn/src/session/persistence/deletion_runtime_tests.rs`
- `crates/norn/src/session/persistence/counter_overflow_tests.rs`
- `crates/norn/src/session/persistence/publication_tests.rs`
- `crates/norn/src/session/persistence/timeline_concurrency_tests.rs`
- `crates/norn/src/session/persistence/timeline_runtime_tests.rs`
- `crates/norn/src/session/migration/tests.rs`
- `crates/norn/src/session/migration/hardening_tests.rs`
- `crates/norn/src/session/manager/standard_tests.rs`
- `crates/norn/src/session/manager/tests/resume_policy.rs`
- `crates/norn/src/session/provider_epoch_tests.rs`
- `crates/norn/src/session/artifacts_tests.rs`
- `crates/norn/src/tests/descriptor_retention.rs`
- `crates/norn/src/config/paths_session_tests.rs`
- CLI path, migration, legacy-inspection, and degraded-resume tests colocated in
  their production modules.

## 4. Contract-to-evidence matrix

All evidence cells deliberately remain open until generated from the frozen
candidate. A source test existing is not a recorded pass.

| Contract | Required evidence | Status |
|---|---|---|
| Strict format-2 only | Exact header, unknown-field/event, duplicate-key/id, non-canonical row, legacy/newer version, malformed row, and torn-tail fixtures | [ ] `PENDING` |
| Versionless standard namespace | CLI and public library constructor fixtures, including relative/unavailable home failure | [ ] `PENDING` |
| No normal legacy content read | Unreadable/renamed legacy-content fixture proving bounded startup still succeeds after valid cutover | [ ] `PENDING` |
| Atomic offline publication | Enumerated crash seams before/after backup and store publication; no foreign destination replacement | [ ] `PENDING` |
| Idempotence and interruption recovery | Repeated same-source result plus owned-stage recovery and changed-source behavior | [ ] `PENDING` |
| Immutable legacy and backup | Before/after source tree digest, backup digest, exact export bytes, and old-binary divergence detection | [ ] `PENDING` |
| Three fidelity classes | Canonical, flattened coherent, malformed/ambiguous, spoofed boundary, stale index, orphan, and duplicate fixtures | [ ] `PENDING` |
| Fresh provider epoch | Canonical migrated resume, explicit degraded approval, inspect-only refusal, and concurrent one-boundary distribution | [ ] `PENDING` |
| Timeline/index atomicity | Publication/deletion-journal crash recovery, first-publication recovery, same-name convergence, reader/writer exclusion, transitive delete/writer exclusion, and exact residue checks | [ ] `PENDING` |
| Generation/ABA isolation | Stale manager rename/reconcile, registered read/repair, sink construction/append, binding, child publication, spool, and fetched-artifact handles after delete/recreate; deterministic index-before-timeline contention | [ ] `PENDING` |
| Exact usage/count accounting | Overflow, mismatch, append ambiguity, and recovery fixtures | [ ] `PENDING` |

## 5. Gate and distribution placeholders

| Gate | Command or evidence generator | Result |
|---|---|---|
| Formatting | `cargo fmt --all -- --check` | [ ] `PENDING` |
| Strict lint | `cargo clippy --workspace --all-targets -- -D warnings` | [ ] `PENDING` |
| Workspace tests | `cargo test --workspace --all-targets` | [ ] `PENDING` |
| Doctests | `cargo test --workspace --doc` | [ ] `PENDING` |
| Focused strict codec/store | `cargo test -p norn session::persistence` | [ ] `PENDING` |
| Focused migration | `cargo test -p norn session::migration` | [ ] `PENDING` |
| Focused manager/resume | `cargo test -p norn session::manager` | [ ] `PENDING` |
| CLI session surface | `cargo test -p norn-cli session` | [ ] `PENDING` |
| Standard-path surface | `cargo test -p norn config::paths` and `cargo test -p norn-cli config::paths` | [ ] `PENDING` |
| Policy diff | Added `unwrap`/`expect`/`panic`, lint bypass, ignored test, and arbitrary-limit scan against the frozen base | [ ] `PENDING` |
| Production LOC | One mechanical production-prefix method for every touched Rust file; strict `<500` result required | [ ] `PENDING` |

The final evidence bundle must record distributions rather than last-run results.
Every concurrency or crash-sensitive test must run at least 20 times, with test
name, command, candidate commit, pass count, fail count, and observed recovery
events retained. Required distributions include:

- same-session concurrent publication convergence;
- same-timeline concurrent writers;
- strict reader versus writer torn-tail exclusion;
- delete versus live writer exclusion;
- one-time migrated provider-epoch boundary insertion; and
- interrupted migration recovery at every enumerated publication seam.

**Retained distribution files:** `PENDING - list exact repository paths and
SHA-256 digests after generation`.

**Retained full-gate file:** `PENDING - list exact repository path and SHA-256
digest after generation`.

## 6. Required review

- [ ] A session-persistence reviewer enumerates every durable write and
  publication seam, independently reruns the distributions, and verifies the
  namespace inventory.
- [ ] A Responses-protocol reviewer checks that migration classification and
  fresh-epoch behavior never manufacture canonical provider semantics.
- [ ] An embedder/API reviewer verifies `SessionManager::standard()` is the
  documented standard front door and custom stores are not represented as
  standard cutover-safe storage.
- [ ] A Fable adversarial reviewer returns `READY` for the frozen D2 range.

## 7. Open before P3/P4 acceptance

- [ ] Freeze and commit the complete D2 source range.
- [ ] Fill every evidence placeholder above from that exact commit.
- [ ] Resolve every finding from the independent D2 review.
- [ ] Add response-scoped audio without inventing a terminal output item.
- [ ] Complete the exhaustive all-discriminator/optional-shape lifecycle matrix.
- [ ] Run and retain final P3/P4 Gate C evidence.
- [ ] Obtain independent P3/P4 acceptance.
