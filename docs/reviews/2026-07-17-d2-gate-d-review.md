# D2 strict session-store — independent Gate D review

- **Review date (Australia/Melbourne):** 2026-07-17
- **Reviewer:** external review seat (coordinator) + three independent
  read-only Opus seats (durable seams/namespace, migration protocol fidelity,
  embedder API/generation-ABA) + one Fable adversarial seat (publication and
  deletion journals, migration transaction, ABA, counters, locks)
- **Handoff:** `2026-07-17-d2-strict-session-store-handoff.md`
- **Review range:** `2c0350d..3ebc468`; candidate tree `b95ab9d4`; docs freeze
  `2e619e0`
- **Owner contract:** `DECISIONS-2026-07.md` § 15

## Verdict: READY, with one MINOR finding to resolve

The frozen D2 range is acceptance-quality for its claimed boundary. All four
seats returned SOUND/READY independently: no BLOCKER and no MAJOR defect
anywhere in the strict store, migration transaction, publication/deletion
journals, generation-ABA design, or exact accounting. One MINOR defect (F1,
backup-stage durability asymmetry, below) enters the handoff § 7
"resolve every finding" ledger and must be corrected before D2 closes; it
fails closed and does not endanger the strict store or the legacy source.

This is Gate D for the D2 range only — it is not P3/P4 acceptance, which still
requires response-scoped audio, the exhaustive lifecycle matrix, final P3/P4
Gate C, and independent P3/P4 review per the handoff.

## Evidence integrity (coordinator)

Every mechanical claim in the handoff reproduced exactly:

- Range/tree: `3ebc468^{tree}` = `b95ab9d411271769c7c0e6a305d0d4a21152b2b2`.
- Retained gate artifact SHA-256 `51267b19…`, policy artifact `309aca51…`,
  runner `run_d2_evidence.py` SHA-256 `756000cf…` — all match the handoff and
  the recorded `runner.sha256`.
- NUL-delimited diff inventory: 161 paths, 7,494 bytes, SHA-256 `3f52c180…`,
  byte-identical reproduction.
- The retained JSON's `repository.head`/`final_repository.head` both bind to
  `3ebc468` with a clean candidate state; `gate` 10/10, `distributions`
  280/280, toolchain `rustc 1.94.0`.
- The Fable seat verified commit `3ebc468` ("preserve linked target path")
  touches only the evidence runner's target-path recording — no migration or
  symlink-semantics change — and found no theatre in the evidence generator
  (sentinels are child-exit-86 + exactly-one-sentinel contracts that can fail,
  snapshots are before-and-after with separate candidate-dirt tracking).

## Battery (coordinator)

Native `1.94.0`, repo warm `target/`, alone on host.

| Leg | Result |
|---|---|
| `cargo +1.94.0 --locked fmt --all -- --check` | pass |
| `cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace --all-targets` | **5,283 passed, 0 failed** |
| `cargo test --workspace --doc` | 8/8 |
| Focused `session::persistence` / `migration` / `manager` | 165/165, 29/29, 37/37 |
| CLI session + paths surfaces | 51/51, 14/14, 11/11 |
| **Independent distribution rerun** (all 14 exact tests × 20) | **280/280, 0 failed** |

The +2 over the handoff's 5,281 is exactly the owner's unrelated uncommitted
`diagnostics_check/tests.rs` worktree edit (44 vs 42 tests in that file); the
retained gate ran from a clean detached checkout. Not a candidate discrepancy.

## Finding

**F1 — MINOR, mechanism CONFIRMED, manifestation PLAUSIBLE — the backup stage
root is never fsynced after `copy_tree` populates it.**
`session/migration/tree.rs:94-143` fsyncs `.session-backup-stage/sessions` and
every directory below it, but the stage root's own `sessions` dirent is only
made durable by filesystem journal ordering: `replace_owned_stage` syncs the
stage root *before* `sessions` exists (`stage_ownership.rs:86`), and after
publication `transaction.rs:238` syncs the destination *parent*
(`session-migration-backups`), not the renamed container inode. The strict
stage has the missing call (`stage.rs:43` ends with `sync_dir(stage)`), so this
is a parity gap, not a design gap. On a strictly-POSIX non-journaling
filesystem, a crash in the `BackupDurable`-adjacent window can leave a
published backup container durable with its marker but without the `sessions`
subtree. Consequence is fail-closed — the next open hits `open_verified_tree`
→ `NotFound` → typed `BackupConflict` — but a migration that reported success
has lost its immutable backup. On APFS/ext4-ordered the deep-file fsyncs mask
it. **Fix:** add `sync_dir` of the backup stage root after `copy_tree`,
matching the strict stage. One-line parity correction; must land before D2
closes per handoff § 7.

## What was proven (per seat)

**Durable seams + namespace (Opus) — SOUND.** Every write seam confirmed
temp+rename (or create-new+linkat) with file-and-parent fsync; no-replace
publication everywhere except the by-design mutable `index.jsonl` swap; journal
written and durable before the action it covers, with idempotent fail-closed
recovery; the six migration checkpoints bracket the real `publish_new_dir`/
`sync_dir` seams with production (non-test) recovery via idempotent re-run;
distinct typed `IndexCommitIndeterminate` vs `DeletionCleanupPending` outcomes;
consistent `session-migration.lock` → `index.lock` → timeline-lock order with
no ABBA and explicit poison handling; the timeline-lock digest is genuinely
domain-separated, length-framed, and count-framed. The § 2 namespace table was
mechanically verified **complete in both directions** (every code-produced
name has a row; every row has real code; the only production name not listed
is covered by the migrated-auxiliary rows).

