# P0 Gate C external-review handoff

> **Superseded current-head notice (2026-07-14):** This corrected handoff is
> retained as the historical `f788823` record reviewed by the earlier Gate D
> rounds. The current clean-head evidence and review request are in
> [`2026-07-14-p0-final-candidate.md`](2026-07-14-p0-final-candidate.md). Its
> `bfa0b8e` totals and residuals replace the mutable status claims below.

**Original date:** 2026-07-11
**Corrected:** 2026-07-14
**Phase:** P0 - credential and workspace authority containment
**Phase base:** `41ea210d24ec0653480be3a097b15adcb1e4bfb0`
**Corrective code head:** `f7888235dcae32fa66416c2380c7f376608cafff`
**Phase status:** not accepted; independent review and owner dispositions remain open

## Evidence status

This document replaces the invalidated 2026-07-11 Gate C packaging. The
original workspace test invocation did exit successfully, but Gate D later
reproduced its P0-introduced concurrency test failing 6/10 times. That result is
historical evidence of one observation, not a pass claim.

The current machine candidate is the complete P0 code range
`41ea210...f788823`. The latest independently reviewable correction range is
`8c66c12...f788823`. The integrated correction record is
[`2026-07-14-p0-d1d-d1e-correction-candidate.md`](2026-07-14-p0-d1d-d1e-correction-candidate.md).

Machine Gate C is green at the corrective code head. This is implementer
evidence, not independent acceptance and not a whole-phase `READY` verdict.

## Correction commits

| Commit | Review unit |
|---|---|
| `b0da011` | MCP registry/view separation, child dispatch, hostile protocol fixtures, and redaction |
| `2685216` | Descriptor lifetime, process-adoption cancellation, and exact weighted admission |
| `e4c3f43` | One-shot filesystem/read admission and task-store transaction governance |
| `f788823` | Deterministic PTY ownership and during-stream resize fixture |

Earlier P0 logical commits and their scoped reviews remain visible in Git and
the remediation plan. No earlier scoped `READY` is treated as a verdict on this
integrated range.

## Gate C commands

The following commands completed successfully:

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | Pass. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass with no warning or allowance. |
| `cargo check --workspace --all-targets` | Pass. |
| `cargo test -p norn --tests` | Pass, including library and integration targets. |
| `cargo test -p norn-cli --tests` | Pass, including the re-pinned extension and index-lock integrations. |
| `cargo test -p norn-tui --tests` | Pass, including all 17 PTY cases. |
| `cargo test --workspace --all-targets` | Pass after the PTY harness correction described below. |
| `cargo test --workspace --doc` | Pass. |
| `cargo test -p norn --doc --features test-utils` | Pass, including four compile-fail authority contracts. |
| `git diff --check 41ea210...HEAD` | Pass at the packaged documentation head. |

### Failed-first-run disclosure

The first final `cargo test --workspace --all-targets` attempt failed in the PTY
integration file with macOS `openpty` `ENXIO`. An unchanged 20-run file
distribution then produced 19 passes and one resize-marker failure. The harness
was running process-global PTY fixtures concurrently, and the resize scenario
raced an instantaneous stream between generating and idle panel geometries.

The correction serializes parent PTY ownership in the test process and uses a
delayed stream to assert the during-stream geometry. It adds no retry, ignore,
lint bypass, or production behavior. The retained post-fix result is 20/20
complete PTY suites, 340/340 individual cases, followed by the green complete
workspace run above.

## Repeated evidence

| Artifact | Distribution |
|---|---:|
| [`2026-07-14-p0-concurrency-final.json`](evidence/2026-07-14-p0-concurrency-final.json) | three macOS-sensitive cases, each 50/50 |
| [`2026-07-14-d1e-integrated.json`](evidence/2026-07-14-d1e-integrated.json) | six descriptor/cancellation/task cases, each 20/20 |
| [`2026-07-14-mcp-startup-integrated.json`](evidence/2026-07-14-mcp-startup-integrated.json) | four MCP startup/protocol/cancellation cases, each 20/20 |
| [`2026-07-14-p0-review-corrections-final.json`](evidence/2026-07-14-p0-review-corrections-final.json) | two prior Gate D corrections, each 20/20 |
| [`2026-07-14-p0-pty-final.json`](evidence/2026-07-14-p0-pty-final.json) | complete PTY suite, 20/20 |

The concurrency JSON records the exact head and worktree status at execution.
The shell-runner JSON files retain every aggregate denominator; the checked-in
runners reproduce the distributions and print failure logs rather than only the
last result.

## Policy and artifact inventory

[`2026-07-14-p0-final-policy.json`](evidence/2026-07-14-p0-final-policy.json)
was generated over `41ea210...f788823` and records:

- 227 changed Rust files, including 42 test-only files.
- Zero production files over 500 syntax-aware LOC and zero thin entrypoint
  violations. The maximum is `agent/builder.rs` at 498 production LOC.
- Zero added unwrap, expect, panic, suppression, ignore, empty-cfg,
  command-line allowance, unresolved comment marker, todo, or unimplemented
  matches.
- 92 production/build-script artifact-writer candidates.

The exact 92 rows are classified in
[`2026-07-12-p0-artifact-writer-inventory.md`](2026-07-12-p0-artifact-writer-inventory.md).
The increase from the prior 88-row snapshot is reconciled by file/text set
comparison: every new row belongs to session persistence/artifacts, process
spool, private line log, task storage, or explicit CLI step output. The reviewer
must reproduce that reconciliation; this handoff does not ask them to trust the
summary.

The marker audit detects debt markers in Rust comments. `todo!` and
`unimplemented!` are separate prohibited rules. A TOML test fixture that defines
a policy for rejecting the literal word `TODO` is therefore not falsely counted
as unresolved debt.

## Traceability and open process evidence

The finding-to-candidate matrix is
[`2026-07-12-p0-traceability.md`](2026-07-12-p0-traceability.md). Its `SEC-14`
row now includes MCP provenance, approval, hostile protocol, complete-pool/view,
and real child-dispatch fixtures.

The baseline audit proves that Git does not contain an executable red-state
chronology for most original P0 defects. That history cannot be reconstructed
honestly. Two owner dispositions therefore remain open:

- A P0-only retrospective Gate A timing exception.
- A Gate B disposition accepting source-review proof where no retained
  baseline execution exists.

Neither exception changes the unchecked historical facts or creates precedent
for P1 and later phases.

## Product work still open

- D1D startup and D1E structural admission require independent adversarial
  acceptance over the current range.
- MCP live list/inspect/add/remove/enable/disable/reload, child-only connection
  beyond the startup pool, dynamic roots, and request-boundary provider-tool
  refresh remain a separate unimplemented slice.
- OAuth lifecycle correctness remains assigned to P2.
- The session-centric outputs/transcripts/subagent layout remains reserved for
  the explicit pre-P3 storage and transcript design discussion.

## Requested reviewer action

1. Review `8c66c12...f788823` as four logical units, then inspect its integration
   with the full `41ea210...f788823` P0 range.
2. Rerun every Gate C command and checked-in distribution runner rather than
   trusting the tables.
3. Reconcile all 92 artifact-writer candidates and the descriptor permit
   lifetimes across success, failure, timeout, cancellation, adoption, transport
   drop, and shutdown.
4. Adversarially verify MCP project approval, protocol rejection, registry/view
   separation, optional-server failure, and real child dispatch.
5. Return scoped `READY` or `NOT READY` for the D1D startup and D1E correction
   slices. Do not call whole P0 accepted while the owner exceptions and required
   live MCP product slice remain open.

P1 does not begin until the plan's P0 acceptance gate is satisfied.
