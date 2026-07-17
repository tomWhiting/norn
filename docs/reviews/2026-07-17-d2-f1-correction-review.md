# D2 F1 correction — narrow confirmation

- **Review date (Australia/Melbourne):** 2026-07-17
- **Reviewer:** the same Gate D coordinator seat that returned the `59dc244`
  verdict (external review seat; coordinator-only per the handoff — no panel)
- **Corrects:** F1 of `2026-07-17-d2-gate-d-review.md`
- **Handoff:** `2026-07-17-d2-f1-correction-handoff.md`
- **Correction commit:** `e9755fe2533979410f53eda88966349437a54517`; tree
  `9663e625dc57c7e23e87e3caf4f11b3adee8e231`; docs reconciliation `9fda7d2`
- **Narrow diff:** `59dc244..e9755fe`

## Verdict: F1 CLOSED; prior contingency satisfied; corrected D2 Gate D READY

Every item in the handoff's narrow review request was verified directly:

- **Diff scope.** `git diff --stat 59dc244..e9755fe` touches exactly one
  production file, `crates/norn/src/session/migration/transaction.rs`, with
  seven inserted lines and nothing else. `9fda7d2` is docs/evidence-only. No
  other production source changed since the reviewed candidate.
- **Placement.** The new `root.sync_dir(&backup_stage)` sits immediately after
  `copy_tree` populates the stage and before `open_verified_tree` staged
  verification, `verify_owned_directory`, every `#[cfg(test)]` checkpoint, and
  both no-replace publications. This is precisely the parity call the Gate D
  review prescribed: the stage root's `sessions` dirent is now made durable by
  an explicit fsync instead of filesystem journal ordering, matching the strict
  stage's `sync_dir(stage)` discipline.
- **`sync_dir` semantics.** `PrivateRoot::sync_dir` →
  `sync_relative_directory` (`util/private_fs.rs:445-447`) opens the target
  directory descriptor-relative (`open_relative_directory`, `O_NOFOLLOW` walk)
  and calls `File::sync_all()` — a real fsync of the directory descriptor
  itself; non-Unix/unsupported targets fail closed as unsupported.
- **Typed error mapping.** The failure maps to
  `SessionMigrationError::mutation("synchronizing backup migration stage", …)`
  — same typed family as every adjacent seam, occurring before publication, so
  a failed sync aborts the transaction with the stage still owned and
  recoverable.
- **Evidence binding.** Correction gate artifact SHA-256 `8cb4caf0…`, policy
  artifact `c8565b5c…`, runner `756000cf…` (unchanged) — all reproduce. The
  gate JSON binds `repository.head`/`final_repository.head` to `e9755fe` with
  tree `9663e625…`, clean at both snapshots; gate 10/10, distributions
  280/280, workspace 5,281/5,281. The 164-path base diff inventory
  (7,654 NUL-delimited bytes, SHA-256 `68462c3e…`) reproduces byte-exact from
  `2c0350d..e9755fe`.

## Independent rerun (coordinator, native 1.94.0, repo warm target, alone on host)

| Leg | Result |
|---|---|
| `cargo +1.94.0 --locked fmt --all -- --check` | pass |
| `cargo +1.94.0 --locked clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo +1.94.0 --locked test -p norn session::migration` | 29/29 |
| `migration_recovers_after_backup_prepared` × 20 | 20/20 |
| `migration_recovers_after_backup_published` × 20 | 20/20 |
| `migration_recovers_after_backup_durable` × 20 | 20/20 |
| Policy on the changed file | zero unwrap/expect/panic/todo; 452 non-blank non-comment lines (<500) |

The handoff's honesty about test limits is accepted: an ordinary process test
cannot prove a power-loss fsync guarantee, and no synthetic sync-observation
hook was added just to watch the call — the evidence is the exact ordered
production call plus the retained abrupt-process recovery distribution, which
is the right trade under the no-theatre standard.

## Standing

F1 is closed and the `59dc244` contingency is satisfied: **the corrected D2
range `2c0350d..e9755fe` holds an unconditional Gate D READY.** The eight
non-blocking observations from the Gate D review remain the P3/P4 ledger
(explicitly including global `index.lock` occupancy and quadratic
full-timeline rescans under repeated appends). D2 Gate D is complete; P3/P4
acceptance still requires the response-scoped audio sidecar, the exhaustive
lifecycle matrix, final P3/P4 Gate C, and independent P3/P4 review.
