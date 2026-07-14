# Review of the 2026-07-14 P0 round — G-1, D1D/D1E corrections, live MCP, final candidate

**Reviewer:** Claude (Fable), external Gate D reviewer (same seat as all prior P0 reviews).

**Range reviewed:** `51b83ea..046b0f9` (33 commits; tested code head `bfa0b8e`, later commits documentation-only). Sub-rounds: G-1 correction (`880891f`), external PRs #13/#14/#15 (libyggd relock, toolchain), D1D/D1E integrated corrections (`b0da011..f788823`), live MCP slice (`23cf40c..fa1fc6f`), stable-toolchain reconciliation (`d391f0c`), final disclosure/OAuth/evidence round (`03d1317..046b0f9`).

**Gate evidence freshness:** isolated worktrees, cold target dirs, exit codes captured directly to log files. Attestation reproduced from a second clean worktree at the exact tested head.

**Verdict: ACCEPTED for everything this review scopes — with the explicit boundary that the three big candidates (D1E weighted admission, D1D startup, live MCP) have been desk-verified and spot-checked, not adversarially reproduced.** The final candidate record itself demands a fresh whole-phase Gate D with full reproduction (writer inventory, permit lifetimes, secret inspection across 333 files); that remains the right closing step and is not discharged here.

## 1. Independent gate results

| Gate | Result | Fingerprint |
|---|---|---|
| `cargo fmt --all --check` @ `046b0f9` | **Pass** (exit 0) | — |
| `cargo clippy --workspace --all-targets -- -D warnings`, rustc 1.94.0 | **Pass** (exit 0) | cold, 121s, all crates |
| Same command, rustc 1.92.0 | Fails **only** on the documented `large_stack_arrays` false positive against the compiler-generated lib-test harness — exactly as the reconciliation record predicted, at the synthetic span, no source fix possible | cold, 100s |
| `cargo test --workspace --all-targets` @ `046b0f9`, rustc 1.92.0 | **4,624 passed, 0 failed** (exit 0) | 24 suites — **first fully green workspace battery of the campaign** |
| `attest_p0_evidence.py` from a clean worktree at `bfa0b8e` (my run) | **passed: true, zero errors** — matches their retained attestation | attester correctly *refused* my first attempt from a mismatched dirty head |
| Evidence chain | All six SHA-256 hashes in the final-candidate table match the retained files exactly | — |

## 2. Prior findings — all closed as recommended

- **G-1**: test re-pinned to `resolve_invocation` (the layer that owns validation), positive case continues through builder assembly; production diff empty. The per-round `cargo test -p <crate> --tests` integration fence is now a written process rule — and the D1D/D1E candidate's machine Gate C shows it being run.
- **TRANSIENT_HEADROOM (my flag for owner ruling): dissolved, not deferred.** The invented 8 is gone; the governor now reserves exactly 1 descriptor for the observer (`DESCRIPTOR_OBSERVER_RESERVE`, source-derived), and every family — including the previously excluded one-shot filesystem/read/task paths (`e4c3f43`) — carries typed weighted admission. Review question 3 from the admission candidate is thereby answered in code: nothing is excluded anymore.
- **D1D §10 attribution**: now explicitly recorded as Tom's ruling.
- **F-3 residual** (live-SIGKILL fixture): the correction-runner cases remain 20/20 in the retained final distributions.

## 3. What this round added (desk-verified)

- **Live MCP slice**: generation/runtime pairs published atomically with request-boundary leases; watcher lifecycle bounded; 7 evidence cases 20/20. Its five self-posed review questions are the right adversarial surfaces and remain open for the Gate D reproduction.
- **Evidence machinery** (`82e44f4`): exact-case runner + attester that binds gate/distribution/policy artifacts to a clean head and regenerates policy semantically. Honestly labelled "mechanical validation, not cryptographic provenance."
- **Two failed-first-run disclosures** (browser-sentinel race, PTY ownership race) fixed as test-determinism corrections with focused 20/20 distributions before the full gates — no retries, no weakened assertions. The failed gate artifacts are retained alongside the passing ones, hashes and all.
- **Final distributions**: 750/750 observations across 33 cases, serial to avoid the harness manufacturing descriptor pressure.
- **External PRs** (#13/#15) absorbed with a clean reconciliation record; the 1.92-vs-1.94 clippy divergence disclosed with the exact false-positive span.

## 4. Findings

### H-1 — Recommendation: pin the stable toolchain version
`rust-toolchain.toml` says `channel = "stable"`, which floats with the machine's rustup alias. The round's own record demonstrates the cost: this machine's alias resolves to 1.92.0, whose clippy fails the strict gate on a false positive that 1.94.0 (the version the final gate actually ran and proved) does not emit. My battery reproduced exactly that divergence. Pin `channel = "1.94.0"` — a factual value (the proven gate toolchain), not an invented one. Owner call if floating-stable was deliberate for contributor ergonomics.

### H-2 — Observation (no action): attestation trust boundary is correctly stated
The attester's self-description ("a fully fabricated self-consistent bundle remains indistinguishable without a trusted signed execution service") is accurate; my independent re-run from a clean head is the current mitigation and reproduced their verdict.

## 5. Whole-phase P0 state

**Everything the implementer can close is closed.** Remaining before READY:
1. **Owner dispositions** — Gate A retrospective timing exception; Gate B baseline-evidence gap (still: cannot be reconstructed without inventing history).
2. **H-1** toolchain pin (small; owner call).
3. **Fresh whole-phase Gate D** — full adversarial reproduction per the final candidate's reviewer-action list: rerun runner+attester (done here), manual secret inspection, 333-file LOC/bypass reproduction, 97-row writer inventory, permit-lifetime reconciliation, and the three candidates' self-posed adversarial questions. Sized for a panel, not a desk.

## 6. Implementer feedback (delta)

The integration-fence rule was adopted and immediately institutionalized. The finish-line failure mode remains dead: two first-run gate failures this round were disclosed with retained failed artifacts and fixed as determinism corrections. New strength: the round *dissolves* review flags structurally (headroom → exact observer reserve; one-shot exclusion → typed admission) instead of defending them. The evidence machinery — runner, attester, hash-bound chain, failed-run retention — now exceeds what this seat asked for. No new negative pattern observed.
