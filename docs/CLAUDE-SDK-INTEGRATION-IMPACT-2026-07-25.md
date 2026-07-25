# Claude Agent SDK integration: Norn impact and migration guide

**Date:** 2026-07-25

**Audience:** Norn maintainers, Meridian maintainers, and operators using Claude Code subscriptions through Norn

**Implementation branch:** `codex/claude-sdk-parity`

**Preserved remote tip:** `2624391dd369624a6d79cb0954b14a065be87351`

**Branch point:** `44eca74331`

**Claude Runner revision:** `3231faadbecd7cddaeb4c9c4e00c206341c73456`

## Executive summary

This branch adds the foundations for treating Claude Code as a real, controllable
subscription-backed runtime instead of a command that Norn can only invoke once
and scrape for text.

It makes three material changes:

1. Norn's model catalogue knows about the current Claude subscription models,
   including Claude Opus 5, Claude Opus 5 with the one-million-token context
   window, and Claude Sonnet 5.
2. Norn can automatically select a Claude Code subscription provider for those
   catalogue models and forward supported reasoning-effort values.
3. Norn exposes a persistent Claude Agent SDK session wrapper with typed
   messages, concurrent interruption, usage queries, runtime controls,
   respondable permission/dialog/elicitation requests, rich user input, and
   explicit process lifecycle management.

The important architectural qualification is that Norn now has **two different
Claude integration modes**, and they are intentionally not interchangeable:

- `ClaudeRunnerAdapter` is a one-shot, model-only `Provider`. It fits Norn's
  existing provider abstraction, but it rejects tool-bearing requests because
  it cannot safely bind Norn's tool contract through that abstraction.
- `NornWrappedClaudeSession` is a persistent, full Claude Code session. It
  disables Claude's native tools and exposes Norn tools through a strict MCP
  boundary. It is the path intended for an agentic host such as Meridian.

Simply choosing “Claude Opus 5” in existing Norn CLI/provider selection does
**not** silently turn the main Norn loop into the persistent wrapper. It selects
the safe model-only adapter. A consumer wanting the full agentic path must host
the Norn MCP server and explicitly instantiate the persistent wrapper.

That distinction should remain visible in configuration, telemetry, and user
interfaces. Hiding it would make tool availability and session behaviour depend
on an implementation detail users cannot see.

## Why this work exists

Norn already has a strong provider-independent conversation model and tool
runtime. Claude Code subscriptions offer a separate useful capability: access
to Claude through the installed CLI and the user's Claude subscription rather
than Anthropic API billing.

The previous wrapper was too narrow to serve as a serious runtime boundary. It
could launch Claude Code, collect a limited event set, and recover a session ID,
but it did not expose the control plane needed by a collaboration host:

- no typed initialization/capability negotiation;
- no concurrent interrupt handle;
- no explicit permission, question, or elicitation response path;
- no context or subscription-usage queries;
- no rich image/document input;
- no mid-session model, permission, output, MCP, or task controls;
- no complete close/kill/wait lifecycle;
- incomplete coverage of current Claude SDK messages.

The Claude Runner parity work supplies that missing substrate. This Norn branch
adapts it into Norn without pretending the generic `Provider` trait can carry
the whole Claude Agent SDK protocol.

## What has changed

### Model catalogue and provider resolution

The embedded catalogue now defines a Claude Code subscription backend with:

- provider/backend identity for the Claude subscription route;
- authentication through the installed Claude CLI session;
- an Agent SDK API surface;
- Claude Opus 5;
- Claude Opus 5 with the one-million-token context window;
- Claude Sonnet 5;
- the supported Claude reasoning efforts:
  `low`, `medium`, `high`, `xhigh`, and `max`;
- no forced reasoning-effort default, leaving Claude Code's default intact.

When a caller supplies one of these catalogue model names without an explicit
provider selection, Norn can resolve it to the Claude subscription backend.
Explicit provider, backend, API-shape, or profile selections still have
authority. Conflicting explicit selections are rejected rather than silently
rewritten.

This changes the practical meaning of the model name. A current Claude catalogue
model can now be enough to choose the subscription route, but it is not enough
to choose the persistent agentic wrapper.

### Reasoning effort

The selected Norn `ReasoningEffort` is translated to the exact Claude CLI value
when supported. Omitting it leaves the Claude default untouched.

`ReasoningEffort::None` is not sent as a made-up Claude value. In the persistent
wrapper it is rejected with a typed configuration error; callers wanting the
Claude default must omit `reasoning_effort`.

### One-shot provider mode

`ClaudeRunnerAdapter` remains the integration that implements Norn's `Provider`
trait. It builds Claude Runner's subscription-minimal command profile, forwards
the selected model and effort, sends the prompt, consumes the typed event
stream, and fails closed if the terminal result is unsuccessful.

Its security and correctness boundary is deliberate:

- it accepts model-only turns;
- it rejects non-empty Norn tool schemas before spawning Claude Code;
- it does not claim support for Norn's canonical retained response items;
- it does not attempt to translate Norn tools into Claude native tools;
- it does not expose Claude's full session control plane through the `Provider`
  trait.

