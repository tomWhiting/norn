# P0 status report and implementer feedback — 2026-07-13

**Reviewer:** Claude (Fable), external Gate D reviewer (same seat as the Gate D, openat, fetch-artifact, and D1C/D1E reviews).

**Purpose:** owner-requested desk review of everything landed since the D1C/D1E review head, plus a whole-phase status picture and structured feedback on the implementer agent. This is **not** a whole-phase Gate D verdict and it is **not** the independent review the two new candidates (D1E weighted admission, D1D MCP startup) are waiting on — those are larger, separately-scoped reviews and are called out below.

**Range examined:** `ca43c1b..373c5b0` (11 commits): D1E weighted admission (`485b6b0`, `a9db38e`, `6aa7185`, candidate doc `aa6a653`), review corrections (`89c3b6b`, `d488c1a`, record `5015e79`), D1D MCP startup (`8c4119c`, `8963538`, `a949af1`, candidate doc `373c5b0`).

**Gate evidence freshness:** all commands ran in an isolated worktree at `373c5b0` with a freshly wiped target directory (fully cold build — fresh by construction). Exit codes captured directly to a log directory, never through a pipe. Toolchain `rustc 1.96.0-nightly (80282b130 2026-03-06)`.

---

## 1. Independent gate results at head `373c5b0`

| Gate | Result | Fingerprint |
|---|---|---|
| `cargo fmt --all --check` | **Pass** (exit 0) | — |
| `cargo clippy --workspace --all-targets -- -D warnings` | **Pass** (exit 0) | cold, 103s, all workspace crates re-analyzed |
| `cargo test --workspace --all-targets` | **3,694 passed, 1 FAILED** (exit 101) | 13 suite results; failure deterministic 3/3 → G-1 |
| `run_p0_review_corrections.sh 20` (my own rerun) | **Pass** (exit 0) | both cases 20/20: configured lock-deadline reporting, repaired-anchor full replay |

As with the previous round, the failing test does **not** contradict any claim in the range: every candidate record scoped its verification to named `--lib` suites and explicitly declined to claim the workspace gate. But it is the second consecutive round to land with a deterministically red integration test at head — see G-1 and the feedback section.

## 2. Review-corrections round (`aa6a653..d488c1a`): **ACCEPTED**

All three actionable findings from my D1C/D1E review are closed in the recommended shape or better. Verified in code and by independent rerun, not from the record alone.

- **F-1 (lock deadline reporting) — fixed exactly as recommended.** `lock_index` now threads the pair `(poll_budget, reported_wait)` into `lock_with_deadline`: the residual bounds the cross-process poll, and every typed `IndexLockTimeout.waited` arm reports the caller's configured duration. The previously failing test passes 20/20 in their runner and in my independent rerun. The lock path also now admits through the new `DescriptorGovernor` (a `DescriptorPermit` held for the lock's lifetime) — consistent with the weighted-admission candidate.
- **F-2 (dead newline state) — closed by deletion.** The `needs_newline` field is gone from `JsonlSink`; the tear-state helper survives only under `#[cfg(test)]`, where its independently pinned contract still has a purpose. Verified by grep: no production references remain.
- **F-3 (first resume 400 after hard kill) — root-caused and fixed structurally.** My server-thread hypothesis was confirmed at request-shaping level: open-time repair appends the synthetic tool result *locally*, but the provider-side thread named by `previous_response_id` cannot contain it, so sending it as a delta produced the 400. The fix treats the synthetic interruption result as a durable response-thread boundary — anchor discovery encountering it after an assistant response discards the stale anchor, and the first resumed request full-replays the healed transcript (`conversation_state.rs::latest_response_anchor`). The regression test constructs the exact killed-mid-tool persisted shape, runs the *real* repair, and asserts no provider anchor plus a complete replay; 20/20 in both their runner and mine. Identification is by sentinel string on the result's `error` field, and a false match is fail-safe: it can only force a full replay, never corrupt threading. The record honestly discloses this is request-construction proof, not a live-SIGKILL e2e — that residual gap belongs to the D3 threading decision, as my review said.
- **F-4 (reopen cost)** — no change, correctly: the trade was accepted, and the record says any future performance work must live behind the same identity invariants.

