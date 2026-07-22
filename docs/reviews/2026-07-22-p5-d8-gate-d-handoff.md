# P5 D8 prompt-authority Gate D handoff

Date: 2026-07-22

Candidate branch: `codex/p5-d8-role-authority`

Requested verdict: `D8 READY as an implementation candidate`, or a precise
finding list that prevents that verdict.

Whole-P5 acceptance: explicitly out of scope.

## Exact review boundary

- Base: `05be10c40981460c00fed9acc306938ff93b40b2`
- Base tree: `266a3b5543c50cbe8d32dca9e6397f2fad86a3a4`
- Frozen source: `4fa6c6756ed497a002b4281f51cbb14f7bd7a3eb`
- Frozen source tree: `c0d9f69bb5283184432862016c1212644f7088c2`
- Source range: `05be10c40981460c00fed9acc306938ff93b40b2...4fa6c6756ed497a002b4281f51cbb14f7bd7a3eb`

The later branch commits are documentation and retained evidence only. The
policy artifact rejects dirty or untracked Rust, and `4fa6c67..HEAD` contained
zero Rust changes when the evidence was generated.

### Commit map

| Commit | Purpose |
| --- | --- |
| `fd1f2cf` | Preserve root instruction authority |
| `c286398` | Preserve Developer authority on compatible Chat |
| `0eab282` | Separate managed runtime and child authority |
| `56d0cb5` | Split CLI override configuration modules |
| `2183c99` | Reject misplaced role-policy options |
| `1b14662` | Retain profile authority provenance |
| `a456a21` | Close the untyped prompt-cache mutation surface |
| `83063fc` | Split builder and assembly modules without include/path indirection |
| `4166ae4` | Preserve skill authority provenance |
| `4fa6c67` | Complete D8 Responses authority, seed, setup, and durability behavior |

## Contract under review

Authority comes from provenance, never a filename, discovery position, content
label, settings precedence, or transport field:

| Authority | Sources |
| --- | --- |
| System | Compiled product/embedder policy, child/fork lifecycle policy, built-in variants, and compiled skill-use policy |
| Developer | Trusted operator profiles/overrides, `~/.norn/NORN.md`, operator rules/skills, and trusted prompt-command output |
| User | Project/nested `NORN.md`, workspace profiles/rules/skills, configured variants, human task/delegation/steering, and child output |

The canonical model is an ordered typed `PromptPlan`. Its flattened System
string remains only a compatibility/introspection view. Root, spawn, and fork
publish their actual typed plan through `ParentPromptPlan`; the legacy
`ParentSystemInstruction` is accepted only as an input bridge and becomes one
explicit embedder-System fragment. D8-built children never publish that
flattened bridge for another generation.

Sourced rules persist their origin. Operator rules reconstruct as Developer,
workspace rules as User, and readable originless pre-D8 rows reconstruct as
User. An originless row also prevents an unbound legacy provider anchor from
being silently inherited. Child results remain User input with neutral framing,
so their content cannot impersonate a higher role.

## Provider projection

For public provider-threaded Responses, current source-System fragments and
Norn-owned request policy use top-level `instructions` on every request. The
wire field has no literal role discriminator, and Norn retains the source roles
inside its typed plan. Stable Developer/User material is sent once as the
provider seed and bound into provider-state provenance.

Trusted prompt-command output is Developer seed material. The same value keeps
the anchor; a changed value makes it ineligible and requires exact replay.
Replay is not promised to succeed: missing encrypted reasoning or another
unreplayable history fails typed before the changed prompt is persisted or a
new provider request is dispatched. Source-System or Norn-owned request-policy
changes preserve the anchor and take effect through current instructions.

Stateless transports receive one trailing compatibility Developer message,
after persisted history, containing Norn-owned request policy followed by
trusted prompt-command output. Compatible Chat preserves Developer natively by
default and supports explicit reject or `downgrade_to_user` behavior for an
incapable server. Claude has no Developer channel: source-System alone reaches
`--system-prompt`, while Developer is explicitly lowered to ordinary positional
input. No compatibility path promotes Developer to System.

