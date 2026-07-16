# P3 output-inventory implementation handoff

**Date:** 2026-07-17

**Verdict requested:** implementation-candidate review, not P3 or P4 acceptance

**Base:** `6e279a3`

**Source head:** `07bf9c1`

**Review range:** `6e279a3..07bf9c1`

## Outcome

This range closes the bounded output-inventory and canonical-lifecycle slice of
P3/P4. It does not close D2, response-scoped audio, the exhaustive all-lifecycle
media matrix, retained phase evidence, or either phase's independent acceptance
gate.

The official contract used for this slice contains 28 public Responses output
item discriminators. None is treated as a Codex-only output item. Response audio
is instead represented by four response-scoped stream events with no output-item
identity or terminal audio item; this range deliberately does not invent one.

The public tool inventory contains 16 tool schemas represented by 18 accepted
tool literals. Output-item actionability is explicit: 20 inert items, six
executable items, and two conditional items. "Inventoried" does not mean every
item is locally executable. Unsupported executable and conditionally executable
forms fail closed after their authoritative schema is checked.

## Source commits

1. `7429490` `feat(responses): enforce authoritative output-item contracts`
2. `ad9fffe` `feat(responses): preserve caller ownership through replay`
3. `07bf9c1` `feat(agents): preserve canonical lifecycle across fork and spawn`

## Contract and reconciliation

- A checked manifest pins the exact ordered 28-discriminator public output union.
- Every public discriminator maps to an explicit authoritative validator.
- The shipped nested schemas cover core, hosted, container, advanced, client,
  and tool-definition shapes, including recursive filters and MCP headers.
- Known malformed items fail before unsupported-capability classification and
  before canonical identity/channel/completion mutation, both for
  `response.output_item.done` and terminal response output.
- Unknown future discriminators retain the existing opaque, non-executable,
  typed-unsupported behavior rather than being accepted by a generic decoder.
- Exact duplicate frames are idempotent; identity conflicts and incompatible
  authoritative completion remain typed protocol failures.

## Canonical replay and caller lineage

- Local call/result resolution is canonical rather than reconstructed from a
  grouped display projection.
- `caller` preserves absent, explicit `null`, and object forms. Program-owned
  function calls retain their program caller and are linked to an active program
  before persistence, hooks, or execution.
- Canonical items and caller ownership survive assembly, JSONL persistence,
  reload, manager resume, top-level session fork, `store:false` replay, and the
  representative real persistent spawn and real agent-fork paths.
- The representative real spawn fixture carries the 22-item non-audio lifecycle
  vector; the real fork fixture carries the 24-item resolved-history vector.
  This is representative evidence, not an exhaustive optional-shape or
  all-discriminator lifecycle claim.
- This range establishes caller lineage only. P7 still owns complete
  programmatic-tool capability negotiation and tool-envelope behavior.

"Exact replay" refers to the parsed provider JSON value. It does not claim
preservation of SSE whitespace, JSON member ordering, or transport framing.

## Official basis

The primary sources were the OpenAI Developer Docs MCP contract and official
generated SDK types where the prose guide did not expose the complete returned
resource schema:

- Programmatic tool calling and the separate `program`, program-owned
  `function_call`, and `program_output` items:
  <https://developers.openai.com/api/docs/guides/tools-programmatic-tool-calling#understand-program-response-items>
- Computer use and optional modifier keys on mouse actions:
  <https://developers.openai.com/api/docs/guides/tools-computer-use>
- Local shell:
  <https://developers.openai.com/api/docs/guides/tools-local-shell>
- Apply patch:
  <https://developers.openai.com/api/docs/guides/tools-apply-patch>
- MCP and connectors:
  <https://developers.openai.com/api/docs/guides/tools-connectors-mcp>

## Verification

All commands used the repository's normal `target/` directory.

- `cargo fmt --all -- --check`: pass.
- `git diff --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass, with no lint
  suppression added.
- `cargo test -p norn --lib`: 3,785 passed after the final schema-order fix.
- `cargo test --workspace --all-targets`: pass, including 3,790 Norn library,
  485 CLI, 682 TUI, macro compile-fail, and integration test groups.
- `cargo test --workspace --doc`: pass, including four compile and four
  compile-fail doctests.
- Final source-size audit: 49 production-touched/new production files, all below
  500 production lines; maximum `response_reconciler.rs` at 498.
- Pre-existing files whose production prefix already exceeds 500 were changed
  only in test regions: `builder.rs` 741, `context_edit.rs` 669, and
  `fork_tool.rs` 668.
- Final added-line audit: 7,780 Rust lines, with zero added `unwrap`, `expect`,
  `panic`, `#[allow]`, or expectation attributes.

## Independent review

The local integrated adversarial review found no blocker, major, or minor issue
after verifying the four client-call schemas, 28-validator manifest, error
non-disclosure, and schema-before-capability/state-mutation ordering.

Persistent structured Norn review session:
`44922544-ec72-45e8-a9ba-a597dd10f2e1`.

Retained envelopes:

- `/Users/tom/.norn/delegations/codex-review-p3-responses-20260716T221140Z.json`
  records the initial upstream `response.failed` without losing the session.
- `/Users/tom/.norn/delegations/codex-review-p3-responses-follow-up-20260716T221955Z.json`
  records the resumed review and its one schema-ordering comment.
- `/Users/tom/.norn/delegations/codex-review-p3-responses-correction-20260717.json`
  records the correction pass and a disputed `double_click.keys` comment.
- `/Users/tom/.norn/delegations/codex-review-p3-responses-correction-refutation-20260717.json`
  records the current `ready` verdict with no findings after the explicit OpenAI
  guide and generated SDK type proved `double_click.keys` is optional.

## Open work

- Resolve D2 with either an isolated rejecting namespace or an approved offline,
  atomic migration.
- Implement the D2-compatible response-scoped audio artifact sidecar and its
  stream, corruption, cancellation, reload, resume, spawn, and fork evidence.
- Complete the exhaustive optional-shape/all-lifecycle media matrix.
- Retain the final P3 and P4 Gate C bundles and obtain independent phase
  acceptance. The current clean reviews are implementation-candidate inputs,
  not those acceptance verdicts.
- Continue P5-P9 only after the owning dependencies and review stops permit it.