## 3. New finding

### G-1 — Stale test at head: `empty_extension_uri_is_argument_error` fails deterministically (3/3)
`crates/norn-cli/tests/assembly_flag_wiring.rs:386`. The D1D round moved empty/whitespace `--extension` URI validation from `builder_from_cli` up into `resolve_invocation` (`runtime/resolve.rs:101`, via `collect_extension_servers` → `collect_extension_uris`, which still hard-errors on empty URIs — `config/extensions.rs:32`). **Both production drivers still validate** — TUI at `tui/driver.rs:93` and print at `print/orchestrator.rs:293` run `resolve_invocation` before `builder_from_cli` — so this is a stale test pinning the old layer, not a production regression. The test's own doc comment names the old site ("the `builder_from_cli` argument error"), which is exactly the breadcrumb a post-move grep would have caught. **Fix:** re-pin the test at the layer that now owns validation (or drive it through `resolve_invocation`), keeping the empty-URI-is-a-hard-error contract covered on a real path. Small, but it must be green before the final Gate C.

## 4. The two candidates awaiting independent review (preliminary read only)

These landed with honest "candidate, review pending" status and disjoint, well-labelled ranges. I have *not* reviewed them to acceptance; what follows is a desk read to size the work and surface early flags.

**D1E weighted admission (`ca43c1b..6aa7185` + corrections, 65 changed files).** The core (`resource/descriptor_governor.rs`, 250 lines) reads well: one process-wide success-only lazy authority; source-derived weights (pipe ends, exec-status pipe, traversal handles — counted, not invented); fail-fast production admission with the only waiting API under `cfg(test)`; live revalidation after reservation that deliberately double-counts in the safe direction; typed errors carrying snapshot + `norn doctor` guidance. Upstream Chiron is pinned by full revision with its own 20/20 lease evidence. Early flags for the real review: **(a)** `TRANSIENT_HEADROOM = 8` needs a factual or owner-ruled source under the no-invented-numbers law — the candidate surfaces it as review question 3 rather than hiding it, but the answer is an owner call (it is the boundary excluding one-shot filesystem syscalls from the authority); **(b)** the claimed permit-lifetime pairings across every cancellation edge (review question 2) are the highest-risk surface and need adversarial verification, not desk reading; **(c)** the Chiron `libyggd` unpinned-`syntax` residual is correctly disclaimed but should be tracked somewhere it can't be forgotten.

**D1D MCP startup (`5015e79..a949af1`, 41 changed files).** Direction is recorded as decided 2026-07-13 in `DECISIONS-2026-07.md` §10 (retain and complete `mcp_servers`; precedence `session > CLI > local > project > user`; remembered approval for checked-in project definitions only). The candidate's scope discipline is good: startup consumption only, a long explicit not-claimed list (live mutation, reconnect, OAuth, bounded bodies…), fixtures that prove a pending-approval project server does not execute, and disclosure of an observed unrelated flake (`model_output_is_incremental_and_unknown_id_is_none`, isolated 20/20 after) presented as observation, not stability proof. Early flags for the real review: **(a)** protocol conformance claims (negotiation, pagination with repeated-cursor rejection, stdio cancellation invalidating the channel) need hostile-server fixtures verified, not just enumerated; **(b)** the candidate itself says root/spawn selection end-to-end needs a stronger fixture; **(c)** the "MCP is not a filesystem sandbox" product boundary should carry explicit owner sign-off since it is a security-posture statement users will rely on.

## 5. Whole-phase P0 scoreboard

