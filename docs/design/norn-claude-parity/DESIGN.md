---
type: design
cluster: norn-claude-parity
title: Reconcile preserved Claude SDK parity into current Norn
---

# Reconcile preserved Claude SDK parity into current Norn

> **Cluster:** norn-claude-parity

> **Status:** Draft development-branch checkpoint only. Local focused tests, strict workspace/all-targets Clippy and fmt passed; merge and release blocked by NSP-LIFECYCLE-1 and NSP-UNDERSCORE-1.

## Intention

Integrate real unmerged Claude SDK work without regressing current Norn model, channel, retained-view or session fixes.

## Problem

Nine parity commits diverge from current Norn; a cleanup merge would conflict with selected-route model validation and later Claude error handling.

## Decisions

- Use one isolated integration worktree from the exact pushed integrated base.
- Keep current typed model authority and validated budgets; Claude capability metadata must identify its actual subscription backend.
- An inherited 128-character tool-name cap without protocol authority is removed rather than replaced with an invented value.
- Main Norn driver ownership and Liminal host responsibilities remain explicit; persistent SDK library functionality alone does not assert they are wired.
- Draft development-branch checkpoint only. Local focused tests, strict workspace/all-targets Clippy and fmt passed; merge and release blocked by NSP-LIFECYCLE-1 and NSP-UNDERSCORE-1.
- NSP-LIFECYCLE-1 — BLOCKED: Runner 3231faadbecd7cddaeb4c9c4e00c206341c73456 Query::wait removes the sole live child controller before a blocking wait. Cancelling the Norn wait can detach that owner and release descriptor admission before actual teardown. The SDK also aborts an unfinished blocking reader without joining it. Fix and deterministically prove child/reader ownership before landing; no safe complete fix through the existing wrapper API was identified.
- NSP-UNDERSCORE-1 — BLOCKED: ten inherited underscore operations remain in changed source: build.rs:399 and adapter.rs:156,161,164,172,186,196,207,226,334. Seven discard channel-send Results; two adapter bindings retain descriptor permits and one build binding discards an error. Replace silent result handling and resolve the bindings structurally while preserving resource lifetimes. Passing Clippy and no newly introduced AST violation do not close this policy finding.
- Local proof: /private/tmp/norn-claude-parity-integration-proof/INTEGRATION-HANDOFF.json; checked source freeze SHA256 540cc75d5459346d5bc4aa386544c06c95da70418748d14826fd852afec74ba4. Focused tests: 38 Claude, 13 model-selection, 3 catalogue, 187 config, 16 runtime resolution and 15 CLI/child cases. After the sole test-only Clippy repair, strict workspace/all-targets Clippy including compile-only live-smoke, the exact repaired adapter test, and workspace fmt passed. Full workspace runtime tests were deferred to release the shared build lane; no live provider, current Fable acceptance or exact-commit venue receipt.

## Structure

| Path | Note | Brief |
|------|------|-------|
| `Cargo.lock` | NSP-001 R1 exact file wall |  |
| `Cargo.toml` | NSP-001 R1 exact file wall |  |
| `assets/models.json` | NSP-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/model_aliases.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn-cli/src/config/provider_selection.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn-cli/src/runtime/resolve.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn/build.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn/src/loop/command_options.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn/src/model_catalog.rs` | NSP-001 R1 exact file wall |  |
| `crates/norn/src/integration/claude/adapter.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/adapter/effort_tests.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/adapter/validation.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/mod.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/command_line.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/config.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/control.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/error.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/events.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/legacy.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/session.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/claude/wrapped/tests.rs` | NSP-001 R2 exact file wall |  |
| `crates/norn/src/integration/mod.rs` | NSP-001 R2 exact file wall |  |
| `docs/CLAUDE-SDK-INTEGRATION-IMPACT-2026-07-25.md` | NSP-001 R3 exact file wall |  |
| `docs/reviews/2026-07-25-claude-sdk-parity-implementation-handoff.md` | NSP-001 R3 exact file wall |  |
| `crates/norn/src/model_selection.rs` | NSP-001 R1 named before-edit integration amendment |  |
| `crates/norn/src/model_selection/tests.rs` | NSP-001 R1 named before-edit integration amendment |  |
| `crates/norn/tests/codex_catalogue_snapshot.rs` | NSP-001 R1 named before-edit integration amendment |  |
| `crates/norn/src/integration/claude/wrapped/execution.rs` | NSP-001 R2 pre-edit execution-mode module rename |  |

## Constraints

- **P1** — No source outside row walls; add exact paths before extending scope.
- **P2** — Preserve current NMS/NCS/NRT/NFP semantics; no main merge, release or install from this worktree.
- **P3** — No Cargo until parent releases the build lane. Direct formatting/static checks are allowed.
- **P4** — No arbitrary limits, compatibility shims, bypass attributes, library unwrap/expect/panic or ignored tests.
- **P5** — The persistent Claude SDK wrapper is not proof of a Norn session daemon, live attach/detach or Liminal host integration.
- **P6** — This checkpoint preserves unfinished work on a development branch; it grants no main merge, release, installation, original-branch deletion or visibility-change approval.
