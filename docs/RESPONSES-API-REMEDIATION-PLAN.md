# Responses API remediation plan

- **Status:** Draft for owner and team approval
- **Baseline:** `main` at `263cc4f466b3` on 2026-07-10
- **Scope:** OpenAI Responses, ChatGPT/Codex OAuth, prompt caching, streaming,
  conversation state, transport, schema, and usage behavior
- **Source review:**
[`reviews/2026-07-10-responses-api-implementation-review.md`](reviews/2026-07-10-responses-api-implementation-review.md)

## Purpose

This is the execution tracker for the findings in the source review. It is not a
second findings document. It turns those findings into ordered work, defines the
observable difference expected after every phase, and prevents a phase from
being called complete without reproducible evidence and independent review.

The checkboxes in this file are acceptance records. An implementation being
written, compiling, or having tests added is not enough to check a box. A phase
is complete only when its exit gate, evidence bundle, and review gate are all
complete. A required live test that does not run leaves its phase blocked.

## Target state

On completion:

- Codex OAuth credentials can reach only an explicitly trusted ChatGPT/Codex
  authority that repository-controlled configuration cannot expand.
- Norn stores and replays the ordered Responses item transcript rather than a
  lossy Chat-Completions-like reconstruction.
- Refusals, phases, hosted search, annotations, compaction, unknown items,
  `end_turn`, and turn-scoped Codex state have explicit semantics.
- Local conversation state and provider-side state cannot silently disagree.
- Cancellation owns the network producer, and HTTP and streamed failures use
  one retry/accounting model.
- Model, schema, structured-output, and tool-call controls are truthful from
  configuration through request, stream, persistence, replay, and UI.
- Prompt-cache policy is selected from measured GPT-5.5/GPT-5.6 behavior, with
  cache reads, writes, cost-relevant usage, and payload stability observable.
- Every changed production module meets the repository's lint and structure
  rules without suppressions or oversized-file exemptions.

## How this plan is operated

1. P0 ships first because it closes the critical credential-exfiltration path.
   After P1 establishes the campaign gates, the dependency declarations on each
   phase control ordering; P2 and P3 may proceed independently.
2. Every finding is owned by one phase in the traceability table. A finding may
   be supported by earlier foundation work, but it closes only in its owning
   phase.
3. A confirmed defect starts with a regression that demonstrates the reviewed
   failure on the baseline and passes after the production path is fixed. An
   unproven measurement or design tradeoff instead starts with a captured
   baseline, an owner-approved contract or experiment registered before the
   candidate is measured, a candidate conformance test, and independent
   reproduction. The plan never fabricates a red test for an unknown outcome.
4. Evidence is recorded in the ledger at the end of this document. Large or
   sensitive traces are stored as redacted artifacts and linked from the
   ledger; usable credentials and private prompt content are never retained.
5. The implementer cannot approve their own phase. Each phase gets a fresh
   rigorous Fable-model reviewer who did not implement it. That reviewer stays
   with the phase through its fix rounds, then a different reviewer handles the
   next phase.
6. A reviewer returns `READY` or `NOT READY`. Every review finding is fixed in
   the same phase and rechecked by the same reviewer. Nothing is accepted as a
   deferred follow-up.
7. Only after the final reviewer returns `READY` may the phase checkbox and its
   finding statuses be changed to complete.

## Non-negotiable delivery invariants

These are phase exit criteria, not recommendations.

### Correctness and evidence

- [ ] Every confirmed defect has a traceable chain:
  `finding -> failing baseline regression -> production fix -> passing candidate -> independent rerun`.
- [ ] Every unproven/design finding has a traceable chain:
  `finding -> captured baseline -> pre-registered decision/experiment -> candidate evidence -> independent reproduction`.
- [ ] Tests exercise the real request, stream, auth, persistence, or loop entry
  path. A parallel helper that production does not call is not closure evidence.
- [ ] No error is hidden through `.ok()`, a default value, an ignored `Result`,
  a dropped event, or an undocumented stale-value fallback.
- [ ] Missing provider data remains distinguishable from a provider-reported
  zero, empty value, or explicit success.
- [ ] No runtime timeout, retry count, cache lifetime, origin exception, token
  limit, or fallback default is invented. It comes from provider facts, model
  catalog data, existing owner-approved configuration, or a new owner decision
  recorded in `docs/DECISIONS-2026-07.md`.
- [ ] No runtime compatibility shim, duplicate v2 path, dual-write path,
  deprecated wrapper, or zombie implementation is introduced. The canonical
  path is replaced and every caller is updated in the same phase. If D2 selects
  migration, it is an offline one-shot operation, not a retained runtime reader.

### Strict lint policy

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes with no
  warning.
- [ ] Campaign work introduces no `#[allow(...)]`, `#[expect(...)]`,
  `#[deny(...)]`, `#[ignore]`, `#[cfg(any())]`, lint-silencing underscore
  rename, or workspace lint relaxation.
- [ ] Campaign work does not use Clippy command-line allowances, lowered lint
  levels, `RUSTFLAGS` suppression, dead-code hiding, or test exclusion to make a
  gate pass.
- [ ] Existing suppressions elsewhere in the repository are not precedent. No
  diff may add, widen, or move one. No changed production item may sit under a
  production suppression; existing test-module suppressions may not be widened.
- [ ] Tests that require an external service use an explicit runtime gate and
  report that they did not run. They are never marked `#[ignore]`.
- [ ] `TODO`, `FIXME`, `HACK`, `todo!`, and `unimplemented!` markers are absent
  from the completed phase diff.

