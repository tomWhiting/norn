# P3 cross-session fixture handoff

**Date:** 2026-07-17

**Source range:** `d9242b8..4662fbc`

**Source commit:** `4662fbc` (`test(responses): prove canonical child lifecycles`)

**Status:** Reviewable fixture candidate, not P3 acceptance

## Outcome

This slice closes the previously unproved representative cross-session seams
without changing production behavior:

- the live second `store:false` request now has an exact full-input assertion;
- a real persistent `SpawnAgentTool` run writes a representative canonical
  non-audio output vector to the nested child session and reserializes it for
  stateless replay;
- a real persistent `ForkTool` run copies the parent's canonical vector through
  anchor truncation and `seed_fork_events`, reloads it from the nested child
  file, resumes it through `SessionManager`, and reserializes it unchanged;
- the shared fixture covers reasoning, annotated/logprobed output text, hosted
  search, image generation, MCP output, and code-interpreter image output;
- no Clippy suppression, `unwrap`, `expect`, or `panic` was added, and the new
  shared test module is 153 physical lines.

The launch fixtures passed without a production correction. The gap was
evidence weakness, not a confirmed child-session implementation defect.

## Exact request evidence

The prior runner assertion accepted a matching hosted-search subsequence plus
any later correlated result. It could not reject an extra or reordered item.
The replacement fixture requires the live continuation input to equal, in
order:

1. the user input item;
2. the exact preceding canonical `response.output` vector;
3. the correlated `function_call_output`;
4. the runner-owned dynamic Collaboration Mode Developer tail.

The persisted direct-reload seam requires the exact durable sequence plus the
final completed item. It deliberately excludes the dynamic Developer tail,
which the runner regenerates for each iteration rather than persisting as
provider transcript history.

## Audio contract correction

The OpenAI Developer Docs MCP was used as the API authority. The current
[Responses streaming event reference](https://developers.openai.com/api/reference/resources/responses/streaming-events)
defines four response-scoped audio events:

| Event | Required schema payload |
|---|---|
| `response.audio.delta` | `type`, `sequence_number`, Base64 `delta` |
| `response.audio.done` | `type`, `sequence_number` |
| `response.audio.transcript.delta` | `type`, `sequence_number`, text `delta` |
| `response.audio.transcript.done` | `type`, `sequence_number` |

They have no item ID, output/content index, codec or MIME type, or authoritative
terminal payload. The examples include `response_id`, but the schemas do not,
so it may be retained raw when present but cannot be required. The terminal
Responses output union has no audio item.

Consequently the plan no longer asks for impossible streamed/terminal audio
equivalence. The future implementation is a non-replayable response-scoped
private artifact sidecar with stream-to-artifact persistence/reload evidence.
Until D2 permits that durable representation, the existing raw retention plus
typed `UnsupportedResponseMedia` failure remains correct.

## D2 blocker

Two independent audits confirmed that D2 is substantive:

- `SESSION_FORMAT_VERSION` remains 1 even though canonical `response_items`
  were added to `AssistantMessage`;
- a pre-canonical format-1 Serde reader can ignore the unknown field and later
  fork or rewrite only the lossy flat projection;
- the current reader warns on a future header version and continues, so a
  header bump alone does not exclude already-installed writers;
- format-0/headerless sessions can also receive current canonical events.

The runtime must not add audio or other new durable transcript state to this
format. The owner decision remains: either isolate a strict new namespace that
legacy binaries cannot discover, or build an explicit offline, atomic,
idempotent migration with byte-identical backup/recovery and an honest
`LegacyProjection`-style representation for unrecoverable history. Given
Norn's auditability requirement, offline migration is the recommended end
state, but this handoff does not record that as an owner ruling.

## Verification

All builds used the repository's normal `target/` directory.

| Command | Result |
|---|---|
| `cargo test -p norn under_persistent_parent_persists_child_timeline -- --nocapture` | 2/2 passed: real persistent spawn and agent fork |
| `cargo test -p norn hosted_search_survives_runner_tool_continuation_and_persisted_resume -- --nocapture` | 1/1 passed with exact input equality |
| `cargo test -p norn canonical_ -- --nocapture` | 36/36 selected library tests passed; 2/2 selected catalog integration tests passed |
| `cargo test -p norn responses_replay_matrix -- --nocapture` | 1/1 passed |
| `cargo test --workspace --all-targets` | `norn` 3710/3710; follow-up 13/13; model catalog 6/6; auth API 2/2; skill assembly 2/2; static Codex API 1/1; CLI 485/485; TUI 682/682; PTY 17/17 |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `cargo test --workspace --doc` | 8/8 Norn doctests passed; other crates had zero doctests |
| `cargo fmt --all -- --check` | Passed after formatting |
| `git diff --check` | Passed |
| Added bypass scan | Zero added lint allows, unwraps, expects, or panics |

## Structured independent audit

A persistent structured Norn `gpt-5.6-sol` review was retained as required:

- **Session ID:** `f4c3fb8c-d330-43f9-b334-90f2597b2da7`
- **Envelope:** `/Users/tom/.norn/delegations/codex-review-p3-cross-session-20260717.json`
- **Stop reason:** `completed`
- **Reviewed source:** `d9242b8` before this slice

Its `not_ready` verdict identified the D2 boundary, the missing real
spawn/agent-fork evidence, and the need to label the fixture representative
non-audio coverage rather than full multimodal closure. This slice addresses
the launch-path evidence finding. D2 and exhaustive media coverage remain open,
so the prior verdict is not represented as an acceptance review of `4662fbc`.

## Claim inventory

Closed by `4662fbc`:

- canonical projections are authoritative over poisoned flat projections;
- uninterrupted and persisted exact second-request replay is pinned;
- representative canonical items traverse real persistent spawn and agent-fork
  launch paths, disk reload, and manager resume;
- stream provenance remains outside replayed provider JSON.

Still open:

- D2 version rejection versus offline migration and old-writer exclusion;
- exhaustive public/Codex content and media lifecycle inventory;
- durable response-scoped audio artifact sidecar;
- retained final P3/P4 gate evidence and independent acceptance review.

## Worktree boundary

The following pre-existing changes were not edited, staged, or included in
`4662fbc`:

- `.claude/skills/norn/SKILL.md`
- `CONVENTIONS.toml`
- `crates/norn/src/tools/diagnostics_check/tests.rs`
- `docs/reviews/evidence/p1/__pycache__/`
- `scripts/`
