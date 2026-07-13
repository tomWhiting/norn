# P0 D1D/D1E integrated correction candidate

**Date:** 2026-07-14
**Phase base:** `41ea210`
**Correction base:** `8c66c12`
**Code head:** `f788823`
**Status:** implementation and machine Gate C complete; independent review pending

## Scope and commits

This candidate packages the corrections requested after the D1D/D1E status
review. It does not change the original Gate D `NOT READY` verdict and does not
approve itself.

| Commit | Review unit |
|---|---|
| `b0da011` | MCP pool/view separation, child selection, hostile protocol fixtures, URL redaction, and retained MCP distributions |
| `2685216` | Descriptor lifetime, private line-log reopening, process adoption cancellation, exact admission weights, and retained descriptor distributions |
| `e4c3f43` | One-shot filesystem/read admission, task-store transaction governance, and typed read failures |
| `f788823` | Deterministic PTY ownership and during-stream resize contract after the first final Gate C attempt exposed two harness races |

The unrelated working-tree edit at `.claude/skills/norn/SKILL.md` is excluded
from every commit and every P0 claim.

## D1D outcome

- The runtime keeps the complete connected MCP pool separately from each
  agent's selected view. A root view can be narrow while a variant or spawned
  child explicitly selects another connected server; deny rules still apply.
- Collision checks use the complete registry. Provider-facing names remain
  server-qualified while execution dispatches the original leaf tool name.
- Shared project definitions still require definition-bound approval. The
  approved-project fixture now proves real startup activation; pending approval
  proves zero process/network activation.
- HTTP protocol fixtures reject invalid JSON-RPC versions, IDs, result shapes,
  initialization fields, protocol negotiation, and repeated pagination cursors.
  Transport errors strip secret-bearing URLs before rendering.
- A real spawned-child fixture proves the selected hidden server is advertised
  and dispatched, rather than merely testing a selection helper.

Live list/inspect/add/remove/enable/disable/reload, child-only connection beyond
the startup pool, dynamic roots, and request-boundary provider-catalog refresh
remain a separate MCP product slice. This candidate claims startup correctness,
not that later live-mutation surface.

## D1E outcome

- `PrivateLineLog` retains no idle file descriptor and revalidates identity on
  every private W5 transaction. Process management no longer probes a spool
  root eagerly.
- A pending-adoption guard kills the process group if cancellation or an error
  occurs before foreground-to-background spool attachment commits.
- The descriptor governor uses an exact one-descriptor observer reserve instead
  of arbitrary transient headroom. Recursive walk and subprocess peaks are
  represented explicitly, with opaque filesystem/subprocess/HTTP permits.
- Official `doctor` and provider paths use their documented weights. Relevant
  HTTP clients disable idle pooling so admission lifetime matches socket
  lifetime.
- Configuration, context, profile, skill, variant, extension, diagnostics, LSP,
  OAuth storage, editing, follow-up, task, and TUI discovery paths now acquire
  typed one-shot admission. Nested task operations reuse one W5 transaction.
- Task reads preserve missing versus corrupt/admission-failed outcomes rather
  than collapsing errors into empty values.

This does not claim that Norn can prevent unrelated embedder or system-wide
descriptor exhaustion.

## First Gate C attempt and PTY correction

The first `cargo test --workspace --all-targets` attempt was not a pass. The PTY
integration file reported one macOS `openpty` `ENXIO` acquisition failure. A
subsequent 20-run distribution of the unchanged file was 19 pass / 1 fail; that
failure showed the resize case racing between the five-row generating panel and
four-row idle panel.

The harness correction serializes parent PTY ownership inside the test process;
the child entrypoints do not acquire that lock. The resize scenario now uses a
delayed provider and asserts the five-row during-stream scroll region it claims
to test. This is test isolation and deterministic synchronization, not a retry,
ignored test, or production bypass.

The retained post-correction distribution is 20/20 complete `pty_smoke` suites,
340/340 individual cases. The subsequent complete workspace/all-target run
passed.

## Retained distributions

| Artifact | Cases | Result |
|---|---:|---:|
| [`2026-07-14-p0-concurrency-final.json`](evidence/2026-07-14-p0-concurrency-final.json) | three process-isolated regressions x 50 | 150/150 |
| [`2026-07-14-d1e-integrated.json`](evidence/2026-07-14-d1e-integrated.json) | six descriptor/cancellation/task regressions x 20 | 120/120 |
| [`2026-07-14-mcp-startup-integrated.json`](evidence/2026-07-14-mcp-startup-integrated.json) | four MCP startup/protocol/cancellation regressions x 20 | 80/80 |
| [`2026-07-14-p0-review-corrections-final.json`](evidence/2026-07-14-p0-review-corrections-final.json) | two prior Gate D corrections x 20 | 40/40 |
| [`2026-07-14-p0-pty-final.json`](evidence/2026-07-14-p0-pty-final.json) | complete 17-case PTY suite x 20 | 20/20 suites, 340/340 cases |

## Machine Gate C

All commands below completed successfully at the code head unless stated
otherwise:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo check --workspace --all-targets`
- `cargo test -p norn --tests`
- `cargo test -p norn-cli --tests`
- `cargo test -p norn-tui --tests`
- `cargo test --workspace --all-targets`
- `cargo test --workspace --doc`
- `cargo test -p norn --doc --features test-utils`
- `git diff --check 41ea210...HEAD` after packaging the documentation commit

The full-range policy artifact
[`2026-07-14-p0-final-policy.json`](evidence/2026-07-14-p0-final-policy.json)
enumerates 227 changed Rust files and 92 production/build-script artifact-writer
candidates. It reports zero production files over 500 syntax-aware LOC, zero
thin-entrypoint violations, and zero added unwrap, expect, panic, suppression,
ignored-test, empty-cfg, lint-allowance, unresolved-comment-marker, todo, or
unimplemented matches. The maximum changed production file is
`crates/norn/src/agent/builder.rs` at 498 LOC.

The marker audit treats `TODO`/`FIXME`/`HACK` as debt only in Rust comments;
`todo!` and `unimplemented!` remain separate prohibited rules. This prevents a
test fixture that defines a policy for rejecting the literal word `TODO` from
being misreported as unresolved debt without weakening the production rule.

## Open acceptance items

- The D1D startup and D1E structural-admission slices require independent
  adversarial reproduction; this record is implementer evidence only.
- The owner must dispose the retrospective Gate A timing exception and Gate B
  baseline-evidence gap without inventing history.
- A fresh reviewer must reconcile the 92-row writer inventory and permit
  lifetimes, rerun the distributions and Gate C commands, and return a
  whole-phase `READY` or `NOT READY` verdict.
- MCP live mutation remains separately planned and unclaimed.