This campaign deliberately adopts a stricter rule than the repository's narrow
test-only `#[allow]` exception: it adds no new lint suppressions in tests either.
If a test is hard to express cleanly, the test is refactored.

### File size and module structure

The 500-line rule means production Rust code, excluding comments, whitespace,
and syntax items compiled only under `#[cfg(test)]`. Whole-file physical line
counts and raw whole-file `tokei` totals are not sufficient because many current
files contain large inline test modules.

- [ ] No new production Rust file exceeds 500 production LOC.
- [ ] No production file at or below 500 LOC crosses the limit.
- [ ] Any phase that changes production behavior in a legacy over-limit file
  finishes with that file at or below 500 production LOC.
- [ ] A legacy over-limit file is decomposed into cohesive named modules before
  or with its behavioral change. It is not grandfathered because it was already
  large.
- [ ] Untouched legacy violations are listed individually in the Phase 1
  baseline with an owner and remediation record. Responses-area violations are
  assigned to a phase here; unrelated debt is assigned outside this program and
  may not grow.
- [ ] `mod.rs` contains declarations and re-exports only. `lib.rs` and `main.rs`
  remain thin entry points and stay below the configured 200 production-LOC
  limit.
- [ ] Test code is not moved behind `#[cfg(test)]` to disguise production LOC.
- [ ] Module splitting preserves behavior and is reviewed separately from the
  behavioral fix where practical.

`CONVENTIONS.toml` currently reports the 500-LOC and bypass rules as advisory
and counts whole files differently from the policy above. Phase 1 must replace
the disagreement with one shared, failing, syntax-aware implementation used by
post-mutation checks and the protected merge gate. Advisory output alone is
never acceptance evidence for this program.

## Universal phase gate

Every phase must satisfy all four gates below.

### Gate A: entry and design

- [ ] All dependency phases are complete.
- [ ] The phase's owner decisions are recorded before implementation.
- [ ] Its finding IDs, invariants, production touch points, and defect-regression
  or measurement/design evidence method are agreed by the implementer and
  domain reviewer.
- [ ] Any live credential use, external call, or billable experiment has
  separate owner approval before it runs.

### Gate B: implementation

- [ ] Confirmed-defect regressions fail for the documented reason on the reviewed
  baseline; measurement/design work has its pre-registered baseline and contract.
- [ ] The production fix is complete across request, stream, persistence,
  replay, loop behavior, and user surface wherever the capability crosses them.
- [ ] Replaced paths and temporary scaffolding are deleted in the same phase.
- [ ] Changed production files satisfy the file-size and module-structure rules.
- [ ] The finding-to-test traceability table is updated.

### Gate C: machine verification

