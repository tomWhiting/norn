# Claude SDK parity: Norn implementation and review handoff

**Date:** 2026-07-25

**Repository:** `/Users/tom/Developer/ablative/norn`

**Persistent review worktree:** `/Users/tom/Developer/ablative/.worktrees/norn-claude-sdk-parity`

**Branch:** `codex/claude-sdk-parity`

**Remote tip:** `2624391dd369624a6d79cb0954b14a065be87351`

**Original branch point:** `44eca74331`

**Do not merge directly into:** `feat/retry-forever`

## Purpose

This document is the hands-on handoff for the implementation candidate. It is
intended to let a maintainer rebase it, review it in coherent slices, correct
policy issues, run the right evidence battery, and give Meridian an exact
accepted revision.

It is deliberately separate from
`docs/CLAUDE-SDK-INTEGRATION-IMPACT-2026-07-25.md`, which explains the product
and architecture impact.

## Preservation and branch state

The work is not in a temporary directory and is not dependent on an untracked
worktree:

```text
remote branch: origin/codex/claude-sdk-parity
remote tip:    2624391dd369624a6d79cb0954b14a065be87351
worktree:      /Users/tom/Developer/ablative/.worktrees/norn-claude-sdk-parity
```

At the time of handoff, the active Norn development branch is
`feat/retry-forever` at `efe9758`, with an unrelated untracked `.mcp.json` in
the active checkout. That active checkout was not modified.

The two branches share `44eca74331` and have both advanced:

```text
feat/retry-forever-only commits: 5
claude-sdk-parity-only commits:  8
```

The active-side commits include retry, cancellation, stdout JSON, and persistent
worker changes in exactly the execution and lifecycle territory this branch
touches conceptually. Rebase or transplant the Claude commits onto the chosen
current Norn integration branch. Do not merge the two heads and treat a clean
textual merge as sufficient review.

## Commit train

Review the implementation in this order:

| Commit | Purpose |
|---|---|
| `03d5cf3` | Pin the current Claude Runner revision. |
| `3e18e2d` | Add Claude Opus 5, Opus 5 `[1m]`, and Sonnet 5 catalogue entries. |
| `5b1a8d9` | Forward supported Claude reasoning efforts. |
| `0946867` | Resolve Claude catalogue models to the subscription adapter. |
| `0201c2d` | Advance to the Runner control-plane implementation. |
| `a9b8dfb` | Add persistent Norn-wrapped Claude SDK sessions. |
| `6f6426e` | Isolate Claude subscription provider resolution from other paths. |
| `2624391` | Keep provider-resolution coverage lint-clean and deterministic. |

The branch changes 23 files with approximately 2,603 additions and 400
deletions. The size is mostly tests and the split persistent wrapper modules,
not a single monolithic integration.

## Main code surfaces

### Dependency and catalogue

- `Cargo.toml`
- `Cargo.lock`
- `assets/models.json`
- `crates/norn/src/model_catalog.rs`

Review for:

- exact accepted Claude Runner revision;
- no unintended second Runner revision inside Norn;
- catalogue identity, aliases, context windows, effort set, and lack of a
  fabricated default;
- authentication/API-surface labels that make subscription routing explicit.

### CLI/provider resolution

- `crates/norn-cli/src/config/provider_selection.rs`
- `crates/norn-cli/src/config/model_aliases.rs`
- `crates/norn-cli/src/runtime/resolve.rs`
- associated provider/model resolution tests

Review for:

- catalogue model auto-routing only when provider selection is not explicit;
- explicit provider/backend/API/profile authority;
- conflicts rejected rather than silently reinterpreted;
- no leakage of Claude-specific routing into other providers;
- effort validation after the final model/provider choice.

### One-shot provider

- `crates/norn/src/integration/claude/adapter.rs`
- `crates/norn/src/integration/claude/adapter/validation.rs`
- `crates/norn/src/integration/claude/adapter/effort_tests.rs`
- `crates/norn/src/integration/claude/adapter/role_authority_tests.rs`

Review for:

