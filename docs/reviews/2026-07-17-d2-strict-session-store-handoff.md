# D2 strict session-store implementation handoff

**Status:** D2 accepted. The same Gate D coordinator closed F1 at `26b4e28` and
returned unconditional `READY` for corrected range `2c0350d..e9755fe`. This
document remains the implementer handoff, not P3/P4 acceptance.

**Owner contract:** `docs/DECISIONS-2026-07.md` section 15 and D2 in
`docs/RESPONSES-API-REMEDIATION-PLAN.md`.

**Transcript base:** `07bf9c1` is the frozen pre-D2 transcript/streaming candidate.

**D2 implementation base:**
`2c0350d96660db3da0d1d3089dfac525b5fbbfdd`.

**D2 reviewed source/range:**
`2c0350d..3ebc468ba60152dbdb59ae9aff3ad48f15ede1fe`; reviewed tree
`b95ab9d411271769c7c0e6a305d0d4a21152b2b2`.

**Corrected D2 candidate/range:**
`2c0350d..e9755fe2533979410f53eda88966349437a54517`; correction tree
`9663e625dc57c7e23e87e3caf4f11b3adee8e231`. See the
[`D2 F1 correction handoff`](2026-07-17-d2-f1-correction-handoff.md).

## 1. Claimed implementation boundary

The candidate establishes only these D2 claims:

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

The accepted D2 boundary does **not** claim response-scoped audio, the exhaustive
all-lifecycle media matrix, final P3/P4 Gate C, or P3/P4 acceptance.

## 2. Namespace and artifact inventory

This table enumerates the D2 session-store and migration families. It does not
claim to enumerate unchanged Norn-root families owned by other subsystems, such
as `outputs/**` or `tasks/**`.

| Relative to the trusted Norn root | Authority and lifecycle |
|---|---|
| `session-store/` | Active versionless strict store; atomically published by migration or created for native strict sessions. |
| `sessions/` | Untouched legacy/old-binary namespace; explicit offline source only. |
| `session-migration-backups/` | Private container for immutable digest-addressed migration backups. |
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
| `session-store/<root-id>/` | Root-owned capability directory for child timelines, spool payloads, and fetched artifacts. |
| `session-store/<root-id>/children/` | Native child-timeline directory for the root session. |
| `session-store/<root-id>/children/<path-slug>.jsonl` | Strict child timeline, located only through its index row. Descendants remain rooted under the ultimate root directory. |
| `session-store/<root-id>/spool/` | Native immutable oversized-result payload directory. |
| `session-store/<root-id>/spool/<event-id>.bin` | Native immutable verbatim oversized tool-result payload owned by the root session. |
| `session-store/<root-id>/artifacts/` | Native artifact capability root, created when `SessionArtifactStore` is constructed while an agent session is opened; it is not eagerly created for every persisted session. |
| `session-store/<root-id>/artifacts/fetched/` | Native immutable fetched-document artifact directory. |
| `session-store/<root-id>/artifacts/fetched/<uuid>.md` | Native immutable fetched-document artifact owned by the root session. |
| `session-store/<root-id>/spool/**` | Migrated shape-compatible legacy auxiliary subtree. Migration preserves every observed shape-compatible regular-file descendant below a resumable root, not only native event-ID names. A malformed regular-file collision at the `spool` anchor remains backup-only and does not block native directory creation. |
| `session-store/<root-id>/artifacts/**` | Migrated shape-compatible legacy auxiliary subtree. Migration preserves every observed shape-compatible regular-file descendant below a resumable root, not only native fetched-document names. Malformed regular-file collisions at the `artifacts` or `artifacts/fetched` anchors remain backup-only and do not block native directory creation. |
| `session-store/.timeline-locks/` | Central inter-process timeline-lock directory. |
| `session-store/.timeline-locks/<sha256>.lock` | Central inter-process lock for one normalized timeline path; the digest uses a domain separator and length-framed path components. |
| `session-store/.norn-publication-<uuid>.json` | Durable crash-recovery journal for atomic timeline/index publication. |
| `session-store/.norn-publication-journal-<uuid>.tmp` | Pre-publication journal temporary file; an exact UUID artifact without its matching durable journal is reclaimed under `index.lock`. |
| `session-store/.norn-publication-timeline-<uuid>.stage` | Staged timeline paired with a publication journal; an exact UUID artifact without its matching durable journal is reclaimed under `index.lock`. |
| `session-store/.session-deletion.<uuid>.json` | Durable index-first deletion journal; removed after descriptor-bounded sequential registered-timeline cleanup and optional whole-root artifact-tree removal. |
| `session-store/.session-deletion.<uuid>.json.tmp` | Pre-publication deletion-journal temporary file. |