- [ ] Phase-specific tests pass.
- [ ] `cargo fmt --all --check` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace --all-targets` passes.
- [ ] `cargo test --workspace --doc` passes.
- [ ] `git diff --check` passes.
- [ ] For P0, reviewer-verified production LOC and bypass inspection covers every
  changed Rust item. From P1 onward, the syntax-aware repository policy command
  passes as a hard failure in the protected merge check.
- [ ] For P0, security reviewers manually inspect all fixtures/evidence for
  secrets. From P1 onward, the checked-in evidence-redaction validator passes;
  no credential, real account identifier, private prompt content, reusable turn
  state, or raw cache key is present.

### Gate D: independent review

- [ ] The domain reviewer inspects the implementation, tests, and raw evidence.
- [ ] A fresh rigorous Fable-model reviewer receives the source review, this plan,
  the diff, relevant official-source revision, and evidence bundle.
- [ ] Reviewers rerun the relevant tests and policy gates rather than trusting
  pasted output.
- [ ] All review findings are fixed and the same reviewer verifies the fix round.
- [ ] Final verdict is `READY`, with no unresolved item at any severity.
- [ ] Commit, commands, test counts, policy report, reviewer, date, and verdict
  are entered in the evidence ledger.

## Phase roadmap

| Phase | Status | Primary outcome |
|---|---|---|
| P0. Credential destination containment | [ ] | Repository config cannot redirect Codex OAuth credentials. |
| P1. Contract and enforcement baseline | [ ] | The program has executable contracts and protected quality gates. |
| P2. OAuth lifecycle correctness | [ ] | Login, refresh, storage, and logout fail safely and explicitly. |
| P3. Canonical ordered transcript | [ ] | Responses items survive stream, persistence, resume, and replay in order. |
| P4. Streaming and replay conformance | [ ] | Supported events/items are complete, reconciled, and fail closed. |
| P5. Conversation and Codex turn semantics | [ ] | Local/provider history and turn-scoped state have explicit lifetimes. |
| P6. Transport, retry, and usage | [ ] | Retries terminate once; observed and unknown attempt usage remain explicit. |
| P7. Request, schema, and model controls | [ ] | Advertised capabilities match validated payload and tool behavior. |
| P8. Prompt-cache measurement and policy | [ ] | Cache policy is observable, backend-specific, and empirically justified. |
| P9. Integrated conformance and release review | [ ] | All findings are closed together with reproducible end-to-end evidence. |

## Finding ownership

| Finding IDs | Closure owner | Foundation/dependency |
|---|---|---|
| `SEC-01`, `BACKEND-01` | P0 | Existing request/auth path |
| `AUTH-01`, `AUTH-02`, `AUTH-03`, `AUTH-04`, `AUTH-05` | P2 | P0-P1 |
| `STATE-01` | P4 | P3 canonical transcript |
| `EVT-01`, `EVT-02`, `EVT-03`, `EVT-04`, `EVT-05` | P4 | P3 canonical transcript |
| `STATE-02`, `CODEX-01`, `CODEX-02`, `TRANS-01` | P5 | P3-P4 |
| `TRANS-02`, `USAGE-01` | P6 | P5 turn-owned transport |
| `MODEL-01`, `SCHEMA-01`, `STRUCT-01` | P7 | P0, P3-P4, P6 |
| `CACHE-01`, `CACHE-02`, `CACHE-03`, `CACHE-04`, `CACHE-05` | P8 | P4, P6 accounting, P7 request/tool stability |
| Integrated closure and the two retained transport regressions | P9 | P0-P8 |

## Owner decision register

The relevant phase cannot pass Gate A while its decision is open.

| ID | Required decision | Due | Status / record |
|---|---|---|---|
| D0 | CI/merge-gate platform and required-check wiring. P1 remains blocked until a checked-in clean-checkout gate is protected. | P1 | [ ] Open |
| D1 | Exact compiled OAuth authority/path allowlist and redirect policy. Custom trusted-proxy support is out of P0 scope and requires a later provenance-preserving security design. | P0 | [ ] Open |
| D2 | Existing session policy: explicit version rejection or an offline one-shot migration. Record format versioning, crash atomicity, idempotency, backup/recovery, old-binary behavior, and treatment of irrecoverably lossy history. | P3 | [ ] Open |
| D3 | Replaceable Developer-context policy for threaded Responses: reset the anchor, use a genuinely replaceable surface, or disable threading. | P5 | [ ] Open |
| D4 | Single retry owner and existing configured attempt/budget semantics for HTTP and in-stream failures. | P6 | [ ] Open |
| D5 | Native `text.format` versus synthetic tool policy by API shape, including the intentional control-flow cases for the latter. | P7 | [ ] Open |
| D6 | Pre-register the cache experiment: ratify or replace the proposed 20-iteration design; approve models, spending, warm-up, key isolation/reuse, request rate, concurrency, retention/cooldown, service tier, output/effort controls, randomization, primary measures, and statistical treatment. | P8 | [ ] Open |
| D7 | Approve credentials, spending, redaction, and retention for final live Codex and public Responses conformance. Without approval P9 is blocked, not passed by a skipped test. | P9 | [ ] Open |

## Immediate operator mitigation

Until P0 ships, do not use Codex OAuth from a repository that has not been
audited for `.norn/settings.json`, provider overrides, and `base_url`. Do not use
Codex OAuth with any custom Responses endpoint. Use an explicit API-key or
unauthenticated compatible-provider profile for custom endpoints. This reduces
exposure but does not close `SEC-01`.

## P0. Credential destination containment

**Status:** [ ] Not started; **findings closed:** `SEC-01`, `BACKEND-01`;
**dependencies:** D1 only.

### What this phase fixes

Project-controlled `.norn/settings.json` can currently supply `base_url` while
the provider still selects OAuth, causing bearer and account headers to be sent
to that origin. Backend identity is also inferred from the absence of a URL
override, so spelling the canonical ChatGPT URL changes state semantics.

### Difference after the phase

Credential authority, deployment/backend identity, and endpoint selection are
separate typed concepts. Codex OAuth headers are attached only to the compiled,
normalized HTTPS authority/path approved in D1. A cloned repository cannot
extend that trust boundary. An explicitly selected canonical endpoint retains
Codex backend semantics rather than silently becoming public Responses.

### Work checklist

- [ ] Introduce an explicit deployment/backend identity used by capability,
  service-tier, state, auth, and endpoint resolution.
- [ ] Normalize and validate scheme, authority, port, path, and userinfo before
  any credential-bearing request is constructed.
- [ ] Disable automatic redirects on credential-bearing clients. Do not forward
  OAuth headers to any redirect target.
- [ ] Reject OAuth plus an untrusted endpoint before opening a connection.
- [ ] Ensure repository and CLI provider overrides cannot grant OAuth trust.
- [ ] Keep arbitrary compatible endpoints on an explicit non-OAuth path. A
  trusted-proxy feature is separate future security work, not P0 closure scope.
- [ ] Ensure debug output and errors never reveal bearer tokens or account IDs.
- [ ] Put new security logic in cohesive modules below 500 production LOC. If a
  changed legacy file is over the limit, bring it below the limit in this phase.

### Phase-specific evidence

- [ ] A hostile local endpoint receives no request when selected from project
  configuration, and a hostile redirect target receives no redirected request.
- [ ] URL tests cover HTTP, userinfo, case, trailing dots, default/non-default
  ports, lookalike hosts, path variants, redirects, and canonical URLs.
- [ ] Capability/payload snapshots prove explicit and implicit canonical Codex
  selection have identical backend semantics.
- [ ] API-key and unauthenticated compatible endpoints retain their intended,
  explicitly tested behavior.

### Review and exit gate

- [ ] A security reviewer threat-models every credential destination and redirect.
- [ ] A provider/config reviewer verifies trust cannot originate in project data.
- [ ] A Fable adversarial reviewer returns `READY` before this phase ships
  independently of later protocol work.
- [ ] Existing fmt, strict Clippy, workspace tests, doc tests, diff check, and a
  reviewer-verified syntax-aware LOC/bypass inspection pass. P0 does not wait for
  the broader P1 policy infrastructure.
- [ ] Universal Gates A-D pass and `SEC-01`/`BACKEND-01` have closure evidence.

## P1. Contract and enforcement baseline

**Status:** [ ] Not started; **findings supported:** all; **dependencies:** P0
and D0.

### What this phase fixes

The repository currently has strong written rules but no checked-in protected
CI/merge gate and only advisory post-mutation LOC/bypass reporting. The existing
whole-file LOC and blanket-allow checks also disagree with `CLAUDE.md` on inline
test code. The review spans two dialects: public Responses and the
ChatGPT/Codex backend. Without executable fixtures and one hard policy
implementation, later fixes can test the wrong contract or pass conflicting
quality checks.

### Difference after the phase

The team has one approved backend/state matrix, a sanitized fixture corpus, a
finding-to-evidence map, a shared syntax-aware production policy checker, and a
protected clean-checkout merge gate. Existing post-mutation checks use the same
policy semantics. Later phases fail mechanically when they add/worsen debt or
violate campaign rules. No provider behavior changes in this phase.

### Work checklist

- [ ] Ratify this plan and the source review's finding IDs and severity.
- [ ] Record the public Responses documentation revision and official Codex
  source commit used as the conformance contract.
- [ ] Build sanitized fixtures for text, multiple assistant phases, encrypted
  reasoning, function/custom calls, refusal, hosted search and annotations,
  compaction, unknown items, `end_turn`, turn-state headers/metadata, failures,
  rate limits, incomplete streams, and cache usage.
- [ ] Add a traceability record mapping each confirmed defect to a regression
  and each unproven/design finding to its baseline and pre-registered contract.
- [ ] Add one syntax-aware policy implementation used by both post-mutation
  checks and a repository command that fails on LOC, entrypoint/module shape,
  bypass, ignored-test, and unresolved-marker violations.
- [ ] Update or replace the contradictory `CONVENTIONS.toml` LOC/bypass path so
  there are not two authorities with different test-code semantics.
- [ ] Add policy fixtures, including inline `#[cfg(test)]` code, changed
  over-limit files, new/worsened debt, suppressions, and logic-bearing `mod.rs`.