- use of `ClaudeCommand::minimal_subscription`;
- exact model and effort forwarding;
- omission preserving the Claude default;
- explicit rejection of `ReasoningEffort::None`;
- rejection of non-empty tool schemas before spawn;
- rejection of retained/canonical response-item shapes the adapter cannot
  faithfully translate;
- terminal result/error handling remaining fail-closed;
- no native Claude tools accidentally re-enabled.

### Persistent wrapper

- `crates/norn/src/integration/claude/mod.rs`
- `crates/norn/src/integration/claude/wrapped.rs`
- `crates/norn/src/integration/claude/wrapped/config.rs`
- `crates/norn/src/integration/claude/wrapped/command_line.rs`
- `crates/norn/src/integration/claude/wrapped/session.rs`
- `crates/norn/src/integration/claude/wrapped/control.rs`
- `crates/norn/src/integration/claude/wrapped/events.rs`
- `crates/norn/src/integration/claude/wrapped/error.rs`
- `crates/norn/src/integration/claude/wrapped/legacy.rs`
- `crates/norn/src/integration/claude/wrapped/tests.rs`

Review for:

- native Claude tool disabling;
- strict `mcp__norn__...` namespace validation;
- empty configured tool list meaning the full Norn MCP namespace, not “no
  tools”;
- shell-like parsing without shell execution;
- descriptor-governor admission and retained permit accounting;
- startup initialization and typed error mapping;
- the main session API and the intentionally smaller clonable control API;
- EOF, malformed-stream, child-exit, close, and kill behaviour;
- request/response ID fidelity for permission, dialog, and elicitation;
- direct re-export of Runner SDK types from
  `norn::integration::claude::sdk`.

## API contract under review

### `NornWrappedClaudeConfig`

The configuration contains:

- `claude_code_path`;
- `mcp_server_address`;
- `norn_tools`;
- `system_prompt`;
- optional `model`;
- optional `reasoning_effort`.

Its effective command must remain subscription-minimal and strict:

```text
Claude Code subprocess
  native tools: disabled
  setting sources: disabled
  bundled skills/commands/agent view/background helpers: disabled
  system prompt: full Norn/host replacement
  external tools: strict Norn MCP namespace only
```

Reviewers should compare the resulting `ClaudeCommand` with the accepted Claude
Runner migration handoff rather than treating the builder call chain as
self-explanatory.

### `NornWrappedClaudeSession`

The session is the owner of the event receiver and the complete control surface.
The intended host pattern is:

```rust
let wrapped = NornWrappedClaudeCode::new(config);
let mut session = wrapped.spawn_session().await?;
let control = session.control_handle();

session.send_user_message("Begin")?;

while let Some(event) = session.next_event().await? {
    // Persist/translate events.
    // Route respondable control requests to the user or policy engine.
    // Use `control` from another task for interrupts or shutdown.
}

let status = session.wait().await?;
```

The actual host loop must also select process exit, cancellation, and outbound
commands. The example only shows ownership.

### `NornWrappedClaudeControl`

The clonable handle is deliberately narrower than the session. It exists so a
host can interrupt or shut down a session while another future waits on
`next_event`.

Do not casually add every Query method to this handle. Some operations depend on
serialized protocol state or access to the session-owned receiver. Any expansion
needs a concurrency review.

## Gate A: owner decisions required before correction work

### A1. Historical `run` compatibility

`wrapped/legacy.rs` retains `NornWrappedClaudeCode::run`.

Repository policy explicitly says no backwards compatibility. The owner must
decide whether this method:

- is deleted;
- is kept as a genuinely supported one-shot API with an explicit rationale; or
- is moved out of the core integration.

This is a product/API decision, not a reviewer preference.

### A2. Tool-name length

`wrapped/config.rs` rejects tool names longer than 128 ASCII characters.

Repository policy rejects arbitrary limits. Require one of:

- a protocol citation and boundary tests proving 128 is the real limit; or
- removal of the cap, leaving only grammar/namespace validation.

Do not replace it with a different guessed number.

### A3. Full-session ownership

Choose whether Norn's wrapper is the canonical full Claude session for Meridian
or merely a library option beside Meridian's direct Claude Runner loop.