This means existing Norn flows can use a Claude subscription for text/model work
without accidentally giving Claude a tool surface Norn did not bind.

### Persistent wrapped session mode

`NornWrappedClaudeCode::spawn_session` and
`NornWrappedClaudeCode::spawn_session_with_options` now create a persistent
Claude Code subprocess wrapped in Norn's resource controls.

The session:

- starts from Claude Runner's subscription-minimal profile;
- disables Claude's native tools;
- connects Claude to a Norn MCP server;
- applies a strict allow-list of `mcp__norn__...` tools, or the full Norn MCP
  namespace when the configured list is empty;
- supports a full system-prompt replacement;
- applies an optional model and reasoning effort;
- acquires Norn descriptor-governor capacity for the subprocess pipe peak;
- retains the descriptor permits needed by the running query;
- exposes typed Claude SDK events and controls;
- leaves process completion explicit and observable.

The wrapper accepts the MCP server launch command as a shell-like command line.
It parses that into the executable and arguments rather than invoking it through
a shell.

### Public API

Norn re-exports Claude Runner as:

```rust
use norn::integration::claude::sdk;
```

Consumers should use this re-export for Claude SDK event and control types.
Depending independently on a second `claude-runner` revision risks two
nominally similar but incompatible Rust type graphs.

The primary Norn-owned types are:

```rust
use norn::integration::claude::{
    NornWrappedClaudeCode,
    NornWrappedClaudeConfig,
    NornWrappedClaudeControl,
    NornWrappedClaudeError,
    NornWrappedClaudeSession,
};
```

The persistent session supplies:

- plain and typed user messages, including image-bearing content;
- `next_event`;
- initialization refresh and cached capabilities;
- permission, user-dialog, and elicitation responses;
- interrupt, interrupt-with-options, and interrupt-plus-queued-cancellation;
- context usage and experimental subscription usage;
- permission-mode, model, thinking-token, flag-setting, and output-style
  updates;
- MCP status, reconnect, toggle, replacement, and permission override;
- task stop, task listing, and async-message cancellation;
- close-stdin, close, kill, try-wait, and wait.

`NornWrappedClaudeSession::control_handle` returns a clonable
`NornWrappedClaudeControl` for the subset that must remain available while the
event loop owns the session:

- send a plain user message;
- interrupt;
- inspect capabilities;
- close stdin;
- kill;
- poll or await process exit.

### Resource accounting

Spawning a wrapped Claude session passes through Norn's
`DescriptorGovernor`. The wrapper acquires the known two-pipe spawn peak and
retains permits for the live query. Capacity failure is a typed error, not an
untracked subprocess spawn.

This is particularly important for Meridian and other multi-agent hosts. A
Claude session is not “just another future”; it consumes process descriptors
for its full lifetime and must participate in Norn's admission model.

## What this changes for users

### Model selection

Users can select the new Claude catalogue entries and use the same Norn
reasoning-effort vocabulary used by other providers. If they do not specify an
effort, Claude Code chooses its own default.

### Authentication and billing boundary

The backend uses the installed Claude CLI session. It is not an Anthropic API
key route, and this work does not reproduce Claude Code's private HTTP requests.
Claude Code remains the process that authenticates and communicates with
Anthropic.

The application should say “Claude Code subscription” rather than merely
“Anthropic” wherever that distinction matters. Authentication failures should
direct users to the Claude CLI login/session state, not to Norn's API-key
configuration.

### Tools

The one-shot provider route is text/model-only. A user who selects a Claude
model in an ordinary Norn provider flow must not be promised Norn tools unless
the host has chosen and configured the persistent wrapper.

The persistent path makes Norn, not Claude Code, the tool authority:

- Claude native tools are disabled;
- only the configured Norn MCP namespace is exposed;
- Norn's MCP implementation remains responsible for tool validation,
  authorization, execution, and result fidelity.

### Interrupt and stop

An interrupt is a request to stop the current Claude turn while preserving the
session. It is not the same operation as closing or killing the session.

Hosts should expose separate actions:

- **Interrupt current turn:** send the SDK interrupt and keep the session
  usable.
- **Stop session gracefully:** close input and drain the event/process
  lifecycle.
- **Kill session:** terminate the process as the final fallback.

Conflating those actions recreates the session-loss problem the SDK control
plane is intended to solve.

### Permissions and questions

Claude SDK permission requests, user questions, and MCP elicitations are
respondable protocol messages. A host must either:

- render and answer them through the corresponding response method; or
- choose an explicit policy that guarantees they cannot be emitted.

Logging or displaying the request without responding can leave Claude waiting
forever. Dropping the request is therefore not a safe default.

### Usage

The persistent path can expose both:

- context-window usage for the current conversation; and
- Claude's experimental subscription usage/rate-limit windows.

These are different concepts. A context gauge and a five-hour subscription
window should not be collapsed into one percentage.

## Meridian impact

Meridian currently has two relevant execution paths:

1. a direct Claude Runner print/session path; and
2. its existing Norn runtime path.