- [ ] Create an explicit legacy baseline containing file, verified production
  LOC, owner, due phase/remediation record, and baseline identity. The checker
  fails on a new entry, growth, production edits, or an overdue entry. A touched
  entry is removed only after the file is at or below 500 production LOC.
- [ ] Add a checked-in evidence-redaction validator for fixtures and evidence.
- [ ] Wire every Gate C command, the policy checker, and the redaction validator
  into the D0-selected protected merge check running from a clean checkout.
- [ ] Record toolchain, baseline commit, test counts, exact gate commands, and
  full verification results.

### Phase-specific evidence

- [ ] Every source-review finding has exactly one closure owner and the correct
  confirmed-defect or measurement/design evidence class.
- [ ] Redaction-validator negative fixtures prove credentials, account IDs,
  private prompt text, reusable turn state, and raw cache keys fail the gate.
- [ ] Policy-checker tests prove violations return non-zero, inline test items
  are excluded from production LOC, and an unchanged baseline entry may remain
  only until its recorded due phase.
- [ ] The checker catches every prohibited bypass form and a logic-bearing
  `mod.rs` fixture, and post-mutation feedback reports the same result.
- [ ] The protected check runs all Gate C commands on a clean checkout and is
  required rather than advisory.
- [ ] Baseline commands pass. A pre-existing failure is fixed, not waived.

### Review and exit gate

- [ ] Security/auth, request/state, and streaming/item domain reviewers approve
  the fixture coverage for their original review areas.
- [ ] A fresh Fable architecture reviewer returns `READY` on the contract,
  shared policy implementation, protected check, and phase ordering.
- [ ] Universal Gates A-D pass and P1 evidence is recorded.

## P2. OAuth lifecycle correctness

**Status:** [ ] Not started; **findings closed:** `AUTH-01` through `AUTH-05`;
**dependencies:** P1.

### What this phase fixes

Norn's login fallback reads the account ID from the wrong JWT shape, refresh is
single-flight only inside one process, credential load and proactive refresh
errors are hidden, browser success precedes durable save, and revoke failure
prevents local deletion.

### Difference after the phase

Norn-created and imported Codex credentials produce the required account
metadata. Multiple processes converge on one rotating credential transaction.
Corrupt storage and refresh failures remain typed and visible. Browser success
means the credential is saved, and logout always removes the local credential
while reporting remote revoke separately.

### Work checklist

- [ ] Parse the namespaced Codex auth claim and retain only provider-shape
  compatibility that is justified by sanitized fixtures.
- [ ] Implement an interprocess reload-lock-refresh-save transaction with
  atomic durable storage and explicit lock failure behavior.
- [ ] Preserve typed load, parse, refresh, persistence, and permission errors.
- [ ] Define and test any stale-token use explicitly; do not silently fall back.
- [ ] Delay browser completion until exchange and durable save succeed.
- [ ] Always clear local credentials during logout and report remote revocation
  as an independent result.

### Phase-specific evidence

- [ ] Redacted real-shape JWT fixtures cover namespaced and supported fallback
  claim sources without storing a usable token.
- [ ] Two independent processes sharing a rotating refresh token perform one
  effective authority exchange and converge on the same stored credential.
- [ ] Corrupt, unreadable, partially written, and permission-denied storage
  cases surface the correct typed error.
- [ ] Browser exchange/save failures cannot render a final success page.
- [ ] Revoke network/authority failure still deletes the local credential.

### Review and exit gate

- [ ] Security/auth and concurrency/persistence reviewers independently approve.
- [ ] A Fable adversarial reviewer returns `READY` after attacking failure paths.
- [ ] Universal Gates A-D pass and `AUTH-01` through `AUTH-05` are closed.

## P3. Canonical ordered transcript

**Status:** [ ] Not started; **foundation for:** `STATE-01`, `EVT-02`;
**dependencies:** P0-P1 and D2. P2 may proceed independently.

### What this phase fixes