This decision determines which code owns:

- Norn MCP launch and authentication;
- descriptor admission;
- event persistence;
- permission/question response routing;
- retry/recovery;
- final process classification.

It must be settled before both implementations independently grow overlapping
production behaviour.

## Gate B: rebase and mechanical reconciliation

1. Record the original branch and tip shown above.
2. Create a fresh persistent worktree/branch from the intended current Norn
   integration head.
3. Rebase or cherry-pick the eight commits in order.
4. Resolve dependency and lockfile changes intentionally.
5. Reconcile retry/cancellation semantics with `feat/retry-forever`; do not
   retain two competing lifecycle authorities.
6. Re-check file sizes against the repository's production-file limit.
7. Run formatting after reconciliation.
8. Inspect the complete diff against the chosen base before running expensive
   gates.

Because the implementation branch and active branch diverged from the same
base, the accepted SHA is expected to change.

## Gate C: focused correctness review

### C1. Provider selection matrix

Test at minimum:

- Claude catalogue model with no explicit provider selects the subscription
  backend;
- explicit Claude subscription selection works;
- explicit non-Claude provider retains authority;
- provider/backend/API/profile conflicts fail with actionable errors;
- aliases resolve before final effort validation;
- no Claude route affects OpenAI, Anthropic API-key, local, or other provider
  selections.

### C2. Effort matrix

For each Claude catalogue model:

- omitted effort produces no `--effort` override;
- `low`, `medium`, `high`, `xhigh`, and `max` use exact CLI strings;
- `none` fails before subprocess spawn;
- unsupported future values fail visibly;
- switching models clears or rejects an effort unsupported by the destination
  model.

### C3. One-shot adapter contract

Prove:

- no tools: request can reach the fake CLI;
- any non-empty tool schema: request fails before spawn;
- retained provider-native history: request fails before lossy translation;
- non-success terminal result: provider call fails;
- child non-zero exit without a success result: provider call fails;
- malformed JSON/event: provider call fails with useful context;
- model and effort are present only when requested.

### C4. Persistent config and command

Prove:

- native tools remain disabled;
- strict Norn MCP tool patterns are exact;
- malformed/non-Norn tool names fail;
- empty `norn_tools` produces the intended wildcard namespace;
- quoted MCP executable/argument parsing is correct;
- invalid/empty MCP command fails before spawn;
- full system-prompt replacement is applied;
- subscription-minimal environment/settings remain applied.

### C5. Descriptor accounting

Prove:

- insufficient capacity fails before spawn;
- spawn reserves the two-pipe peak;
- live session retains the documented descriptor count;
- all success, startup-failure, EOF, kill, and drop paths return permits exactly
  once;
- concurrent session admission observes the global governor.

### C6. Protocol and lifecycle

Using a deterministic fake Claude CLI, prove:

- initialization is captured and capabilities are queryable;
- multiple user turns share one process/session;
- rich input reaches stdin without lossy conversion;
- permission/dialog/elicitation request IDs round-trip exactly;
- interrupt acknowledgement is not fabricated before the protocol response;
- queued-input cancellation semantics match the Runner contract;
- graceful close drains events and yields the real exit status;
- kill remains available while event receive or interrupt is pending;
- malformed stream terminates/cleans up rather than hanging;
- process exit is observed even if stdout remains oddly behaved;
- non-zero exit is not classified as success.

## Gate D: repository evidence

After rebasing and corrections, run the repository-mandated evidence battery.
At minimum:

1. focused Claude integration tests;
2. provider-selection and catalogue tests;
3. descriptor-governor tests;
4. affected Norn CLI tests;
5. affected crate test suites;
6. workspace all-target checks;
7. strict Clippy with warnings denied;
8. formatting;
9. any repository-specific clean-pass repetition required by the current
   branch instructions.

The historical branch run reported:

- 4,428 of 4,434 main Norn tests passing, with six Rhai/strict-session failures
  also reproduced from the base;
- 533 of 534 Norn CLI tests in one parallel run, with the remaining
  environment-sensitive test passing in isolation;
