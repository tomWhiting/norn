# Review of the P0 openat corrective round — 2026-07-12

**Reviewer:** Claude (Fable), external Gate D reviewer (same reviewer as the 2026-07-11 Gate D review).

**Range reviewed:** `c0d32e5..ac7c8f3` (fix commit `c25e841`, reconciliation `cdaae83`, evidence `ac7c8f3`). The `.claude/skills/norn/` additions in this range are owner tooling, out of review scope.

**Verdict: the F1 corrective work is ACCEPTED.** The macOS `openat(O_CREAT)` fix, its regression tests, the retained evidence, and the documentation reconciliation all verified independently. Whole-phase P0 Gate D correctly remains **open** — the corrective checklist still has seven unlanded items (D1B fetch migration, D1C NOFILE, D1D, zombie helpers, LOC regen, sentinels, traceability records), and the implementer's own disposition says exactly that. No overclaim found anywhere in this round.

---

## 1. Independent verification (all performed by this reviewer on the affected platform)

| Claim | Source | Independent result |
|---|---|---|
| Baseline repro: 174/400 spurious ENOENT | `evidence/2026-07-12-openat-baseline.json` | JSON internally consistent; matches report text exactly |
| One retry: 400/400 success, 189 retries | `evidence/2026-07-12-openat-one-retry.json` | Matches; zero second-order ENOENT |
| O_EXCL control: 100 winners / 300 EEXIST / 0 ENOENT | `evidence/2026-07-12-openat-exclusive-control.json` | Matches |
| 50/50 × 3 regression distribution at `c25e841` | `evidence/2026-07-12-p0-concurrency-c25e841.json` | Recorded head and dirty-worktree disclosure verified; **independent rerun at `ac7c8f3`: 15/15 × 3** (the Gate D coin-flip test is now 65/65 combined post-fix) |
| `cargo clippy -p norn --all-targets -- -D warnings` pass | correction report §Targeted verification | **Pass** (exit 0) |
| `cargo fmt --all --check` pass | ibid. | **Pass** (exit 0) |
| `private_fs.rs` 412 / `private_fs_tests.rs` 418 code lines | ibid. | **Exact match** (tokei) |

**Contention probe beyond the retained evidence (new, this review):** the checked-in reproducer run at higher contention than anything previously tested —

| Threads | Attempts | Outcome with one retry | Retries fired | Second-order ENOENT |
|---:|---:|---|---:|---:|
| 16 | 4,800 | 4,800 success | 1,138 | 0 |
| 32 | 9,600 | 9,600 success | 984 | 0 |

Combined with the retained 4-thread evidence and the Gate D probe, that is **~3,400 observed retry events across 4–32 racing threads with zero consecutive ENOENTs**. The single-retry bound holds at every contention level tested.

## 2. On the single-retry bound (the one design divergence from the Gate D patch)

The Gate D report proposed a 16-attempt bound; the implementer landed **one** retry, arguing it is the largest bound the retained evidence supports. That is the correct application of the no-invented-numbers rule — 16 was a reviewer-invented safety margin — and the implementer's own evidence contains a *structural* argument that the correction report does not articulate but should:

- Every baseline failure recorded `target_exists: true` at observation time — the spurious ENOENT occurs only when **losing** a create race, and by the time the errno is observed the winner's directory entry is visible.
- The pre-existing-target control fired **zero** ENOENTs in 400 attempts — an existing entry never triggers the race.

Together: the retry always executes against an already-existing entry, which the controls show never fails. One retry is therefore structurally sufficient, not merely empirically unrefuted. The narrow theoretical residue — the entry being unlinked and a fresh create race starting *between* the failure and the retry — remains fail-loud (typed error, no corruption) if it ever occurs, and no unlink-recreate churn of that shape exists on the affected paths today.

**Recommendation (non-blocking):** add the two bullet points above to `2026-07-12-p0-openat-correction.md` so the bound's justification is structural + empirical, not empirical alone.

## 3. Fix coverage and code quality

- `open_file_at` is the **only** production file-open call site in the primitive; every create path (`open_lock`, `create_new`, write/append opens) routes through it. Coverage is complete for the proven failure surface.
- The retry helper is injectable and unit-tested without syscalls (`macos_create_retry_is_single_bounded_and_create_only` pins: one retry on create+ENOENT, termination on persistent ENOENT, no retry for non-create). The two new concurrency regressions pin the production topologies (independently opened roots racing one lock name; O_EXCL exactly-one-winner).
- macOS-only `cfg`: correct — the race is Darwin-specific per the controls; other platforms keep single-call behavior, and no cross-platform behavior was smuggled in.
- The Gate D doc/identifier drift is fixed honestly: `create_final` → `create_missing`, `PrivateRoot` docs now state that `create` makes missing ancestors. The *policy* question (whether a missing mount point should fail loudly instead) is correctly still open in the corrective checklist rather than silently closed by the relabel.
- **Watch item (not a finding):** the directory-traversal `openat` sites (reopen after `mkdirat`, `private_fs.rs:300–376`) are outside the retry guard. The race is create-specific and no directory-open failure has ever been observed; if one ever appears it fails loud. Noted so the surface is on the record.

## 4. Evidence-integrity assessment

This round is the direct test of the Gate D countermeasures, and they were adopted fully:

- Gate claims now come from **checked-in scripts** with complete denominators, errno distributions, per-run exit codes, commit identity, and dirty-worktree disclosure — not from single lucky runs.
- The invalidated Gate C handoff was **corrected, not appended around**: an invalidation banner, the "Pass" table cell rewritten as a historical single observation, and the tracker text re-pointed at the live plan.
- The remediation plan unchecked its own Gate C boxes and rewrote the P0 status header to lead with `NOT READY`.
- Only three corrective-checklist items are checked, each with retained evidence; everything unlanded is `[ ]`. The correction report's disposition ("complete pending independent re-review; integrated Gate C remains open") understates rather than overstates.
- DECISIONS §9 records the owner rulings (D1B fetched-content privacy, D1C NOFILE, distributional-evidence rule, D1D open) and contains one legitimate pushback the Gate D report missed: `RLIMIT_CORE=0` would also disable core dumps for user commands norn spawns, so it is held as a separate open decision rather than bundled into the NOFILE ruling. That is the right call.

## 5. Standing state after this round

**Closed by this round:** Gate D F1 (fix + regression + distribution evidence), the Gate C evidence correction, the `private_fs` doc drift, and the fetch-cache *ruling* (D1B decided; migration still to implement).

**Still open before whole-phase READY** (all honestly tracked in the plan's corrective checklist): fetch-write migration to private session artifacts (D1B implementation), D1C NOFILE implementation (`doctor`, typed EMFILE/ENFILE), D1D mcp_servers decision, zombie helper deletion (`session_file_path`/`resolved_session_file_path`), single-method LOC regeneration, 429/401 and loop-level sentinels, traceability records, Gate A exception decision, and the full Gate C rerun over the finished corrective candidate.