**Closed and externally accepted:** F1 openat race (single-retry guard); D1B fetch→session artifacts; D1C NOFILE to the ruling; D1E idle-retention slice; review corrections F-1/F-2/F-3 (this report, §2); all Gate D §6 hygiene items (zombie helpers, sentinels, LOC method, TUI-history family, traceability, inventory).

**Open before whole-phase READY:**
1. **G-1** stale extension test (small, this report).
2. **Independent review: D1E weighted admission** — including the owner ruling on the one-shot-filesystem exclusion and the `TRANSIENT_HEADROOM` source.
3. **Independent review: D1D MCP startup.**
4. **Owner dispositions:** Gate A P0-only retrospective exception; Gate B baseline-evidence disposition (the audit's position stands: the red-state chronology cannot be reconstructed without inventing evidence).
5. **Final Gate C:** full workspace battery with distributions at the final head, then a fresh whole-phase Gate D verdict.
6. Held separately by prior rulings: `RLIMIT_CORE=0`; MCP live-mutation slice; D2–D10 in their own phases.

## 6. Implementer-agent feedback

The Gate D review identified this agent's failure mode precisely: **evidence overclaim at the finish line** — unqualified "Pass" over a coin-flip test, "complete coverage" without an enumeration. Three corrective rounds later, that failure mode is gone, and the correction has held under pressure:

- **Claim discipline is now the agent's strongest trait.** Every record in this range states its exact verification scope, declines the workspace gate explicitly, ships enumerations with its "every X" claims, discloses observed nondeterminism unprompted, and ends with review questions inviting refutation. The D1D candidate disclosing a flake it *survived* — and labelling the 20/20 isolated rerun "not presented as a stability proof" — is the overclaim lesson fully internalized.
- **Fixes land structural, not cosmetic.** F-3 could have been papered with a retry; instead the agent reproduced the mechanism at request-shaping level and made the synthetic result a thread boundary. F-1 threads the semantic pair instead of patching the assertion. F-2 deletes rather than documents-around.
- **It finds its own work.** The TUI-history artifact family, the upstream Chiron lease redesign, and the corrections runner all originated implementer-side, not from review findings.

**The one habit still costing it: the focused-gate blind spot, now a two-round pattern.** Both this round (G-1) and the last (F-1's test) landed with a deterministically red norn-cli *integration* test at head that lib-only focused verification structurally cannot see. The scoping language is honest, so no claim was false — but "honest about not running the gate" is not the same as "the gate would have passed," and twice in a row the deferred gate caught a real defect. Two mechanical countermeasures:
1. **Per-round rule:** every crate whose source is touched gets its integration suites run (`cargo test -p <crate> --tests`), not just `--lib`. The cost at norn's scale is minutes.
2. **Post-move rule:** when a behavior moves between layers, grep for tests (and doc comments) naming the old layer before packaging — `assembly_flag_wiring.rs` names `builder_from_cli` in the very doc comment above the test that broke.

**Minor:** decision provenance should be legible at the point of use — the D1D candidate cites §10 but neither doc states in whose voice the ruling was made; under this house's evidence rules an owner ruling should be attributable in one hop.

Net assessment: the agent is now producing review-grade work packaged for adversarial verification, and the correction trajectory across four rounds is exactly what the coding standards demand. With the integration-suite rule adopted, the remaining risk profile is ordinary.

## 7. Recommended sequence from here

1. Implementer: fix G-1 (small), adopt the per-round integration-suite rule.
2. Owner: confirm the §10 D1D ruling is his; rule on `TRANSIENT_HEADROOM`/one-shot-exclusion (D1E review question 3); dispose Gate A exception and Gate B baseline evidence.
3. Independent reviews of the D1E weighted-admission and D1D startup candidates — sized for adversarial panels (permit-lifetime pairing and MCP protocol conformance respectively are the surfaces desk-reading cannot verify).
4. Final Gate C with distributions at the final head → whole-phase Gate D verdict.