- focused Claude tests and strict Clippy passing.

Those numbers are provenance, not current acceptance evidence. Do not waive
failures merely because they existed at the old branch point. Reproduce and
classify them against the actual rebased base.

No build or test was run while producing this documentation.

## Gate E: live Claude Code validation

Fake CLI coverage proves framing and lifecycle logic but cannot prove the
installed CLI contract. On a dedicated test account/session, verify:

1. initialization and capability capture;
2. one prompt and one follow-up in the same Claude session;
3. all supported effort values, plus omitted effort;
4. current model selection for Opus 5, Opus 5 `[1m]`, and Sonnet 5 as available;
5. context usage;
6. subscription usage/rate-limit payload;
7. interrupt during output, followed by another successful turn;
8. permission/dialog/elicitation request and response where the CLI can be made
   to emit each;
9. Norn MCP tool invocation with native tools absent;
10. graceful close and forced kill;
11. authentication-expiry/error classification;
12. an unknown future event preserved as raw data rather than crashing.

Sanitize account data, prompts, tokens, and local paths in stored fixtures.

## Review tiers

### Tier 0: preservation and provenance

Confirms exact base, branch, Runner revision, generated catalogue source, and
no uncommitted implementation hidden outside Git.

### Tier 1: dependency, catalogue, and routing

Reviews the first five commits. Suitable for maintainers familiar with Norn
configuration and model resolution.

Blocking defects include wrong model IDs, route leakage, overridden explicit
authority, fabricated defaults, and duplicate/incompatible Runner types.

### Tier 2: runtime and resource safety

Reviews the persistent wrapper, descriptor accounting, protocol serialization,
and process lifecycle.

This is mission-critical code and requires a rigorous implementation reviewer.
Blocking defects include descriptor leaks, unkillable waits, incorrect exit
classification, request-ID corruption, or any path that can leave a subprocess
or caller permanently stuck.

### Tier 3: security and authority

Reviews tool disabling, MCP namespace restrictions, prompt authority,
permission responses, command parsing, and error redaction.

Blocking defects include a native tool escaping the Norn boundary, an
unvalidated executable/argument path, unbound tool execution, or silently
approved permissions.

### Tier 4: consumer integration

Reviews the accepted Norn revision inside Meridian, including duplicate
dependency elimination, session routing, interaction persistence, UI
semantics, and shutdown behaviour.

Norn is not integration-complete merely because its library tests pass.

## Expected corrections and follow-on work

The following are expected after this branch, not evidence that the branch is
already complete:

- resolve the two Gate A policy issues;
- reconcile with current retry/cancellation work;
- decide whether the main Norn agent loop will ever host the persistent wrapper;
- add a durable interaction-request record if Norn rather than Meridian owns
  that persistence;
- replace synchronous pipe-write assumptions if Runner's control handle gains
  an asynchronous/backpressured writer;
- track new SDK message variants and Runner APIs as the TypeScript reference
  moves;
- add live-version compatibility evidence;
- coordinate the Meridian Norn revision upgrade and route selection.

## Meridian handoff contract

Once the Norn branch is accepted, provide Meridian maintainers:

- the exact accepted Norn commit;
- the exact transitively accepted Claude Runner commit;
- the public import path `norn::integration::claude::sdk`;
- the chosen disposition of the legacy `run` method;
- the chosen full-session ownership model;
- the final tool-name validation contract;
- a list of live-tested Claude Code versions;
- the event/request types Meridian must persist and answer;
- descriptor-governor expectations;
- shutdown and non-zero-exit semantics.

Meridian should not advance to a floating branch or infer this contract from
the Rust types alone.

## Completion definition

This Norn work is ready for consumer adoption only when:

- it is reconciled onto the intended current Norn base;
- all Gate A decisions are recorded and implemented;
- focused and repository-wide evidence is green or every inherited failure is
  freshly reproduced and explicitly accepted;
- a rigorous runtime/security review has no blocking findings;
- installed Claude Code has passed the live matrix;
- Meridian has an exact Norn revision and an explicit routing plan.