Runtime MCP descriptions remain only in the live tool definitions. The
Norn-owned hosted-tool policy may describe the surface but does not copy or
promote server-supplied descriptions into a prompt role.

The authority and seed path follows the provider-neutral request boundary.
Threaded managed requests validate the exact seed before persisting the prompt.
A stateless custom provider receives the finalized managed tail after preflight
by design; the handoff does not claim universal validation-before-persistence
for arbitrary embedder providers.

Official contract references:

- [Responses create reference](https://developers.openai.com/api/reference/resources/responses/methods/create)
- [Message roles and instruction following](https://developers.openai.com/api/docs/guides/text#message-roles-and-instruction-following)

## Prompt-command cache and request boundary

Prompt commands resolve once while preparing each request. A cache entry binds
the command map key, exact command text, configured positive TTL, and working
directory. Its deadline is computed from successful completion, is absolute,
and does not slide on cache hits. No TTL means no cache. If the clock cannot
represent the deadline, the fresh result is used for that request and is not
cached; no overflow panic is possible.

The typed stable plan is frozen before command execution. A command may rewrite
`NORN.md`, but that write cannot retroactively alter the request being assembled;
the following request observes the refreshed source. This makes each request a
single internally coherent authority snapshot.

Pre-cancelled and zero-iteration requests skip prompt-command execution and
provider dispatch. Affinity/provenance validation deliberately occurs first,
so these paths may validate or bind managed session identity and are not
described as universally free of session mutation.

## Setup timeout and input durability

There is still no timeout by default. When an embedder opts into
`step_timeout`, setup time is charged against that budget. Accepted prompt and
wake input is durably appended under cancellation shielding before the
provider/tool machine begins. Only the remaining budget wraps that machine; if
setup exhausts the budget there is no provider call and the step returns
`TimedOut` after the durable commit.

The provider/tool machine remains a hard cut. D8 adds no grace-period default,
polling cadence, or promise that a running tool completes after expiry.

After durable append, a proven prompt or wake message is removed from its outer
retry queue before cancellable notification hooks. Dropping the setup future at
that seam therefore cannot re-inject the same accepted message. This guarantee
depends on an agent ID plus pending/durable coordination. An embedder that
supplies neither coordination nor a pending-input store cannot durably requeue
an owned seed after an early pre-injection setup error; that boundary is
explicitly outside the exact-once claim.

## Retained source-bound evidence

| Artifact | SHA-256 | Result |
| --- | --- | --- |
| [`D8 inventory`](evidence/p5-d8/2026-07-22-p5-d8-inventory-4fa6c67.json) | `a82b3ade3cd0a3707c5474b11f3d4f591873c8a7dc0f20aea03daa4d6161d65e` | Exact NUL-safe inventory |
| [`D8 policy`](evidence/p5-d8/2026-07-22-p5-d8-policy-4fa6c67.json) | `424fa29c47a3e906283f9b6727df53a77a52ff65413fa241ee5df6a092332ca8` | Passed with zero production or split-bypass violations |

The inventory contains 205 paths and 9,809 exact NUL-delimited bytes, SHA-256
`72bc3637e393bda812c29e55cab426563386670081bd6771cdc58b5ab0428526`:

- 102 added and 103 modified paths;
- 42 production, 62 mixed production/test, 101 test-only, and zero documents;
- 204 Rust paths plus one test golden file; and
- no duplicate path.

The policy report records:

- 102 new Rust files, all below 500 physical lines; maximum 494;
- every changed AST-stripped production body below 500 code lines; maximum 453;
- zero added production `allow`/`expect`/`unwrap`/`panic` matches;
- zero added `include!` or `#[path]` split bypasses in any scope;
- zero module-shape or thin-entrypoint violations; and
- permitted test-only/cfg(test) matches retained and enumerated rather than
  suppressed: 5 lint attributes, 49 unwraps, 279 expects, and 31 panics.

Twenty-six touched files were already at or above 500 physical lines at the
exact base; twenty grew in this range. They are enumerated with base/head/delta
in the policy JSON. This is an honest pre-existing physical-file residual, not
a claim that every touched legacy test container was decomposed. No new file is
oversized, and no changed production body breaches the 500-code-line rule.

## Coordinator gates

All builds used the repository's normal `target` directory. No temporary target
or temporary worktree was used.

- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`:
  pass with no suppression.
- `cargo test --locked --workspace --all-targets --all-features --no-fail-fast`:
  pass, followed by a second complete compact sample; `5,684/5,684` in each.
- Principal harnesses in both samples: Norn `4,308/4,308`, CLI `522/522`, TUI
  `683/683`.
- `cargo test --locked --workspace --doc --all-features`: `8/8`.
- `cargo fmt --all -- --check`: pass.
- `git diff --check`: pass.
- Focused D8 filter: `15/15`, including setup-timeout and frozen-request cases.
- Prompt-command evaluation/cache filter: `9/9`.
- Direct durable-inbound cancellation regression: `1/1`.

The first full Norn source gate was run outside the filesystem/network sandbox
because Wiremock fixtures require local loopback binding; this is an environment
permission, not a product correction. The unrestricted Norn gate passed
`4,308/4,308`. No failing product test was relabelled as a pass.

## Audit corrections before freeze

The pre-freeze internal audit did not stop at the first green suite. It found
and corrected:

- cache entries that did not bind every definition/working-directory input;
- a hit path that could renew expiry instead of respecting one absolute
  deadline;
- an unrepresentable TTL path that could panic;
- setup timeout placement that could cut through accepted-input persistence;
- wake ownership transfer that happened after a cancellable notification hook;
- error paths that had lost ownership of initial input;
- a V1 bootstrap expectation that omitted the prompt-command Developer prefix;
  and
- the missing frozen-first-request regression for a command that rewrites its
  own context source.

Those findings are closed in frozen source `4fa6c67`. A final independent
internal correction audit returned `READY` for this focused source; that is not
a substitute for Gate D.

## Requested adversarial review

Please independently verify at least these seams:

1. Source authority survives root, spawn, fork, sourced-rule persistence,
   compaction/resume, and legacy reconstruction without escalation.
2. Public Responses instructions and seed slicing neither accumulate volatile
   policy nor omit current policy behind an anchor.
3. Changed Developer/User seed material cannot reuse an old anchor and cannot
   fabricate replay material.
4. Compatible Chat and Claude lowering never promotes Developer to System.
5. Runtime MCP prose has no prompt-authority path.
6. Cache identity and absolute expiry cannot reuse output across a changed
   command, TTL, or working directory, or panic on an extreme TTL.
7. Setup timeout and cancellation cannot lose or duplicate a proven prompt or
   wake message at the durable-append/notification seam.
8. The 205-path inventory and policy classifications reproduce from the exact
   base/source, including the 26-file legacy physical-LOC disclosure.

A cross-model adversarial seat is requested because it found the decisive
finding in three earlier P5 security-sensitive slices. Reviewers should rerun
the tests and inventory rather than trust this summary.

## Explicitly withheld

This handoff does not claim or request:

- whole-P5 acceptance;
- completion of the broad volatile-source, concurrent-agent, resume, or every
  anchor-reset matrix still unchecked in the remediation plan;
- authenticated D7/P9 real-wire conformance;
- WebSocket transport coverage;
- P2 OAuth phase acceptance;
- universal exact-once recovery for coordination-less embedders; or
- cleanup of every pre-existing physically oversized mixed/test file.

D8 can receive a focused implementation-candidate verdict without changing any
of those open statuses.