Norn currently reduces a Responses turn to grouped reasoning, one text string,
and local calls, then reconstructs a different sequence. Session persistence has
no lossless place for message phase, multiple message boundaries, annotations,
hosted calls, refusal, compaction, or unknown output items.

### Difference after the phase

An ordered Responses item vector is the canonical provider transcript. Display
text, reasoning UI, local executable calls, and stop behavior are projections.
Persistence, reload, and stateless replay operate on the canonical items and do
not change their order or invent missing semantics.

### Work checklist

- [ ] Use the P1 production-LOC baseline to identify touched over-limit request,
  assembly, and session-event files; decompose each identified file into
  cohesive named modules before adding behavior.
- [ ] Introduce a canonical item union with typed core variants and an opaque raw
  variant for unknown items.
- [ ] Preserve replayable item order, message phase, call IDs, encrypted
  reasoning, refusal, annotations, hosted search, and compaction data.
- [ ] Store stream provenance such as item ID, output index, and content index
  separately from replayable provider item JSON. Envelope coordinates must never
  leak into the next request unless the provider item schema owns that field.
- [ ] Make normalized text, reasoning, calls, and stop reason derived views only.
- [ ] Version the new persisted format and replace the flat representation under
  D2. A rejecting runtime must fail before mutation. An approved migration must
  be offline, atomic, idempotent, recoverable from interruption, backup-aware,
  and absent from the normal runtime read path.
- [ ] Define the minimal documented normalization allowlist for server-only
  fields rejected on replay; preserve everything else.
- [ ] Update uninterrupted, persisted, resumed, spawned, and forked session paths.

### Phase-specific evidence

- [ ] Golden serialize-deserialize-serialize tests preserve item count, type,
  order, phase, IDs, content, annotations, and opaque JSON while keeping stream
  provenance out of replay serialization.
- [ ] A sequence containing reasoning, commentary, call, further reasoning, and
  final answer remains in that exact order after persistence and resume.
- [ ] The second `store:false` request replays the preceding `response.output`
  sequence exactly except for the approved normalization allowlist.
- [ ] Unknown items round-trip opaquely without becoming executable by accident.
- [ ] Existing-session tests cover format versioning, interrupted/repeated
  migration where selected, backup/recovery, old-binary behavior, rejection
  before mutation, and honest handling of phase/order that cannot be recovered.

### Review and exit gate

- [ ] Responses-protocol and session/persistence reviewers approve the model and
  migration/rejection behavior.
- [ ] A Fable adversarial reviewer compares raw fixtures with persisted and
  replayed forms and returns `READY`.
- [ ] Universal Gates A-D pass. `STATE-01` remains open until P4 proves streaming.

## P4. Streaming and replay conformance

**Status:** [ ] Not started; **findings closed:** `STATE-01`, `EVT-01` through
`EVT-05`; **dependencies:** P3.

### What this phase fixes

The SSE mapper drops item identity and phase, ignores refusal and hosted-search
provenance, cannot repair missing deltas from authoritative completion data, and
silently skips unknown output items. A refusal can therefore become an
ordinary empty success.

### Difference after the phase

Completed output items drive the canonical transcript. Deltas drive responsive
UI keyed by item/content identity and reconcile against authoritative completed
content. Refusal is a non-retryable model outcome distinct from transport error,
hosted search survives replay, and an unknown output item cannot be reported as
ordinary success.

### Work checklist

- [ ] Use the P1 production-LOC baseline to decide whether the SSE production
  implementation requires decomposition. Split it by parser, item assembly,
  reconciliation, and terminal mapping where required by LOC or cohesion.
- [ ] Maintain separate checked manifests for the pinned public taxonomy
  (52 stream events and 28 output-item variants at review time) and Codex-specific
  events/items/headers. Each name is handled, allowlisted lifecycle-only, or
  typed unsupported, and a taxonomy change fails the manifest check.
- [ ] Key deltas by item ID, output index, and content index.
- [ ] Reconcile deltas with `.done` and completed item data; repair or emit a
  typed mismatch instead of silently truncating.
- [ ] Preserve refusal, message phase, annotations/citations, hosted-search
  actions/sources, reasoning, compaction, and multiple message boundaries.
- [ ] Represent refusal as a non-retryable model outcome, not provider failure;
  preserve canonical refusal content and usage through persistence and replay.
- [ ] Preserve every unknown output item raw and return a typed
  `UnsupportedResponseItem` rather than ordinary success unless its exact type
  is on the pinned inert/lifecycle allowlist. Apply the same rule to an unknown
  event not on the pinned lifecycle allowlist.
- [ ] Make parser/mapper terminal delivery exactly once and reject/ignore later
  frames deterministically. P6 separately owns stopping the network producer.

### Phase-specific evidence

- [ ] Pure refusal, mixed content/refusal, tool-loop refusal, structured-output
  refusal, and resumed refusal fixtures preserve usage and cannot retry or
  become empty success.
- [ ] Hosted-search action, sources, annotations, and answer survive a tool-loop
  continuation and a persisted resume.
- [ ] Missing/malformed delta fixtures are repaired by authoritative completion
  data or fail with a typed diagnostic.
- [ ] Tests cover arbitrary SSE chunk boundaries, CRLF, multiline data,
  duplicate/out-of-order frames, incomplete EOF, and post-terminal input.
- [ ] Public and Codex manifest tests fail when a variant is added, removed, or
  loses classification.
- [ ] An unknown output item and unknown non-allowlisted event preserve raw data
  and terminate with the exact typed unsupported outcome.

### Review and exit gate