Deleting a child removes its timeline and the timelines/index rows of every
transitive descendant. Spool and fetched files are root-owned rather than
child-owned, so they remain until the root session is deleted. Deleting the root
removes its complete `<root-id>/` root-owned capability tree after all subtree
rows are atomically removed from the index.

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

The retained gate artifact mechanically records the complete NUL-delimited
`git diff --name-only --diff-filter=ACDMRTUXB 2c0350d..HEAD --` inventory rather
than relying on a manually maintained shortlist:

- 164 paths in the complete corrected D2 range;
- raw inventory length 7,654 bytes;
- raw inventory SHA-256
  `68462c3e7d5359f49a819becf4a44ccf1e8c84044292ab09fa497e708a2556dc`;
- exact ordered path list in
  `docs/reviews/evidence/d2/2026-07-17-d2-f1-correction-gate-e9755fe.json` at
  `repository.base_diff_name_inventory.paths`; and
- identical correction commit, tree, inventory, and clean status at the initial
  and final pre-output snapshots.

The companion policy artifact inventories all 145 changed Rust files, including
33 test-only files. It reports no production file at or above 500 lines, no
thin-entrypoint or module-shape violation, and no added bypass/debt match.

## 4. Contract-to-evidence matrix

Each checked cell below means the source suites containing the described
fixtures executed successfully in the retained exact-correction gate. The
stage-root `fsync` mechanism is instead established by exact source inspection;
the surrounding checkpoint recovery behavior is exercised by the retained
distributions. A checked cell does not mean an independent reviewer has accepted
the contract or the sufficiency of those fixtures.

| Contract | Required evidence | Status |
|---|---|---|
| Strict format-2 only | Exact header, unknown-field/event, duplicate-key/id, non-canonical row, legacy/newer version, malformed row, and torn-tail fixtures | [x] Retained candidate gate |
| Versionless standard namespace | CLI and public library constructor fixtures, including relative/unavailable home failure | [x] Retained candidate gate |
| No normal legacy content read | Unreadable/renamed legacy-content fixture proving bounded startup still succeeds after valid cutover | [x] Retained candidate gate |
| Atomic offline publication | Exact source ordering for the populated backup-stage root fsync; enumerated crash seams before/after backup and store publication; no foreign destination replacement | [x] Source inspection plus correction gate and six abrupt-process distributions |
| Idempotence and interruption recovery | Repeated same-source result plus owned-stage recovery and changed-source behavior | [x] Gate plus six abrupt-process distributions |
| Immutable legacy and backup | Before/after source tree digest, backup digest, exact export bytes, and old-binary divergence detection | [x] Retained candidate gate |
| Three fidelity classes | Canonical, flattened coherent, malformed/ambiguous, spoofed boundary, stale index, orphan, and duplicate fixtures | [x] Retained candidate gate |
| Fresh provider epoch | Canonical migrated resume, explicit degraded approval, inspect-only refusal, and concurrent one-boundary distribution | [x] Gate plus 20/20 boundary distribution |
| Timeline/index atomicity | Publication/deletion-journal crash recovery, first-publication recovery, same-name convergence, reader/writer exclusion, transitive delete/writer exclusion, and exact residue checks | [x] Gate plus exact concurrency distributions |
| Generation/ABA isolation | Stale manager rename/reconcile, registered read/repair, sink construction/append, binding, child publication, spool, and fetched-artifact handles after delete/recreate; deterministic index-before-timeline contention | [x] Gate plus exact generation distributions |
| Exact usage/count accounting | Overflow, mismatch, append ambiguity, and recovery fixtures | [x] Retained candidate gate |

The primary fixture ownership is mechanical and reviewable: strict codec and
relationship cases live under `session/persistence/strict/*_tests.rs` and
`index_strict_tests.rs`; standard-path guards live in
`config/paths_session_tests.rs` and the CLI `config/paths.rs` tests; migration,
classification, backup, idempotence, and interruption cases live in
`session/migration/tests.rs`, `hardening_tests.rs`, and `tests/recovery.rs`;
fresh-epoch cases live in `session/provider_epoch_tests.rs` and manager resume
tests; publication, deletion, concurrency, and generation cases live in
`publication_tests.rs`, `deletion_runtime_tests.rs`,
`timeline_concurrency_tests.rs`, `timeline_runtime_tests.rs`, manager tests,
`artifacts_tests.rs`, and colocated spool/branch tests; exact-counter cases live
in `counter_overflow_tests.rs`, `index_strict_tests.rs`, and the timeline suites.

## 5. Retained gate and distributions