This Norn branch does not automatically replace either path. Meridian must make
an explicit routing decision:

- continue using Claude Runner directly for Claude print sessions and use Norn
  for all other providers;
- use `NornWrappedClaudeSession` as the full Claude agentic path so Norn owns
  MCP/tool authority and descriptor admission;
- or support both, but label and test them as distinct modes.

The persistent Norn wrapper is the more coherent route when Meridian wants
Claude to use Norn's tools, because it provides one place for:

- MCP tool binding;
- descriptor admission;
- Claude SDK type re-export;
- session lifecycle;
- typed interaction responses;
- usage and runtime controls.

It is not yet wired into Meridian's current Claude print loop. Meridian's own
handoff documents should be used for that migration; simply advancing its Norn
Git revision is not sufficient.

## Explicit non-goals of this branch

This branch does not:

- call Anthropic's Messages API directly with a Claude OAuth token;
- implement an HTTP proxy or session-log-to-Messages conversion layer;
- translate arbitrary Norn retained response items into Claude SDK history;
- make the generic Norn `Provider` trait sessionful;
- bind arbitrary Norn tool schemas inside the one-shot adapter;
- add a complete interactive UI for Claude permissions or questions;
- prove compatibility against every installed Claude Code release;
- remove the need for a terminal/PTY fallback if Claude print mode changes.

Those are separate projects or later integration stages.

## Important decisions still required

### 1. Is the one-shot compatibility method allowed?

`NornWrappedClaudeCode::run` is retained as a historical convenience while the
new session API is added. Norn's repository policy says there is no backwards
compatibility requirement.

Before merge, the owner should choose one:

- remove `run` and require all callers to use the explicit session API;
- retain it because it remains a useful independent one-shot contract, and
  record that as an explicit product decision;
- move the compatibility behaviour outside the core integration.

It should not survive merely because deleting it might inconvenience an unknown
caller.

### 2. Is the 128-character tool-name limit factual?

The wrapper currently rejects configured tool names longer than 128 ASCII
characters. Norn policy forbids arbitrary caps.

Before merge, the limit needs either:

- a cited MCP/Claude/Norn protocol bound and matching tests; or
- removal in favour of validation derived from the actual namespace grammar.

### 3. Which layer owns full Claude sessions?

Norn now provides the wrapper, but the main Norn agent loop does not use it.
Meridian also has a direct Claude Runner path.

The owner must decide whether the full Claude agentic session is:

- a Norn integration consumed by Meridian;
- a Meridian-owned direct Runner integration;
- or intentionally available in both forms for different products.

The decision affects event storage, tool ownership, retry semantics, process
admission, and the number of `claude-runner` revisions in the final dependency
graph.

### 4. What is the interaction persistence contract?

Permission/dialog/elicitation requests have request IDs and final responses.
For a durable collaboration platform, Meridian/Norn need a defined record for:

- request received;
- request presented;
- response chosen, by whom, and under which policy;
- response submitted;
- late, duplicate, or stale response rejection;
- process/session termination while a request is pending.

The SDK types make the protocol possible; they do not define the product's
durability policy.

## Recommended rollout

1. Review and land the Claude Runner parity branch.
2. Rebase this branch onto the current Norn integration branch; do not merge it
   blindly because both branches have advanced from the same base.
3. Resolve the two Norn policy gates above.
4. Re-run Norn's focused Claude tests, full affected-crate tests, workspace
   checks, strict Clippy, and formatting on the rebased result.
5. Land the Norn branch and publish/pin an accepted Norn revision.
6. Upgrade Meridian to that revision as its own migration, resolving current
   Norn API drift independently of the Claude Runner work.
7. Choose Meridian's direct-vs-Norn session routing.
8. Implement the host interaction-response protocol before enabling any mode
   that can emit permission/dialog/elicitation requests.
9. Add live installed-CLI tests for initialization, one follow-up turn,
   interrupt-and-continue, usage, interaction response, graceful stop, and
   forced kill.
10. Remove duplicate Claude Runner revisions from the final Meridian dependency
    graph once the route is stable.

## Operational invariants

The following should be treated as merge and release invariants:

- No direct Anthropic OAuth/API emulation; Claude Code remains the transport.
- No native Claude tools in the Norn-wrapped path.
- No unbound Norn tools in the one-shot provider path.
- No silent downgrade from persistent agentic mode to model-only mode.
- No fabricated reasoning-effort default.
- No interrupt implemented as process death.
- No permission/question request dropped while the process waits for an
  answer.
- No process spawn outside descriptor admission.
- No terminal failure reported as a successful model turn.
- No second Claude Runner type graph exposed to downstream consumers.

## Status

The implementation is preserved on the remote branch and is attached to the
persistent worktree:

```text
/Users/tom/Developer/ablative/.worktrees/norn-claude-sdk-parity
```

The branch is an implementation candidate, not a merge-ready declaration. Its
main remaining work is reconciliation with current Norn, explicit owner
decisions at the two policy gates, consumer wiring, and live Claude Code
validation.
