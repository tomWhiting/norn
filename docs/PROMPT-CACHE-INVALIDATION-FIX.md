# Prompt-Cache Invalidation in Per-Iteration Context

**Status:** the original placement correction was implemented 2026-07-09. The
typed D8 reconciliation is frozen as an implementation candidate at source
`4fa6c6756ed497a002b4281f51cbb14f7bd7a3eb` (tree
`c0d9f69bb5283184432862016c1212644f7088c2`) and remains pending focused Gate D.
**Author:** Frodo (original diagnosis and design).

The original defect was reported by an external adopter running Norn headless
through Aion. The correction keeps volatile runtime context out of the stable
prefix while preserving current request values; prompt-command output follows
an explicitly configured cache TTL when present. D8 subsequently made the
transport explicit:

- Stateless providers append one compatibility Developer message at the
  request tail, containing Norn-owned runtime policy followed by trusted
  prompt-command output.
- Provider-threaded Responses sends Norn-owned runtime policy through current
  request-local `instructions`; the wire field is an instruction channel, not
  a literal message-role discriminator, and is not stored as an input item.
- Trusted prompt-command output is an explicit Developer seed item on threaded
  requests. An unchanged value is inherited; a changed value cuts the anchor
  and is sent once with the replacement seed.
- Stable product, operator, repository, and human content retains its typed
  System/Developer/User authority rather than being flattened into one System
  message.

See `crates/norn/src/loop/dev_context.rs`,
`crates/norn/src/loop/runner/prompt.rs`, and
`crates/norn/src/system_prompt/plan.rs`.

---

## Original defect

Before the 2026-07-09 correction, Norn rewrote one managed dynamic-context
Developer message near the start of the message list on every iteration. That
message contained a `# Environment` section with a second-resolution clock, so
its bytes changed every turn. Because provider prompt caches cover only a
contiguous prefix, the change invalidated the cache for the complete growing
history after that message.

The timestamp was only the most reliable trigger. Collaboration mode,
prompt-command output, hosted-tool framing, and any other volatile managed
section caused the same class of invalidation. The correction therefore changed
the position/projection of the complete managed container rather than freezing
or special-casing an individual field.

**Reported impact:** approximately 554 million input tokens across 147 sessions,
with a prompt-cache hit rate near 13 percent where agent loops normally exceeded
90 percent. `cache_read` remained pinned near the static prompt size while the
per-turn input grew with a roughly 150-turn history. The raw diagnostic corpus
remains with the reporter.

## Current request layout

The stable prompt is an ordered `PromptPlan`; it is not one authority-flattened
string:

| Stable source | Authority | Cache/anchor treatment |
|---|---|---|
| Compiled product, child, fork, and built-in policy | System | Projected to current Responses instructions; stable on stateless transports |
| Trusted operator profile, user `~/.norn/NORN.md`, operator rules/skills | Developer | Included in the stable non-System seed and sent once per threaded anchor |
| Repository context, workspace profile/rules/skills, configured variants, human task | User | Included in the stable non-System seed or durable turn history |

Persisted user, assistant, tool, compaction, and sourced-rule events follow the
stable prefix. Rule delivery formatting does not choose authority: operator
rules reconstruct as Developer, workspace rules as User, and readable
originless pre-D8 rows reconstruct conservatively as User.

Volatile request context is assembled before stable-seed reconciliation and
preflight, then split by authority:

- `# Environment`, including current time, working directory, branch, session,
  model, platform, and shell.
- `# Collaboration Mode`.
- Norn-owned hosted-tool surface framing selected from current provider
  capabilities. Runtime MCP descriptions themselves remain only in live tool
  definitions.
- Trusted prompt-command output at Developer authority.

Sourced rule injections are no longer materialized into this volatile block.
They are durable messages with origin-derived authority.

Prompt commands are resolved exactly once while preparing each request. A
command without `cache_ttl` always executes and cannot populate the cache. A
cache hit requires the same command name, exact command text, configured TTL,
and working directory. Its absolute deadline is set after the successful
execution; hits do not extend that deadline. If the deadline cannot be
represented on the platform clock, Norn uses the fresh output for that request
but does not cache it.

### Stateless providers

`ManagedDevMessage` detaches the prior volatile tail before token estimation and
in-flight compaction, then attaches the freshly assembled Developer message at
the tail. The stable typed prefix plus persisted history remains a contiguous
cacheable prefix; only the final volatile message changes.

### Provider-threaded Responses

Norn-owned volatile policy is not appended to `input`. It is projected to
top-level `instructions` on every request because `previous_response_id` does
not carry prior top-level instructions forward. The
[Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
describes this field as inserting a system or developer message, while the
[text guide](https://developers.openai.com/api/docs/guides/text#message-roles-and-instruction-following)
treats it as roughly equivalent to Developer input. Norn therefore records the
source authority separately rather than claiming the field has a literal role.

The stable non-System prompt seed, including current trusted prompt-command
output, is sent once when an anchor is created and is bound into V2
provider-state provenance. A stable Developer/User or prompt-command change
cuts the anchor and requires full replay. Replay is not guaranteed: when the
provider cannot safely replay the exact history, Norn fails with a typed error
before persisting the new prompt or dispatching the request. A Norn-owned
request-local policy change preserves the anchor and takes effect through
current instructions.

## Why the alternatives were rejected

1. Freezing the environment at session start would make mutable state appear
   current when it was not.
2. Moving only the timestamp would leave every other volatile section capable
   of invalidating the history cache.
3. Coarsening the timestamp would reduce frequency but retain the defect.
4. Flattening repository/operator content into System would improve neither
   cache behavior nor authority correctness and would violate D8 provenance.

The implemented design isolates the complete volatile class and gives each
transport an explicit compatible projection.

## Invariants and evidence

- Stable prompt fragments retain typed provenance through root, spawn, and
  fork assembly.
- A hot stable Developer/User change changes the seed, cuts a threaded anchor,
  and requires replay; an unreplayable history fails typed before the new
  prompt is persisted or sent. A stable System-only change does not cut the
  anchor.
- Prompt-command cache entries bind command name, exact command text, TTL, and
  working directory to a non-sliding absolute deadline. No TTL disables
  caching.
- Managed stateless context is absent during preflight and exists at most once
  at the request tail.
- Threaded Norn-owned runtime policy is present at most once in current
  top-level instructions and never becomes a durable provider input row.
- Threaded prompt-command output appears once as Developer seed material;
  unchanged output reuses the anchor and changed output cuts it.
- Runtime-dynamic tool descriptions have one authority path: the live tool
  definitions selected for that request.
- Durable sourced rules reconstruct exactly once with the same authority live
  and after resume.
- A readable old originless System-append row makes an unbound pre-D8 anchor
  ineligible, forcing one safe full replay before V2 threading resumes.

Deterministic source-to-wire tests bind these structural guarantees. The
reported adopter's credentialed before/after cache telemetry remains a separate
live A/B evidence item; this document does not claim that external experiment
has run.

## Key source references

- `crates/norn/src/loop/runner/prompt.rs`: per-request seed reconciliation,
  managed-context assembly, and transport projection.
- `crates/norn/src/loop/dev_context.rs`: stateless tail lifecycle.
- `crates/norn/src/loop/conversation_state/request_state.rs`: anchor/seed
  matching and threaded request slicing.
- `crates/norn/src/system_prompt/authority.rs`: source-derived authority.
- `crates/norn/src/system_prompt/plan.rs`: ordered typed stable prompt.
- `crates/norn/src/session/conversion.rs`: durable event reconstruction.
- `crates/norn/src/system_prompt/environment.rs`: current environment section.