| Gate | Command or evidence generator | Result |
|---|---|---|
| Formatting | `cargo +1.94.0 --locked fmt --all -- --check` | [x] Pass |
| Strict lint | `cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings` | [x] Pass |
| Workspace tests | `cargo +1.94.0 --locked test --workspace --all-targets` | [x] 5,281/5,281 |
| Doctests | `cargo +1.94.0 --locked test --workspace --doc` | [x] 8/8 |
| Focused strict codec/store | `cargo +1.94.0 --locked test -p norn session::persistence` | [x] 165/165 |
| Focused migration | `cargo +1.94.0 --locked test -p norn session::migration` | [x] 29/29 |
| Focused manager/resume | `cargo +1.94.0 --locked test -p norn session::manager` | [x] 37/37 |
| CLI session surface | `cargo +1.94.0 --locked test -p norn-cli session` | [x] 51/51 |
| Standard-path surface | `cargo +1.94.0 --locked test -p norn config::paths` and `cargo +1.94.0 --locked test -p norn-cli config::paths` | [x] 14/14 and 11/11 |
| Policy diff | Added `unwrap`/`expect`/`panic`, lint bypass, ignored test, and debt-marker scan against the frozen base | [x] Zero matches in every category |
| Production LOC | AST-based production/test classification plus tokei counting for every touched Rust file; strict `<500` result required | [x] 145 files, 33 test-only, zero violations |

The retained runner executed 14 exact process-isolated tests 20 times each:

- eight concurrency/generation cases covering same-session publication,
  registered-sink counter reconciliation, exact-batch retry convergence,
  reader/tail exclusion, delete/writer exclusion, migrated provider-epoch
  convergence, generation retention while lock-waiting, and stale-reader ABA
  repair; and
- six abrupt-process migration cases, one for each exact checkpoint:
  `backup_prepared`, `backup_published`, `backup_durable`,
  `strict_store_prepared`, `strict_store_published`, and
  `strict_store_durable`.

Result: **280/280**, with exactly one named recovery sentinel in every recovery
observation and no recovery sentinel in concurrency observations. The retained
JSON records every exact test name, command, iteration, exit status, parsed test
count, expected/observed sentinel, duration, and stdout/stderr hash.

**Retained correction full-gate and distribution artifact:**
`docs/reviews/evidence/d2/2026-07-17-d2-f1-correction-gate-e9755fe.json`,
SHA-256
`8cb4caf0b5724bf49de4fe53e9b7a4c4296751d2fb09a73fdb1e809aacd9e077`.

**Retained correction policy artifact:**
`docs/reviews/evidence/d2/2026-07-17-d2-f1-correction-policy-e9755fe.json`,
SHA-256
`c8565b5cf75f7ae88ae94b862ab6e0a6572e988eb333b7c74d27191e9cc6a024`.

The gate ran from a detached checkout whose source was clean at both recorded
snapshots. Its logical Cargo target was `target/shared`, resolving to the main
repository's normal `target/`; it did not create a temporary or duplicate build
tree. The implementer reports that loopback-only test servers required execution
outside the managed network sandbox after an in-sandbox run was invalidated by
`EPERM`; no external network was used. The clean end-state snapshot is taken
immediately before immutable output publication, so the new gate JSON is
intentionally the only post-snapshot file.

## 6. Required review

- [x] A session-persistence reviewer enumerates every durable write and
  publication seam, independently reruns the distributions, and verifies the
  namespace inventory. The review returned `READY` with F1 MINOR.
- [x] A Responses-protocol reviewer checks that migration classification and
  fresh-epoch behavior never manufacture canonical provider semantics.
- [x] An embedder/API reviewer verifies `SessionManager::standard()` is the
  documented standard front door and custom stores are not represented as
  standard cutover-safe storage.
- [x] A Fable adversarial reviewer returns `READY` for the frozen D2 range.

All four seats are recorded in the
[`Gate D review`](2026-07-17-d2-gate-d-review.md). The same coordinator closed
F1 in the
[`correction review`](2026-07-17-d2-f1-correction-review.md), satisfying the
contingency and making corrected D2 Gate D unconditionally `READY`.

## 7. Open before P3/P4 acceptance

- [x] Freeze and commit the complete D2 source range.
- [x] Fill every evidence placeholder above from that exact commit.
- [x] Resolve every finding from the independent D2 review. F1 is corrected at
  `e9755fe` and independently closed at `26b4e28`.
- [ ] Add response-scoped audio without inventing a terminal output item.
- [ ] Complete the exhaustive all-discriminator/optional-shape lifecycle matrix.
- [ ] Run and retain final P3/P4 Gate C evidence.
- [ ] Obtain independent P3/P4 acceptance.

The eight non-blocking observations from the Gate D review carry into P3/P4.
Most materially, registered appends currently hold the global `index.lock`
across full-timeline validation, writes, and fsyncs, making repeated append work
quadratic over a session's lifetime and serializing a store. This is a disclosed
scale concern, not a D2 correctness finding.