- [ ] Streaming/item and UI/session reviewers independently inspect behavior.
- [ ] The streaming/item review-domain owner confirms every immediate capability
  is preserved end to end.
- [ ] A Fable adversarial reviewer returns `READY` on raw-wire fixtures.
- [ ] Universal Gates A-D pass and `STATE-01`/`EVT-01` through `EVT-05` close.

## P5. Conversation and Codex turn semantics

**Status:** [ ] Not started; **findings closed:** `STATE-02`, `CODEX-01`,
`CODEX-02`, `TRANS-01`; **dependencies:** P3-P4 and D3.

### What this phase fixes

Local removal of managed Developer context does not remove it from a
`previous_response_id` chain. Norn also ignores the backend's explicit
`end_turn` decision and `x-codex-turn-state`, so continuation and sticky-routing
semantics are inferred or omitted.

### Difference after the phase

Each backend has an explicit state strategy. Replaceable dynamic context cannot
accumulate invisibly in a provider thread. Top-level instructions are resent
where required. `end_turn` controls continuation explicitly, and Codex turn
state is reused only inside its owning user turn, never across turns, agents, or
sessions. Dropping/cancelling the turn also owns and terminates its HTTP producer,
so sticky state cannot outlive the turn through an orphaned task.

### Work checklist

- [ ] Implement D3 consistently across loop state, provider capabilities,
  compaction, persistence, resume, and request construction.
- [ ] Keep ChatGPT/Codex `store:false` replay distinct from public Responses
  threading and prove both wire shapes.
- [ ] Resend top-level instructions with a response anchor while ensuring
  replaceable Developer context has the intended effective lifetime.
- [ ] Preserve `response.completed.response.end_turn` as typed terminal metadata
  and define its interaction with calls, refusals, and no-output continuation.
- [ ] Capture `x-codex-turn-state` from headers/metadata, replay it within the
  same turn, and clear it at the turn boundary.
- [ ] Make the returned stream/turn session own the HTTP producer and cancel all
  header, body, and SSE waits on receiver drop, user cancellation, or timeout.
- [ ] Reset or invalidate anchors whenever local compaction/state replacement
  makes provider history semantically incompatible.

### Phase-specific evidence

- [ ] A two-turn threaded test proves the second request cannot see the first
  request's replaced environment, rules, mode, timestamp, or prompt-command data.
- [ ] Stateless and threaded tests prove their intended effective context and
  top-level instruction behavior.
- [ ] `end_turn:Some(false)`, `Some(true)`, and `None` each have explicit tested
  loop semantics.
- [ ] Concurrent-agent and resume tests prove turn state never crosses user-turn,
  agent, session, cancellation, or error boundaries.
- [ ] A conflicting second turn-state value follows a documented tested rule
  rather than silently replacing or leaking the first value.
- [ ] A controllable local server promptly observes producer/socket termination on
  receiver drop, cancellation, and timeout, with no surviving task.
- [ ] Compaction and anchor-reset tests show local and provider-visible history
  remain semantically aligned.

### Review and exit gate

- [ ] Prompt/state and Codex-backend reviewers approve authority and lifetime rules.
- [ ] A Fable state-machine reviewer returns `READY` after multi-turn adversarial tests.
- [ ] Universal Gates A-D pass and all four findings close.

## P6. Transport, retry, and usage

**Status:** [ ] Not started; **findings closed:** `TRANS-02`, `USAGE-01`;
**foundation for:** `CACHE-02`; **dependencies:** P5 and D4.

### What this phase fixes

HTTP and in-stream rate limits have different retry paths, mapped errors do not
immediately stop the producer, and failed/missing/cache-write usage is discarded
or collapsed to zero. P5 has already made cancellation own the producer; this
phase makes retry, terminal network shutdown, and attempt accounting consistent.

### Difference after the phase

One component owns retry budget and attempt classification regardless of whether
failure arrives as an HTTP status or SSE event. Every attempt has one terminal
outcome. Observed provider usage distinguishes absent, zero, successful, failed,
cancelled, cache-read, and cache-write values; attempts without terminal usage
remain explicitly unknown rather than being called billed or estimated silently.

### Work checklist

- [ ] Use the P1 baseline to decompose each touched over-limit
  transport/executor file before adding behavior.
- [ ] Centralize retry ownership and carry explicit attempt/budget metadata.
- [ ] Apply the same classified policy to HTTP 429/5xx and in-stream failures.
- [ ] Stop the producer immediately after delivering any mapped terminal error.
- [ ] Preserve attempt-level usage for failed, incomplete, cancelled, and retried
  requests. Report observed totals, unknown-attempt counts, and estimates as
  separate concepts; call usage billed only when provider/billing evidence says so.
- [ ] Parse cache-read and cache-write field presence and values without defaulting
  absent fields to zero.
- [ ] Retain regression coverage for retryable overload/slow-down and clean EOF
  without a terminal event.

### Phase-specific evidence

- [ ] A controllable local server promptly observes task/socket termination after
  a mapped terminal error; P5 cancellation tests remain green.
- [ ] No producer task remains after a mapped terminal event.
- [ ] HTTP 429 and streamed `rate_limit_exceeded` consume the configured retry
  budget exactly once and honor provider `Retry-After` where applicable.
- [ ] Fault injection covers 401 refresh, 429, overload, slow-down, 5xx, reset,
  stall, malformed event, clean EOF, and duplicate terminal input.
- [ ] Usage fixtures prove unknown differs from zero, aggregate observed totals
  include cache writes and reported failures, and unreported attempts remain in
  a separate unknown count. Billing reconciliation is evidence only when
  authority data is available.

### Review and exit gate