**Migration protocol fidelity (Opus) — SOUND.** The taxonomy is exactly
Canonical/FreshEpochProjection/InspectOnly with no upgrade path and no
fabrication: `serde_ignored` rejects unknown fields, the strict writer
re-serializes decoded events in file order, stale index metadata is recomputed
and recorded rather than trusted. The legacy `sessions/` namespace is opened
read-only on every path. Exactly one fresh provider epoch per migrated
resumable session, serialized cross-process via `.provider-epoch-locks/`;
degraded resume requires the explicit `--allow-degraded-session` decision;
InspectOnly is triple-guarded against resume and exports byte-exact from the
hash-verified immutable backup. The epoch boundary clears the response anchor,
so the first post-migration request carries no `previous_response_id` — no
migrated id is presented as provider-issued continuity.

**Embedder API + generation ABA + accounting (Opus) — SOUND.** One shared
standard-store guard behind both `SessionManager::standard()` and the CLI (no
drift; every CLI standard path is guard-resolved). Generation is durable
UUID-v4, never minted on read; all seven authority-holding handle types
revalidate it, and timeline handles hold the index lock while acquiring the
timeline lock. Raw insertion, ID-only append, and arbitrary row-update APIs are
genuinely `#[cfg(test)]`-only, including their re-exports. Durable counters
funnel exclusively through checked arithmetic with typed overflow before any
byte or index state is accepted; all saturating arithmetic sits on
non-authoritative in-memory or display paths. Cleanup authority is exact
prefix+suffix+canonical-UUID round-trip matching; lookalikes raise typed
conflicts.

**Adversarial (Fable) — READY.** Attacks mounted and defeated in read code:
crash at every publication-journal step pair; stale/forged journal replay
(fail-stop on any disagreement, never adoption); concurrent publishers; path
traversal via journal content (shape-validated ids + descriptor-pinned
`O_NOFOLLOW` I/O); torn journals; recovery vs live writers (impossible — all
production writers hold the index lock); post-deletion recreate races (a
recreate cannot exist before deletion recovery completes, plus
`ensure_paths_are_unclaimed`); publication-vs-deletion journal interleaving;
migration digest TOCTOU (bounded, detectable, never absorbed); concurrent
migrations; backup immutability; symlinks inside the legacy source (whole-walk
rejection, `O_NOFOLLOW` everywhere); generation ABA including the
crash-recovery variant of child publication; counter-overflow acceptance
ordering; lock-digest collisions (length+count-framed SHA-256).

## Observations (non-blocking ledger for P3/P4)

1. **Index-lock occupancy and O(n²) append cost.** Registered appends hold the
   global `index.lock` across full-timeline validation scans, writes, and
   fsyncs; per-append rescans make append cost quadratic over a session's
   life. Correct (it is what makes the ABA design airtight) but material for
   P3/P4 scale and should be stated explicitly rather than implied.
2. **TornTail wedge:** a torn write that lands as complete JSON minus the
   trailing newline is a permanent typed `TornTail` with no repair verb.
   Conservative fail-stop; consider an explicit offline repair verb later.
3. **Lookalike posture inconsistency:** non-canonical `index.jsonl.tmp*` names
   fail-stop the store while publication/deletion lookalikes are ignored; both
   satisfy "never cleanup authority."
4. **Logged-only index-counter repair** on resume (`reconcile_index_entry`) —
   consistent with the counters-are-a-repairable-cache model since no
   acceptance path trusts index counters.
5. **Lock files accumulate** (timeline/epoch locks are retained forever,
   including for deleted timelines) — disclosed as "retained."
6. **Cutover guard is canonical-path, not type-enforced:** an embedder that
   hand-builds `norn_dir().join("session-store")` into `SessionManager::new`
   bypasses cutover verification; doc-guarded, subdir constant private, and
   `standard()` is the obvious front door.
7. Intermediate-directory dirents in stages rely on journal ordering rather
   than explicit ancestor fsync (same class as F1 but matching the handoff's
   stated file+parent discipline; F1 is the one place the discipline itself is
   asymmetric).
8. Canonical migrated sessions retain their genuine original provider item
   ids inside `response_items`; replay includes them as recorded input. Not
   manufactured continuity (the anchor is severed); noted for the P3 replay
   reviewer.

## Plan honesty

DECISIONS § 15 and the plan's D2 row record the decided contract, retained
Gate C, and the frozen range while explicitly withholding independent Gate D
and P3/P4 acceptance. The handoff's § 1 boundary matches what the code does;
its § 7 open items are genuinely open. No finish-line overclaim found.

## Standing

Gate D verdict for the frozen D2 range: **READY, contingent on landing the F1
parity fix** (and re-evidencing the touched seam). The observation ledger
carries into P3/P4. P3/P4 acceptance remains open per the handoff: audio
sidecar, exhaustive lifecycle matrix, final Gate C, independent review.