- [ ] Async/concurrency, reliability, and usage/accounting reviewers approve.
- [ ] A Fable adversarial reviewer inspects terminal races, retry ownership, and
  usage claims and returns `READY`.
- [ ] Universal Gates A-D pass and transport/usage findings close.

## P7. Request, schema, and model controls

**Status:** [ ] Not started; **findings closed:** `MODEL-01`, `SCHEMA-01`,
`STRUCT-01`; **dependencies:** P0-P1, P4, P6, and D5. P2 is independent.

### What this phase fixes

Request construction overrides catalog reasoning-summary defaults, advertises
parallel capability while forcing serial calls, can lower schemas into dangling
`$ref` values, and implements structured output only through a synthetic tool.
More generally, advertised capability is not consistently tied to complete
request/stream/persistence/replay support.

### Difference after the phase

One immutable request profile is resolved from backend, API shape, and model
before serialization. Unsupported combinations fail locally. Schema lowering is
validated and never emits dangling references. Parallel calls are enabled only
after ID-based correlation is proven. Structured output follows D5 by API shape
rather than an accidental universal workaround.

### Work checklist

- [ ] Use the P1 baseline to decompose any touched over-limit
  builder/config/request file before adding behavior.
- [ ] Resolve backend/model defaults and capabilities once before payload assembly.
- [ ] Honor catalog reasoning-summary defaults and represent Norn-disabled
  parallelism separately from provider capability.
- [ ] Correlate all call completion by stable item/call identity before enabling
  parallel tool calls for a capable model.
- [ ] Preserve or inline required `$defs` during schema lowering and validate the
  result locally; reject unlowerable schemas with a typed diagnostic.
- [ ] Implement D5 for native `text.format` and intentionally synthetic-tool cases.
- [ ] Reject raw provider-option collisions with Norn-owned typed fields.
- [ ] Advertise a hosted/native capability only when its request, stream,
  persistence, replay, and user-surface path is complete.

### Phase-specific evidence

- [ ] Payload snapshots cover Codex subscription, public Responses, trusted
  configured backends, reasoning/non-reasoning models, service tiers, and tools.
- [ ] `$defs`, nested `$ref`, union, unsupported, and adversarial schemas either
  produce valid deterministic output or a typed local rejection.
- [ ] Randomized/interleaved parallel calls correlate every output by ID.
- [ ] Native and synthetic structured-output cases have explicit, tested selection
  and equivalent schema/error guarantees where both are supported.
- [ ] Unsupported capability and raw-option collision tests fail before network I/O.

### Review and exit gate

- [ ] Provider/catalog, JSON Schema, and tool-protocol reviewers approve.
- [ ] A Fable API-shape reviewer compares payloads with pinned official schemas
  and returns `READY`.
- [ ] Universal Gates A-D pass and all three findings close.

## P8. Prompt-cache measurement and policy

**Status:** [ ] Not started; **findings closed:** `CACHE-01` through `CACHE-05`;
**dependencies:** P4, P6, P7, and D6.

### What this phase fixes

The July 9 tail-placement change fixes the known GPT-5.5 placement defect, but
its GPT-5.6 benefit is unproven. Cache-write usage is currently invisible,
stable cache keys are not universal, tool descriptions can mutate the cached
prefix, and current typed cache controls cannot represent newer breakpoint
behavior.

### Difference after the phase

Every agent path has an intentional namespaced cache-key lifetime. Instructions,
tools, and ordered input items use experiment-scoped keyed pseudonyms rather
than durable raw hashes. Cache reads, writes, and attempt usage are observable.
Cache controls are gated by the exact backend/model contract, and the selected
GPT-5.5/GPT-5.6 policy is supported by reproducible cost/latency evidence rather
than inference.

### Work checklist

- [ ] Assign stable runtime/thread-derived keys to persistent, ephemeral,
  `--no-session`, spawned, and forked agent paths.
- [ ] Define key namespace, root/child/fork uniqueness and inheritance, lifetime,
  collision resistance, rotation, and provider per-key traffic behavior.
- [ ] Resolve session-stable tool definitions once or explicitly classify and
  diagnose variables that are permitted to change them.
- [ ] Record domain-separated keyed MACs for instructions, serialized tool
  surface, ordered input-item sequence, and cache key, using a non-logged
  experiment/session key with explicit retention. Never fingerprint credentials,
  account IDs, private turn-state values, or other reusable secrets.
- [ ] Carry cache-read/write field presence and values through stream, session,
  attempt aggregation, and user/debug reporting.
- [ ] Add typed, capability-gated cache options and content breakpoints only for
  backend/model pairs proven to accept and benefit from them.
- [ ] Never send public-only cache controls to the private Codex backend merely
  because the public Responses schema supports them.
- [ ] Run the D6-approved matrix comparing current implicit tail, no dynamic
  message, stable Developer message, and explicit stable breakpoint where
  accepted, separately for reasoning, tools, hosted search, and variable-expanded
  tool descriptions.
- [ ] Treat `no dynamic message` as a diagnostic control, not a policy candidate,
  because it changes request semantics.
- [ ] Counterbalance/randomize the approved runs and hold warm-up, key isolation,
  request rate, concurrency, retention/cooldown, service tier, output limit,
  reasoning effort, and tool workload to the D6 protocol.

### Phase-specific evidence

- [ ] Offline payload tests prove byte-stable instructions, tool ordering/schema,
  item ordering, and key selection when inputs are stable.
- [ ] Persistent, ephemeral, spawned, and forked executions prove intended
  uniqueness, inheritance, collision resistance, and lifetime without leaking a
  raw key or a durable low-entropy hash.
- [ ] The live experiment records backend/model, request number, keyed
  pseudonyms, ordered item types, input/output tokens, cached reads, cache-write
  presence/value, first-event latency, completion latency, and request ID.
- [ ] Pre-registered primary measures include cached-prefix growth, cache
  read/write ratio, cost-relevant observed tokens, unknown-attempt count, first-
  event/completion latency distributions, and request success/failure.
- [ ] Raw provider usage reconciles with Norn telemetry and cost-relevant
  observed totals; estimates and unknown billing remain separately labelled.
- [ ] The performance reviewer can reproduce every reported curve from redacted
  raw rows and explain every deliberate invalidation.
- [ ] The final policy, including retaining implicit behavior if that wins, is
  recorded as an owner decision with no post-hoc success threshold.
- [ ] If live approval, credentials, or spending capacity is unavailable, P8 is
  marked `BLOCKED`; a runtime-skipped experiment cannot close a cache finding.

### Review and exit gate

- [ ] Request/state and cache/performance reviewers approve the offline and live
  evidence independently of the implementer.
- [ ] A Fable performance reviewer reproduces the analysis and returns `READY`.
- [ ] Universal Gates A-D pass and `CACHE-01` through `CACHE-05` close.

## P9. Integrated conformance and release review

**Status:** [ ] Not started; **findings closed:** all remaining integrated risk;
**dependencies:** P0-P8 and D7.

### What this phase fixes

Individually correct units can still disagree at their seams. The final phase
attacks the complete system, verifies every advertised capability end to end,
updates operational documentation, and proves that fixes did not regress the
two transport behaviors already corrected at the review baseline.

### Difference after the phase

The team has reproducible synthetic and approved real-wire evidence for auth,
request formation, streaming, persistence, replay, state, cancellation, retry,
usage, schemas, tools, and caching. Every review finding has a closure record,
unsupported capabilities remain unadvertised, and release approval does not
depend on implementer assertion or terminal history.

### Work checklist

- [ ] Run the full conformance corpus against ChatGPT/Codex and public Responses
  with the credentials, spending, redaction, and retention approved in D7.
- [ ] Exercise text, reasoning, local tool loops, refusal, hosted search,
  compaction, cancellation, failure/retry, persistence, resume, spawn, and fork.
- [ ] Verify CLI, TUI, and library surfaces report typed outcomes and usage
  consistently.
- [ ] Recheck every capability from configuration/catalog through payload,
  stream, persistence, replay, and user surface.
- [ ] Re-run overload/slow-down retryability and incomplete-EOF regressions.
- [ ] Update provider/backend, auth, session, usage, and cache documentation.
- [ ] Reconcile this plan and the source review so every finding has one final
  status and evidence link.
- [ ] Produce a residual-risk list. Every item must have an owner decision; no
  implementation defect is deferred as routine follow-up.

### Phase-specific evidence

- [ ] Redacted synthetic and real-wire traces agree on item/event semantics.
- [ ] Session resume produces the same intended next request as uninterrupted
  execution under each supported state strategy.
- [ ] Full workspace gates pass from a clean checkout of the release candidate.
- [ ] The repository policy gate reports no new/worsened violation and no
  Responses-program production file over 500 LOC.
- [ ] Every confirmed defect has its baseline failure, candidate pass, reviewer
  rerun, and final closure recorded.
- [ ] Every measurement/design finding has its approved baseline, pre-registered
  contract, candidate evidence, independent reproduction, and closure recorded.
- [ ] If required live conformance cannot run, P9 remains `BLOCKED`. The only
  alternative is to remove/disable the unverified backend or capability and
  update the advertised support surface before review.

### Review and exit gate

- [ ] A fresh cross-cutting Fable reviewer who implemented none of P0-P8 and was
  not a primary phase approver reviews the complete series and returns `READY`.
- [ ] Security, protocol, state, reliability, schema, and performance reviewers
  confirm their prior phase evidence still holds in the integrated candidate.
- [ ] The owner approves release based on the complete evidence ledger.
- [ ] Universal Gates A-D pass and the program status changes to complete.

## Evidence ledger

Update one row only after the phase's final fix-round review.

| Phase | Implementation commit(s) | Finding evidence and full-gate results | LOC/bypass policy report | Domain reviewer | Fable verdict | Status |
|---|---|---|---|---|---|---|
| P0 | | | | | | [ ] |
| P1 | | | | | | [ ] |
| P2 | | | | | | [ ] |
| P3 | | | | | | [ ] |
| P4 | | | | | | [ ] |
| P5 | | | | | | [ ] |
| P6 | | | | | | [ ] |
| P7 | | | | | | [ ] |
| P8 | | | | | | [ ] |
| P9 | | | | | | [ ] |

## Program completion checklist

- [ ] All phase-roadmap and evidence-ledger rows are complete.
- [ ] Every confirmed defect is closed with a reproducible regression; every
  measurement/design finding is closed with its pre-registered evidence path.
- [ ] No Critical or High finding remains open, mitigated-only, or unowned.
- [ ] No review finding from any phase remains deferred.
- [ ] All owner decisions are recorded and reflected in production behavior.
- [ ] Full fmt, Clippy, workspace test, doc test, diff, LOC, bypass, and secret
  gates pass from the final clean checkout.
- [ ] All changed production files are at or below 500 production LOC and all
  changed entrypoints are at or below 200 production LOC.
- [ ] Every Responses-program entry in the P1 legacy LOC baseline is removed;
  unrelated entries have not grown, changed production behavior, or passed their
  recorded due point.
- [ ] Final independent Fable verdict is `READY`.
- [ ] Owner release approval is recorded.
