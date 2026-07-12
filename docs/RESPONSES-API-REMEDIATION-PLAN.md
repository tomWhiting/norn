# Responses API remediation plan

- **Status:** Active; the P0 implementation candidate is committed through
  `ebb82c8`, but whole-phase Gate D returned `NOT READY` in the
  [`P0 Gate D review`](reviews/2026-07-11-p0-gate-d-review.md). The review
  reproduced a P0-introduced concurrent `openat(O_CREAT)` availability failure,
  invalidated the unqualified Gate C test-pass claim, found fetched content
  outside the private-artifact inventory, and required a corrective closure
  round. The prior scoped `READY` reviews and passing commands remain historical
  evidence for their exact snapshots; they are not evidence that the integrated
  candidate is ready. P0 remains unaccepted, and P1 and P2 have not started.
- **Baseline:** `main` at `263cc4f466b3` on 2026-07-10
- **Scope:** OpenAI Responses, ChatGPT/Codex OAuth and explicit named accounts,
  working-directory authority, prompt caching, streaming, conversation state,
  transport, schema, and usage behavior
- **Source review:**
[`reviews/2026-07-10-responses-api-implementation-review.md`](reviews/2026-07-10-responses-api-implementation-review.md)
- **P0 follow-up reviews:** credential/endpoint, transport/streaming, and
  workspace-trust reports under
  [`reviews/2026-07-11-remediation-review/`](reviews/2026-07-11-remediation-review/),
  all reviewing snapshot `7d121c9`. Subsequent targeted closure re-reviews of
  credential/config, transport/streaming, and private artifacts report `READY`
  on their respective surfaces; none is a whole-phase Gate D verdict. The
  final code range and machine/policy evidence are recorded in the
  [`P0 Gate C handoff`](reviews/2026-07-11-p0-gate-c-handoff.md). A separately
  reported `reviews/2026-07-11-exchange-changeset-review.md` artifact has not
  been received and is not evidence.

## Purpose

This is the execution tracker for the findings in the source review. It is not a
second findings document. It turns those findings into ordered work, defines the
observable difference expected after every phase, and prevents a phase from
being called complete without reproducible evidence and independent review.

Checkboxes have two roles. Phase work items, phase-specific evidence, and Gates
A-C are progress records and may be checked once objective evidence is complete
for the active candidate; checking them does not accept a phase or close a
finding. Phase status and roadmap entries, finding closure, Gate D, review/exit
acceptance, evidence-ledger status, and program completion are acceptance
records. They remain open until the final phase reviewer returns `READY`. A
required live test that does not run leaves its phase blocked unless a Gate A
owner decision removes the unverified capability and its advertised surface
before implementation.

## Target state

On completion:

- Codex OAuth credentials can reach only the compiled ChatGPT/Codex authority,
  and working-directory configuration cannot select the source or destination
  of any ambient credential, raw-debug sink, or provider executable. It also
  cannot install an automatic shell hook, rule command, skill-shell expansion,
  convention process, or model-selected profile command that bypasses that
  boundary.
- Raw settings layers cannot be merged through a public API without a
  compiler-enforced validation witness. A production embedder can supply a
  sealed static Codex credential only through a constrained constructor bound
  to the compiled Codex backend and endpoint.
- If live provider evidence proves independently stored credentials remain
  valid, Norn can manage and explicitly select isolated named OAuth accounts
  without repository-controlled account choice or silent identity changes. If
  not, the limitation is explicit. Automatic rotation is advertised only if
  current governing terms/product guidance permits it and later turn/retry
  evidence proves it safe before execution.
- Every automatic repository read uses one immutable launch root and a
  provenance-preserving, no-follow filesystem policy. Repository symlinks cannot
  escape or raise trust, and session, task, foreground-output, debug,
  full-output, and background-process artifacts are private without a
  repository-relative fallback.
- Norn stores and replays the ordered Responses item transcript rather than a
  lossy Chat-Completions-like reconstruction.
- Refusals, phases, hosted search, annotations, compaction, unknown items,
  `end_turn`, malformed/duplicate completion, tool-call identity, and
  turn-scoped Codex state have explicit semantics.
- Repository context/rules/profiles and compatible Developer messages follow an
  owner-approved source-to-wire-role authority matrix.
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
   phase control ordering. This campaign completes and reviews P2, then stops for
   the owner transcript decision before P3 implementation begins.
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
6. Intermediate reviews may record evidence about a finding already assigned
   to a later phase without expanding the current phase. A regression introduced
   by the current phase, a defect in its stated outcome, or a newly discovered
   unowned defect may not be deferred to obtain `READY`; it must be fixed and
   rechecked in the current phase. An item may be classified as a non-defect or
   intentional limitation only through an evidence-backed owner decision. The
   final phase reviewer returns `READY` or `NOT READY` after the fix rounds.
7. Work, evidence, and Gates A-C are checked as their evidence becomes complete.
   Only after the final reviewer returns `READY` may phase acceptance, finding
   status, Gate D, the evidence ledger, and roadmap status change to complete.

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
- [ ] The campaign diff adds no `.unwrap()`, `.unwrap_err()`, `.expect()`,
  `.expect_err()`, or `panic!`, including inside tests covered by pre-existing
  blanket lint allowances. Result-returning tests and explicit pattern
  assertions replace these calls; an existing allowance is not permission to
  add hidden debt.
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

Gates A-C are the active-phase dashboard. The marks below apply to the P0
corrective candidate based on code head `ebb82c8` and the Gate D review committed
at `5c22ba6`. After P0 receives Gate D `READY` and its final evidence is
entered in the ledger, this dashboard resets for P1; the ledger preserves P0's
accepted gate record. Gate D remains open until the whole-phase verdict.

### Gate A: entry and design

- [x] All dependency phases are complete.
- [x] The exact phase-base commit is recorded before implementation so every
  diff, LOC, lint, and added-line audit has one reproducible comparison range.
  P0's campaign base is `41ea210`; each later phase records its own accepted
  predecessor commit in the evidence ledger.
- [ ] The phase's owner decisions are recorded before implementation.
- [ ] Its finding IDs, invariants, production touch points, and defect-regression
  or measurement/design evidence method are agreed by the implementer and
  domain reviewer.
- [x] Any live credential use, external call, or billable experiment has
  separate owner approval before it runs.

P0 retrospective note: the base plan at `41ea210` still recorded D1 as open,
and no durable artifact proves that the implementer and domain reviewer agreed
the expanded evidence method before implementation. Those two timing claims
remain open rather than being inferred from later decisions and scoped reviews.
They cannot be made historically true by Gate D. Under the current
non-negotiable gate text, P0 cannot receive `READY` unless the owner records an
explicit P0-only retrospective process exception; the two boxes remain open
even if such an exception is accepted. For P0 only, an accepted exception
substitutes for those two historical requirements when evaluating Gate A and
the universal exit gate; it does not make the historical claims true. P1 and
later phases must satisfy both requirements prospectively.

### Gate B: implementation

- [ ] Confirmed-defect regressions fail for the documented reason on the reviewed
  baseline; measurement/design work has its pre-registered baseline and contract.
- [x] The production fix is complete across request, stream, persistence,
  replay, loop behavior, and user surface wherever the capability crosses them.
- [x] Replaced paths and temporary scaffolding are deleted in the same phase.
- [x] Changed production files satisfy the file-size and module-structure rules.
- [ ] The finding-to-test traceability table is updated.

P0 evidence note: the candidate regressions pass, but there is no durable matrix
showing each confirmed regression failing for the documented reason on
`41ea210`, and no finding-to-specific-test traceability table has yet been
recorded. Passing candidate tests do not retroactively prove either claim.

### Gate C: machine verification

- [ ] Phase-specific tests pass.
- [x] `cargo fmt --all --check` passes.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace --all-targets` passes.
- [x] `cargo test --workspace --doc` passes.
- [x] `git diff --check <phase-base>...HEAD` passes. Running bare
  `git diff --check` on the required clean checkout is not evidence.
- [x] For P0, reviewer-verified production LOC and bypass inspection covers every
  changed Rust item. From P1 onward, the syntax-aware repository policy command
  passes as a hard failure in the protected merge check.
- [x] For P0, a `git diff --no-ext-diff 41ea210...HEAD` added-line audit reports
  zero campaign-added unwrap, expect, panic, suppression, ignored-test, or
  unresolved-marker uses. Later phases use their recorded phase base.
- [x] For P0, security reviewers manually inspect all fixtures/evidence for
  secrets. From P1 onward, the checked-in evidence-redaction validator passes;
  no credential, real account identifier, private prompt content, reusable turn
  state, or raw cache key is present.

Gate D invalidated the two unchecked test claims: the convergence regression
failed 6/10 isolated reviewer runs and 19/20 subsequent independent repetitions.
The previous single passing workspace invocation is retained as a truthful
historical observation, not a stability claim. Corrective evidence must come
from a checked-in command or script that records the full distribution: at
least 20 repetitions for concurrency-sensitive tests, 50 for this regression,
plus one complete Gate C run. Any `all`, `every`, or `complete` coverage claim
must ship the exact mechanically generated inventory it quantifies.

### Gate D: independent review

- [ ] The domain reviewer inspects the implementation, tests, and raw evidence.
- [ ] A fresh rigorous Fable-model reviewer receives the source review, this plan,
  the diff, relevant official-source revision, and evidence bundle.
- [ ] Reviewers rerun the relevant tests and policy gates rather than trusting
  pasted output.
- [ ] All phase-owned findings, regressions introduced by the phase, and
  defects in the phase's claimed outcome are fixed and rechecked by the same
  reviewer.
- [ ] Final verdict is `READY` for the complete phase outcome, with no
  unresolved phase-owned or newly unowned item at any severity.
- [ ] Commit, commands, test counts, policy report, reviewer, date, and verdict
  are entered in the evidence ledger.

## Phase roadmap

| Phase | Status | Primary outcome |
|---|---|---|
| P0. Credential and workspace authority containment | [ ] | Repository data cannot select credential/backend/process authority, escape the immutable workspace root, or create non-private artifacts. |
| P1. Contract and enforcement baseline | [ ] | The program has executable contracts and protected quality gates. |
| P2. OAuth lifecycle correctness | [ ] | Login, refresh, storage, and logout fail safely; named-account selection is either evidence-backed or explicitly unsupported. |
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
| `SEC-01` through `SEC-15`, `BACKEND-01` | P0 | Existing request/auth, workspace filesystem, provider-config, hook, rule, profile, variant, skill, convention, and session-artifact paths |
| `SEC-16`, `BACKEND-02`, `SEC-08A`, `NF-1`, `NF-2`, `NF-4`, `QUAL-01` | P0 | P0 containment candidate and 2026-07-11 provisional review reports |
| `AUTH-01` through `AUTH-07`, `CONFIG-01`, `CONFIG-02` | P2 | P0-P1 |
| `STATE-01` | P4 | P3 canonical transcript |
| `EVT-01` through `EVT-07` | P4 | P3 canonical transcript |
| `STATE-02`, `STATE-03`, `ROLE-01`, `CODEX-01`, `CODEX-02`, `TRANS-01` | P5 | P3-P4 |
| `TRANS-02`, `USAGE-01`, `NF-3`, `NF-5`, `ROUTE-01` | P6 | P5 turn-owned transport and account affinity |
| `MODEL-01`, `ROLE-02`, `TOOL-01`, `REQ-01`, `SCHEMA-01`, `STRUCT-01` | P7 | P0, P3-P4, P6 |
| `CACHE-01`, `CACHE-02`, `CACHE-03`, `CACHE-04`, `CACHE-05` | P8 | P4, P6 accounting, P7 request/tool stability |
| Integrated closure and the two retained transport regressions | P9 | P0-P8 |

## Owner decision register

The relevant phase cannot pass Gate A while its decision is open.

| ID | Required decision | Due | Status / record |
|---|---|---|---|
| D0 | CI/merge-gate platform and required-check wiring. P1 remains blocked until a checked-in clean-checkout gate is protected. | P1 | [ ] Open |
| D1 | Exact compiled OAuth authority/path allowlist, redirect, workspace filesystem/command, and private-artifact policy. Custom trusted-proxy and repository-command consent are out of P0 scope. | P0 | [x] Decided 2026-07-10 and refined by the P0 threat model and provisional review round: accept no override or normalized `https://chatgpt.com[:443]/backend-api/codex[/]`, discard accepted input in favor of the compiled URL, and follow no redirects on credential-bearing clients. Both CWD layers are untrusted even when gitignored. Direct/raw provider authority, backend-selecting aliases, command-bearing hooks/rules, workspace skill shell, convention process categories, and model-selected profile prompt commands are rejected before use. One immutable launch root uses no-follow workspace reads/enumeration on supported descriptor-capable Unix targets; repository symlinks are unsupported, and Redox, ESP-IDF, and non-Unix workspace input fail closed. Debug-dump and private-artifact hardening are implemented in the candidate on the same supported target class, including descriptor-pinned ancestor traversal, foreground and process spools, task storage, and removal of relative sensitive-data fallbacks; unsupported targets fail before artifact I/O, and final review and phase gates remain required before acceptance. Pre-existing private-artifact links/non-regular entries are rejected, and read-only reopen hardens traversed legacy directories and regular files. Under a concurrent same-UID final-name replacement, the portable invariant is descriptor confinement: the replacement is never followed, no outside target is read/written/deleted, and the operation either fails or affects only the raced entry inside the pinned private root; POSIX does not provide portable strict rejection/serializability against that actor. Response headers remain fully redacted under D1, and redirect refusal follows the structural policy in `DECISIONS-2026-07.md` section 7. Custom API-key endpoints require HTTPS except loopback HTTP. No trusted proxy or implicit project-command consent is admitted. |
| D1A | Non-disclosing representation for unknown provider terminal discriminators: equality semantics, keying, identifier lifetime, output size, and deterministic test control. | P0 | [x] Decided 2026-07-11: known values retain typed mappings; an unknown exact byte sequence is represented only by its terminal category and a domain-separated full HMAC-SHA-256 tag under an OS-random process-lifetime key. The key and raw value are never persisted or logged. A deterministic key seam is crate-private and compiled only under `cfg(test)`; production exposes no public, configuration, or environment override. OS-random initialization failure is a typed fail-closed diagnostic error, never a fixed-key fallback. Tags support equality only within one process and are not cross-run fingerprints. Raw value and byte length are not exposed. |
| D1B | Location and access policy for fetched and other session-derived artifacts. | P0 | [x] Decided 2026-07-11: fetched documents are private session-owned artifacts beneath the trusted user-level Norn session store, never workspace files. P0 establishes a typed active-session artifact scope and migrates new fetch writes without pre-empting P3 transcript-format, historical-reference, fork-copy, or broad storage-migration decisions. Generic model file access may read/search only the active artifact subtree; it does not gain authority over credentials, indexes, raw child transcripts, or other sessions. |
| D1C | File-descriptor exhaustion mitigation introduced by descriptor-pinned private storage and persistent agent sinks. | P0 | [x] Mandatory per the 2026-07-11 post-review owner ruling: the official CLI raises its soft `RLIMIT_NOFILE` only to a finite OS-provided ceiling, reports inherited/effective limits and a labelled descriptor snapshot through `doctor`, and preserves typed `EMFILE` versus `ENFILE` diagnostics across the P0 private/session/process boundary. Library embedders do not receive an implicit process-global mutation. Structural descriptor sharing or lazy reopen remains an explicitly owned follow-up rather than being misrepresented as solved by a higher limit. `RLIMIT_CORE=0` remains a separate open decision because it also affects spawned user commands. |
| D1D | Remove the dormant `NornSettings.mcp_servers` configuration surface, or retain it behind provenance-aware containment and a real no-authority fixture. Active explicit MCP commands and library types are not this decision. | P0 | [ ] Open |
| D2 | Existing session policy: explicit version rejection or an offline one-shot migration. Record format versioning, crash atomicity, idempotency, backup/recovery, old-binary behavior, and treatment of irrecoverably lossy history. | P3 | [ ] Open |
| D3 | Threaded-state policy: decide replaceable Developer context and whether/how local compaction may reset an anchor without losing stored reasoning. Select a genuinely replaceable surface, lossless replay contract, fresh-thread transition, or disable threading/local replay. | P5 | [ ] Open |
| D4 | Single retry owner and existing configured attempt/budget semantics for HTTP and in-stream failures. | P6 | [ ] Open |
| D5 | Native `text.format` versus synthetic tool policy by API shape, catalog-selected apply-patch/search envelopes, and local-dispatch versus user-request semantics for tool-backed slash commands. | P7 | [ ] Open |
| D6 | Pre-register the cache experiment: ratify or replace the proposed 20-iteration design; approve public/private backends, models, spending, warm-up, key isolation/reuse, an approximately 15 requests/minute per-key ceiling rechecked against current guidance, concurrency, retention/cooldown, service tier, output/effort controls, randomization, primary measures, and statistical treatment. | P8 | [ ] Open |
| D7 | Approve credentials, spending, redaction, and retention for final live Codex and public Responses conformance. Without approval P9 is blocked, not passed by a skipped test. | P9 | [ ] Open |
| D8 | Ratify the source-to-wire-role matrix for product System policy, trusted operator Developer policy, repository `NORN.md`/rules/profiles, user input, and compatible backends that cannot preserve Developer. | P5 | [ ] Open |
| D9 | OAuth credential ownership and explicit named-account policy: Norn-managed stores; file-backed foreign `$CODEX_HOME/auth.json`; OS-keyring scope; static/embedder ownership; trusted selection; unknown expiry; accepted `provider.auth` spellings and required/forbidden companion fields; and isolated-account validity. | P2 | [ ] Open. Gate A chooses one branch. The supported branch requires an owner-approved live validity experiment before named-account implementation; success permits explicit named login/list/use/status/logout, while invalidation returns D9 to Gate A for an owner decision on the unsupported branch. If live approval/evidence is unavailable, the owner may instead select the unsupported branch at Gate A, remove the named capability and advertised surface, and return a typed/documented unsupported result without claiming technical incompatibility. A provider pins one credential identity for its lifetime, and repository or model input cannot select an account. |
| D10 | Automatic account rotation policy: applicable product/contract permission, eligible exhaustion signals, trusted candidate allowlist, pre-request rejection proof, turn/session affinity, state reset, cache-isolation handoff, and resume authorization. | P6 | [ ] Open until authoritative current terms/product guidance permits the behavior and P3/P5 establish transcript replay, account-scoped state, and turn affinity. The current [OpenAI Terms of Use](https://openai.com/policies/terms-of-use/) prohibit circumventing rate limits or restrictions, so exhaustion-triggered rotation is unsupported unless OpenAI or the governing contract explicitly establishes that this use is permitted. Even then, switching occurs only before dispatch or after a typed provider outcome proving no execution or state mutation; absence of observed output is insufficient. P6 otherwise keeps `ROUTE-01` unsupported. |

## Immediate operator mitigation

Until P0 ships, do not use Codex OAuth from a repository that has not been
audited for `.norn/settings.json`, `.norn/settings.local.json`, provider
profiles, model aliases, hooks, variants, skill shell policy, `.norn/rules`,
`.claude/rules`, `.meridian/rules`, `.norn/profiles`, `.meridian/profiles`,
`.norn/skills`, `.agents/skills`, `.claude/skills`, `CONVENTIONS.toml`, workspace
symlinks, `provider.options`, `api_shape`, `base_url`, `api_key_env`,
`debug_dump_dir`, and `runner_path`. Treat model-selected user profiles carrying
`prompt_commands` as command authority. Do not use Codex
OAuth with any custom Responses endpoint. For a custom endpoint, use an
explicit user-level or CLI API-key configuration with a dedicated environment
variable and HTTPS, except for a loopback-only local service. Do not rely on the
currently ignored `provider.auth` field to pin the auth mode (`CONFIG-01`). Use a
private umask for session/process artifacts. This reduces exposure but does not
close the P0 findings.

## P0. Credential and workspace authority containment

**Acceptance:** [ ] Gate D `NOT READY`; corrective round active;
**implementation status:** original 33 work items implemented; Gate D F1 and
D1B corrective implementations are complete, with the remaining corrections
still open and the prior Gate C test claim invalidated;
**findings addressed by candidate:** `SEC-01` through `SEC-16`,
`BACKEND-01`, `BACKEND-02`, `SEC-08A`, `NF-1`, `NF-2`, `NF-4`, and `QUAL-01`;
**current evidence:** the scoped closure reviews remain valid for their exact
surfaces, but the integrated Gate D review found one blocker, one now-resolved
artifact-location decision, missing fixtures, and audit/documentation defects;
the original candidate is committed through `ebb82c8` and the Gate D record at
`5c22ba6`; **dependencies:** D1, D1A, D1B, and D1C resolved; D1D and the P0-only
retrospective Gate A exception remain open.

### What this phase fixes

Project-controlled `.norn/settings.json` can currently supply `base_url` while
the provider still selects OAuth, causing bearer and account headers to be sent
to that origin. The same working-directory layers can select both
`api_key_env` and `base_url`, causing Norn to read an arbitrary named ambient
secret and send it to a repository-selected endpoint. Backend identity is also
inferred from the absence of a URL override, so spelling the canonical ChatGPT
URL changes state semantics. The same untrusted layers can silently enable raw
API dumps or choose a Claude Runner executable. Public `test-utils` OAuth seams,
derived credential `Debug`, response metadata, and raw authority error bodies
provide additional token or identity disclosure paths. Adversarial P0 review
also found indirect backend selection, automatic shell commands in hooks,
rules, and workspace profiles, eager arbitrary-file reads from variant prompt
files, and a project override that can re-enable skill shell expansion. The
follow-up threat model found symlink/root-alias races across every automatic
workspace file family, workspace skill and convention subprocess paths,
model-selected trusted profile commands, raw provider options/API-shape/name
collisions, provider-controlled diagnostic text, and non-private session/spool
artifacts.

The provisional review round found that public raw settings APIs could skip
provenance validation, generic auth-provider injection lacked a legitimate
sealed static-Codex replacement, private persistence was incomplete, unknown
terminal discriminators lost all correlation, redirects were ordinary stream
failures, and campaign-added panic-style test helpers hid under older blanket
allowances. The current candidate contains targeted fixes for those findings,
including one descriptor-pinned private-artifact primitive and a real-entry
`SEC-08A` authority regression. Targeted closure reviewers report `READY` on
credential/config, transport/streaming, and private-artifact surfaces. Those
fixes are not accepted until whole-phase Gate D passes.

### Difference after the phase

Credential authority, deployment/backend identity, and endpoint selection are
separate typed concepts. Codex OAuth headers are attached only to the compiled,
normalized HTTPS authority/path approved in D1. A cloned repository cannot
extend that trust boundary or select an API-key environment variable. An
explicitly selected canonical endpoint retains Codex backend semantics rather
than silently becoming public Responses. Trusted user-level and explicit CLI
API-key custom endpoints remain available after HTTPS-or-loopback authority
validation. Untrusted layers cannot enable raw dumps, select executables or
backend-bearing aliases, install automatic hook/rule/profile commands, select a
variant prompt file, or relax a user skill-shell restriction. Trusted dumps use
private regular files and reject symlinks. One immutable launch root governs
secure workspace reads and descriptor-relative enumeration; repository symlinks
are rejected. Workspace skill shell, convention process categories, and prompt
commands on model-selected profiles are disabled as intentional confused-deputy
closures. Session, lock, temporary, full-output, and process-spool artifacts are
private, including task and foreground threshold-output artifacts, with
descriptor-relative ancestor traversal and no repository-relative fallback.
Raw settings cannot be merged without a validation witness. Production static
Codex credentials use a constrained constructor that cannot change backend or
endpoint authority. Unknown terminal discriminators remain structurally
distinguishable without raw provider text, and redirect refusal is explicit.
Static workspace profiles/rules, non-process LOC/pattern conventions, and inline
variant prompts remain available; user and explicit CLI authority surfaces
retain their intended behavior where they are operator-selected.

### Work checklist

- [x] Introduce an explicit deployment/backend identity used by capability,
  service-tier, state, auth, and endpoint resolution.
- [x] Normalize and validate scheme, authority, port, path, and userinfo before
  any credential-bearing request is constructed.
- [x] Disable automatic redirects on credential-bearing clients. Do not forward
  credentials, account headers, or request bodies to any redirect target.
- [x] Reject OAuth plus an untrusted endpoint before opening a connection.
- [x] Reject `base_url`, `api_key_env`, `auth`, `debug_dump_dir`, and
  `runner_path` from both working-directory settings layers, including provider
  profiles, before merge, env lookup, file creation, or process execution.
- [x] Reject working-directory model aliases that select `provider_profile` or
  `api_shape`, and prevent a CWD default model or workspace profile model from
  activating a backend-bearing user alias without an explicit trusted CLI
  selection.
- [x] Reject every non-empty project/local hook slot before merge. Preserve
  rule/profile source provenance and reject working-directory rule
  `shell_source` and bare-name workspace profile `prompt_commands` before loop
  construction, including child-agent profile resolution.
- [x] Reject working-directory `variants.<variant>.prompt_file` before eager
  file loading. Permit an untrusted layer to disable skill shell expansion but
  never to enable it over a trusted restriction.
- [x] Canonicalize the launch working directory once, publish it as immutable
  root/child/fork context, and route every automatic workspace settings,
  context, nested context, rule, profile, capability, skill/resource, variant,
  and convention read through one provenance-preserving API.
- [x] On supported descriptor-capable Unix targets, walk workspace paths and
  enumerate directories relative to pinned descriptors without following any
  symlink; require regular final files and recognize physical path aliases
  without canonicalizing the final candidate. Reject repository symlinks even
  when they point inside the repository.
- [x] Fail closed when workspace input is present on Redox, ESP-IDF, or non-Unix
  targets until an equivalent no-follow implementation exists. Record this
  release limitation and intentional compatibility break rather than using a
  weaker fallback.
- [x] Normalize configured search paths that physically resolve beneath the
  launch root once, require absolute trusted home/explicit paths, and prevent a
  symlink alias from changing trust tier after classification.
- [x] Disable shell expansion for every physically workspace-sourced skill,
  regardless of global user policy. Strip all process-bearing LSP, diagnostic,
  remediation, and report categories from workspace `CONVENTIONS.toml`, retaining
  only non-process LOC/pattern checks.
- [x] Reject `prompt_commands` from every model-selected profile, including a
  trusted user profile. Preserve them only for a trusted operator/programmatic
  selection; do not fall through to a same-name alternative.
- [x] Reject CWD `provider.options`, provider-profile `api_shape`, backend-bearing
  alias/profile collisions, and all typed-field collisions in raw request
  options before backend resolution or network I/O.
- [x] Enforce that provenance rule in both CLI and shared library runtime
  loaders; reject raw forbidden-field presence even if a later CLI value wins.
- [x] Ensure user and CLI endpoint overrides cannot grant OAuth trust beyond the
  compiled canonical destination.
- [x] Keep custom compatible endpoints on an explicit non-OAuth path using
  HTTPS, with plaintext HTTP permitted only for loopback. A trusted-proxy or
  remote-plaintext feature is separate future security work, not P0 scope.
- [x] Remove arbitrary OAuth-authority and injected-auth seams from production
  and `test-utils` feature builds; retain them only in crate unit tests.
- [x] Make raw settings loading and mechanical merging crate-internal. Expose a
  public load-validate-merge path, or a sealed `ValidatedSettingsLayers` witness
  with private fields whose merge method accepts trusted CLI/programmatic
  overrides. No public caller can obtain project/local layers and merge them
  without authority validation.
- [x] Keep arbitrary `Arc<dyn AuthProvider>` injection unit-test-only. Add a
  production constructor accepting only a validated sealed static Codex
  credential, require OAuth/Codex backend identity, and bind it to the compiled
  ChatGPT/Codex endpoint. Until P2 defines an acknowledged owner sink, this P0
  path does not refresh or rotate the static credential; a 401 returns a typed
  owner-refresh requirement. It must not accept a custom auth implementation or
  caller-selected token/request authority. Preserve this contract in an in-repo
  public-API compile fixture; updating Meridian itself is separate downstream
  integration evidence, not part of Norn's clean-checkout gate.
- [x] Prove every spawned/forked child reuses the parent provider instance and
  that profile, variant, and model text cannot trigger backend-alias resolution,
  environment lookup, credential construction, or endpoint replacement.
- [x] Ensure credential-bearing runtime/auth/request `Debug` formatting and
  rejected-destination errors never reveal bearer, refresh, ID, API-key, PKCE,
  or account secrets; redact credential-like response metadata including
  reusable turn state, cookies, and redirect locations, and never propagate raw
  OAuth/provider error bodies or provider-controlled terminal text. This claim
  does not include the legacy raw provider-settings container.
- [x] On the generic error-status path, stream and discard non-redirect bodies
  within the existing request timeout. Preserve the specialized 401 refresh and
  429 backoff paths, which drop their response bodies without draining, and
  classify redirects immediately without reading their bodies. Preserve the
  established stalled generic-error timeout/retry semantics; do not replace it
  with an unreviewed broad status-only behavior change.
- [x] Preserve distinct unknown `response.failed` codes and incomplete reasons
  without propagating provider-controlled text. Known values retain typed
  mappings; unknown values expose only the D1A process-local keyed opaque tag.
- [x] Preserve D1's `NF-4` disposition: every response-header value remains
  redacted. The loss of upstream request correlation is an accepted diagnostic
  limitation unless a later owner decision approves a narrow structural field.
- [x] Classify every 3xx response as a terminal redirect-policy refusal whose
  locally authored error names the status and states that credential-bearing
  redirects are not followed. Do not expose or follow `Location`, and do not
  describe the result as an ordinary stream/body error.
- [x] Require debug-dump targets to be regular non-symlink files and mode `0600`
  on supported descriptor-capable Unix targets; fail closed before artifact I/O
  on unsupported targets.
- [x] Require session data, index, lock, atomic temporary, full-output spool,
  foreground threshold-output, process-spool, and persistent-task
  directories/files to be private (`0700` directories, `0600` regular files on
  supported descriptor-capable Unix targets), descriptor-relative no-follow on
  every ancestor and final component, and fail closed on links or non-regular
  targets across create, reopen, rewrite, and resume. Redox, ESP-IDF, and
  non-Unix targets return typed `Unsupported` before private-artifact I/O.
- [x] Reject pre-existing private-artifact links and non-regular entries. Race
  tests against a concurrent same-UID final-name replacement prove the portable
  D1 boundary: descriptor confinement, no link following, no outside-target
  mutation/disclosure, and only failure or a confined in-root entry effect. Do
  not claim portable serializability against a same-UID process.
- [x] Remove repository-relative `.norn` fallbacks for session data, shared task
  storage, and debug output. When neither an absolute configured root nor a
  trusted home exists, return a typed error rather than writing beneath mutable
  CWD.
- [x] Remove all 177 campaign-added unwrap, expect, and panic calls identified
  at frozen snapshot `7d121c9`. Add or widen no lint suppression.
- [x] Put new security logic in cohesive modules below 500 production LOC. If a
  changed legacy file is over the limit, bring it below the limit in this phase.

### Gate D corrective checklist

- [x] Retain a standalone macOS reproducer and raw distribution for the
  concurrent same-name descriptor-relative `O_CREAT` failure. Implement a
  bounded, documented correction without returning to absolute-path sensitive
  writes; cover existing targets, independently opened parent descriptors,
  `O_EXCL` exactly-one-winner behavior, and persistent failure termination.
- [x] Run the session convergence regression 50 times after the correction and
  record every result, not only the final invocation. The same Gate D reviewer
  independently accepted F1 after an additional 15/15 run of all three tests
  and 14,400 higher-contention reproducer attempts with no second-order
  `ENOENT`; see the
  [`correction review`](reviews/2026-07-12-p0-openat-correction-review.md).
- [x] Introduce one typed active-session artifact scope and migrate fetched
  documents out of the workspace into immutable private session artifacts.
  Repeated fetches of one URL must not rewrite bytes referenced by an older
  transcript event. Implementation and reproducible scoped evidence are in the
  [`D1B correction record`](reviews/2026-07-12-p0-fetch-artifact-correction.md);
  independent acceptance remains pending and whole-phase Gate D stays open.
- [ ] Implement D1C for the official CLI, `doctor`, and typed P0
  private/session/process error paths. Record descriptor-allocation boundaries
  and do not claim coverage beyond the inventory.
- [ ] Resolve D1D. Remove the consumerless settings surface or retain it only
  with explicit provenance-aware containment, redacted secret-bearing fields,
  and a hostile real-entrypoint no-authority regression.
- [ ] Delete or demote `session_file_path` and
  `resolved_session_file_path`; no production-compatible raw path derivation may
  remain beside the validated replacement.
- [ ] Make `PrivateRoot` ancestor-creation behavior, identifier names, and
  documentation agree. Do not silently relabel behavior whose missing-mount
  semantics require a policy decision.
- [ ] Add body-never-read/non-disclosure sentinels for specialized 401 and 429
  responses, plus loop-level timeout and lossless `try_send` to awaited `send`
  handoff regressions.
- [ ] Regenerate production LOC with one syntax-aware method that excludes
  test-only items wherever they occur. Rebuild the bypass and artifact-writer
  inventories with exact commands and inputs.
- [ ] Complete the baseline-failure and finding-to-test traceability records,
  then correct rather than append to the invalidated Gate C handoff.

### Phase-specific evidence

- [x] A hostile local endpoint receives no request when selected from project
  configuration; a repository-selected environment variable is rejected before
  lookup; and a hostile redirect target receives no redirected request.
- [x] Real CLI and shared-library settings entrypoints reject forbidden project,
  local, and profile fields while positive user-level and CLI authority cases
  retain their intended behavior.
- [x] Public API compile-contract evidence proves raw loaded project/local
  layers cannot be mechanically merged without authority validation.
- [x] An in-repo public-API compile-contract fixture and request assertion prove
  a sealed static Codex credential uses the compiled Codex destination without
  exposing generic auth-provider injection.
- [ ] A real Meridian dependency upgrade, build, and request assertion are
  recorded separately as downstream evidence.
- [x] All thirteen hook slots are rejected from both CWD settings layers;
  project/local shared-loader and CLI regressions prove command text is not
  executed or echoed, while user/programmatic hooks remain available.
- [x] Single-scan rule provenance rejects `shell_source` from `.norn`, `.claude`,
  and `.meridian` workspace rules without execution while user rule commands
  remain available.
- [x] Root and child workspace profiles reject prompt commands without
  executing or echoing them; static workspace profiles, user prompt-command
  profiles, and explicit profile paths retain their intended behavior.
- [x] Cross-layer tests reject CWD settings/profile activation of a user backend
  alias before environment lookup, while explicit CLI selection remains
  supported. Variant prompt-file and skill-shell widening sentinels fail before
  file or process side effects.
- [x] A child profile containing a backend-bearing alias sentinel proves no
  child provider reconstruction, environment lookup, or endpoint change occurs.
- [x] Every workspace file family rejects final and ancestor symlinks, `..`,
  non-regular files, launch-root replacement, and user-path alias repointing.
  Root, child, spawn, fork, session-remove, and direct shared-library entrypoints
  use the same immutable launch root.
- [x] Workspace skill activation proves shell text is never executed or echoed,
  including after a search-path alias is repointed. Mixed conventions retain LOC
  and pattern checks while LSP/diagnostic/remediation/report commands cannot run.
- [x] Model-selected workspace and user profiles with prompt commands are
  rejected without execution or command echo; the same user profile remains
  usable when selected through the trusted operator path.
- [x] Project/local provider-options, profile API-shape, and same-name collision
  fixtures prove no untrusted value reaches backend, environment, process, or
  network consumers.
- [ ] A dormant-MCP provenance fixture proves merged `mcp_servers` values cannot
  gain runtime authority before a provenance-aware, consent-gated consumer is
  implemented.
- [x] URL tests cover HTTP, userinfo, case, trailing dots, default/non-default
  ports, lookalike hosts, path variants, redirects, and canonical URLs.
- [x] Capability/payload snapshots prove explicit and implicit canonical Codex
  selection have identical backend semantics.
- [x] Trusted API-key custom and compatible endpoints retain their intended,
  explicitly tested behavior; remote HTTP is rejected and loopback HTTP remains
  supported.
- [x] Debug-dump permission/symlink tests, OAuth feature-surface inspection, and
  sentinel diagnostic tests prove raw tokens, claims, headers, and authority
  error bodies do not escape.
- [x] Response-header dump fixtures prove the D1/NF-4 correlation decision is
  exact: every response-header value remains redacted and no credential, cookie,
  redirect target, turn state, or account metadata is exposed.
- [x] Malformed SSE, `response.failed`, and error-status sentinels prove provider
  text and control bytes never enter logs/errors; stalled generic error-status
  fixtures retain the existing typed timeout behavior while the body is
  streamed and discarded. Specialized 401 and redirect fixtures prove their
  response bodies are not disclosed; code-path inspection confirms those
  responses are dropped without draining.
- [ ] A stalled 429 response body or explicit read-observer fixture proves the
  specialized 429 path does not read or wait for its body, while a sentinel
  proves the body is not disclosed.
- [x] Distinct unknown failed/incomplete discriminators remain distinguishable
  while raw values, control characters, and secret sentinels never appear.
- [x] HTTP 301/302/303/307/308 fixtures produce explicit redirect-policy
  refusal, omit `Location`, and send no second request.
- [x] Session/index/lock/temp/full-output/foreground-output/process-spool/task
  tests cover ancestor and final symlinks, non-regular targets, permissive umask,
  legacy modes, reopen, absent trusted homes, and replacement races that prove
  outside sentinels remain unchanged under the exact D1 confinement contract.
- [ ] A complete private/session artifact-writer inventory names every writer,
  its ownership and lifetime, root, mode, no-follow/atomicity behavior, and model
  read surface. The inventory includes fetched documents and supports every
  coverage claim made in the final handoff.
- [x] A retained concurrency evidence script records 50/50 successful session
  convergence runs plus primitive-level same-name-create, `O_EXCL`, and
  persistent-failure cases on the affected macOS platform.
- [x] A no-external-diff audit reports zero campaign-added unwrap, expect, panic,
  suppression, ignored-test, or unresolved-marker uses.

### Residuals requiring Gate D disposition

These are not claimed fixed by a broad P0 statement and are not presumed to be
phase-owned defects. The external reviewer must verify the evidence-backed
classification or identify a reachable defect in P0's claimed outcome, which
then becomes a P0 blocker:

- Workspace text reads remain unbounded. The remediation must be a designed
  streaming/size policy with an owner-approved value or provider fact, not an
  arbitrary byte cap invented to close review.
- The raw legacy provider-settings container still derives `Debug`. P0's
  structural-redaction claim covers credential-bearing runtime/auth/request
  types and free-form request options, not every raw settings value. No reachable
  logging call has been identified, but the type remains misuse-prone.
- Public `Scanner`, `scan_rule_dirs`, and `discover_skills` convenience APIs are
  trusted-input-only. Secure runtime assembly uses launch-root-aware paths; an
  embedder must not pass repository-controlled roots through the legacy APIs.
- `mcp_servers` and its environment map are merged but currently dormant. They
  require source provenance and explicit consent before any future runtime wiring;
  merge precedence is not authorization.
- Redox, ESP-IDF, and non-Unix workspace input deliberately fail closed. This
  protects the trust boundary but is a release compatibility limitation until
  equivalent no-follow filesystem primitives are implemented and reviewed.

### Review and exit gate

**Current gate state:** Gate D returned `NOT READY`; corrective implementation
and evidence reconciliation are active. Three provisional reports review frozen snapshot
`7d121c9`; they are archived review input, not Gate D evidence for the final P0
candidate. Subsequent targeted credential/config, transport/streaming, and
private-artifact closure reviewers each report `READY` on their owned final
surfaces. The original code range is committed through `ebb82c8`; its single
passing workspace test run and manual audits are recorded in the
[`P0 Gate C handoff`](reviews/2026-07-11-p0-gate-c-handoff.md), but Gate D
invalidated the unqualified test-pass claim. The corrective candidate must close
the findings in the [`Gate D review`](reviews/2026-07-11-p0-gate-d-review.md),
rerun all gates with distribution and inventory evidence, and receive a fresh
integrated verdict over the complete range. No scoped `READY` or prior lucky
sample is a whole-phase verdict.

- [ ] A security reviewer threat-models every credential destination, redirect,
  automatic working-directory command, and eager working-directory file read.
- [ ] A provider/config reviewer verifies trust cannot originate in project data.
- [ ] A Fable adversarial reviewer returns `READY` before this phase ships
  independently of later protocol work.
- [ ] Existing fmt, strict Clippy, workspace tests, doc tests, diff check, and a
  reviewer-verified syntax-aware LOC/bypass inspection pass. P0 does not wait for
  the broader P1 policy infrastructure.
- [ ] Universal Gates A-D pass and all P0-owned findings have closure evidence.
  For P0 only, the explicit owner-approved retrospective exception described in
  Gate A may substitute for its two unsatisfied historical timing requirements.

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
  compaction, unknown reasoning parts/items, interleaved and duplicate call
  completion, malformed terminal data, `end_turn`, turn-state headers/metadata,
  failures, rate limits, incomplete streams, and cache usage.
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

**Status:** [ ] Not started; **findings closed:** `AUTH-01` through `AUTH-07`,
`CONFIG-01`, `CONFIG-02`; **dependencies:** P1 and D9. Gate A selects either the
named-account branch, whose prerequisite live-validity experiment must succeed,
or an owner-approved unsupported branch that removes the named surface without
making a technical-validity claim. Automatic account rotation is not in P2.

### What this phase fixes

Norn's login fallback reads the account ID from the wrong JWT shape. Refresh is
single-flight only among callers sharing one `AuthManager`; separately
constructed providers in one process, other Norn processes, and the Codex CLI
can still race. A successful refresh can be reported after persistence failed,
and a static/embedder credential can rotate without returning the new lineage to
its owner. Credential load and proactive refresh errors are hidden, browser
success precedes durable save, revoke failure prevents local deletion, and
status/doctor do not distinguish local credential states or remote verification.
The file-backed `$CODEX_HOME/auth.json` model also has no trusted named-account
selection contract; Codex OS-keyring storage is a distinct integration surface.

### Difference after the phase

Norn-created and imported Codex credentials produce the required account
metadata. One coordinator owns each credential identity inside a process;
cooperating Norn processes serialize refresh, and a non-cooperating foreign
write is detected rather than knowingly overwritten. Corruption and unknown
expiry are distinct local states; refresh conflict and successful-but-undurable
refresh are distinct operation outcomes. Static credentials cannot rotate without an acknowledged owner
sink. If the Gate A validity evidence permits, a named account can be logged in,
listed, explicitly selected, inspected, and removed without overwriting another
named account or the Codex CLI's current credential. Otherwise Norn reports that
simultaneous named credentials are unsupported rather than presenting an
unusable selector. The selected identity is pinned when a provider
starts; selection changes affect new providers only. Until P5 persists account
affinity, resume requires an explicit trusted account choice and never consumes
the active-account default. Browser success means the
credential and its directory entry are durable, and logout always removes the
local credential while reporting remote revoke separately. P2 makes no
automatic-rotation claim.

### Work checklist

- [ ] Parse the namespaced Codex auth claim and retain only provider-shape
  compatibility justified by sanitized fixtures; reject conflicting account
  identifiers.
- [ ] Remove unused serialization authority from `IdTokenClaims` and stop
  silently converting claim-parse failures into empty metadata.
- [ ] Resolve the existing eight-day unknown-expiry fallback under D9 using a
  pinned provider/source fact or a typed replacement policy. Do not retain or
  replace it through an unlabeled constant.
- [ ] Centralize absolute trusted Codex/Norn auth-root resolution in one
  library-owned typed resolver used by CLI and library callers.
- [ ] Introduce typed credential ownership for a Norn-managed store, the
  file-backed foreign `$CODEX_HOME/auth.json`, and static/embedder-owned
  credentials. D9 explicitly decides whether Codex OS-keyring integration is
  supported; it is out of scope unless a safe library interface is selected.
- [ ] Share one coordinator per credential storage identity inside a process.
  Use reclaimable registry entries rather than a permanent global cache.
- [ ] Implement a bounded per-credential reload-lock-refresh-save transaction
  for cooperating Norn processes with atomic durable storage and explicit lock
  failure behavior.
- [ ] Detect a foreign writer changing a shared credential during refresh and
  return a typed conflict without knowingly overwriting it. Do not claim a Norn
  lock coordinates the Codex CLI.
- [ ] Never report refresh success when a rotated credential was not durably
  accepted by its owner. Static credentials require an acknowledged persistence
  sink or have refresh disabled.
- [ ] Preserve typed load, parse, refresh, persistence, and permission errors.
- [ ] Define and test any stale-token use explicitly; do not silently fall back.
- [ ] Use private no-follow regular-file credential storage with atomic
  replacement, file fsync, parent-directory fsync, and durable deletion.
- [ ] On the D9 supported branch, add a versioned named-account index whose user
  alias maps to an opaque storage identifier; aliases are never filesystem paths.
- [ ] On the supported branch, add explicit `auth login --name`, `auth list`,
  `auth use`, `auth status [name]`, and `auth logout [name]` surfaces with
  deliberate all-account behavior. On the unsupported branch, do not create an
  index or selection surface; return and document a typed unsupported outcome.
- [ ] Preserve `$CODEX_HOME/auth.json` as an explicitly identified foreign
  source. Never duplicate its rotating refresh token into a named store.
- [ ] Pin the selected identity for a provider/run. Before P5 adds persisted
  affinity, every resume requires explicit trusted account selection and rejects
  the Norn active-account default; it never silently chooses a replacement.
- [ ] Make account selection trusted-only: explicit CLI, Norn-owned active
  selection, or trusted user configuration. Project/local settings, model
  aliases/profiles, prompts, and tools cannot select or rotate accounts.
- [ ] Close `CONFIG-01` and `CONFIG-02` with the D9-approved typed
  auth/source/account matrix, including whether `oauth`, `env`, and `api_key` are
  distinct or aliases and which companion fields each requires or forbids. Do
  not invent compatibility semantics during implementation. Validate before
  environment lookup, credential loading, provider construction, or network I/O.
- [ ] Replace CLI status and doctor booleans with one library-owned credential
  state evaluator distinguishing missing, malformed, access-expired,
  refresh-candidate, locally-valid, and unknown states. Refresh conflict and
  undurable persistence remain typed operation outcomes unless D9 separately
  approves a durable recovery journal. List/status remain local,
  side-effect-free, remotely unverified, and free of token or identity
  disclosure. Doctor may add an explicit optional active probe without changing
  the local classification contract.
- [ ] Delay browser completion until exchange, durable credential save, and any
  named-account index update succeed. Own and join the worker outcome so
  cancellation cannot leave a surprise credential write.
- [ ] Always clear local credentials during logout and report remote revocation
  as an independent result; make the local deletion durable.

### Phase-specific evidence

- [ ] Redacted real-shape JWT fixtures cover namespaced and supported fallback
  claim sources through login, import, refresh, and final header application
  without storing a usable token.
- [ ] Two separately constructed providers in one process share a coordinator
  and cannot refresh the same lineage twice.
- [ ] Two real OS child processes targeting one Norn-managed identity perform
  one effective refresh exchange and converge on identical durable state.
- [ ] A scripted lock-ignoring foreign writer replacing file-backed
  `$CODEX_HOME/auth.json` during exchange is detected; Norn does not knowingly
  overwrite it.
- [ ] Successful exchange followed by save, rename, fsync, permission, or owner
  sink failure is not returned as ordinary success.
- [ ] Static credentials cannot strand a rotated refresh token: refresh is
  rejected without an owner sink, and sink failure remains visible.
- [ ] Corrupt, unreadable, partial, permission-denied, symlink, non-regular,
  rename, fsync, and delete cases surface the correct typed state.
- [ ] Browser exchange/save/cancellation failures cannot render a final success
  page or perform an unowned later write.
- [ ] Revoke network/authority failure still deletes the local credential.
- [ ] On the supported branch and before named-account implementation, the
  owner-approved experiment verifies whether logging in and refreshing account B
  leaves account A authenticated and independently refreshable, not merely
  unchanged on disk. Its redacted result selects supported implementation or
  returns D9 to Gate A for an unsupported-branch decision.
- [ ] On the supported branch, two named accounts coexist; switching affects
  only new providers; logout of one leaves the other unchanged; aliases and
  duplicate identities fail safely. On the unsupported branch, tests prove no
  named registry/selector is exposed and the typed result explains the limit.
- [ ] Status and doctor produce the same local classification for every fixture;
  any doctor active-probe result is reported as separate remote evidence.
- [ ] Resume tests prove no pre-P5 session consumes an implicit active-account
  default after account selection changes.
- [ ] The typed auth/source/account matrix rejects every invalid combination
  before reading an environment variable or credential.

### Review and exit gate

- [ ] Security/auth and concurrency/persistence reviewers independently approve.
- [ ] A Fable adversarial reviewer returns `READY` after attacking failure paths.
- [ ] Universal Gates A-D pass and `AUTH-01` through `AUTH-07`, `CONFIG-01`,
  and `CONFIG-02` are closed.

## P3. Canonical ordered transcript

**Status:** [ ] Not started; **foundation for:** `STATE-01`, `EVT-02`;
**dependencies:** P0-P2 and D2.

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
- [ ] Preserve replayable item order, message phase, call/item IDs, encrypted
  reasoning and opaque unknown reasoning parts, refusal, annotations, hosted
  search, and compaction data.
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
`EVT-07`; **dependencies:** P3.

### What this phase fixes

The SSE mapper drops item identity and phase, ignores refusal and hosted-search
provenance, cannot repair missing deltas from authoritative completion data, and
silently skips unknown output items. Completion is not reconciled by stable item
identity; delta-only calls and malformed terminal data can become executable or
ordinary empty/zero-usage success.

### Difference after the phase

Completed output items drive the canonical transcript. Deltas drive responsive
UI keyed by item/content identity and reconcile against authoritative completed
content. Refusal is a non-retryable model outcome distinct from transport error,
hosted search survives replay, and an unknown output item cannot be reported as
ordinary success. Call completion is identity-safe and idempotent, and only an
authoritative completed item can become executable.

### Work checklist

- [ ] Use the P1 production-LOC baseline to decide whether the SSE production
  implementation requires decomposition. Split it by parser, item assembly,
  reconciliation, and terminal mapping where required by LOC or cohesion.
- [ ] Maintain separate checked manifests for the pinned public taxonomy
  (52 stream events and 28 output-item variants at review time) and Codex-specific
  events/items/headers. Each name is handled, allowlisted lifecycle-only, or
  typed unsupported, and a taxonomy change fails the manifest check.
- [ ] Key deltas by item ID, output index, and content index.
- [ ] Preserve item ID and call ID on added/delta/done/completed events. Reconcile
  interleaved and duplicate frames idempotently; reject conflicting identity and
  remove every order-based completion fallback.
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
- [ ] Replace empty-string, zero-usage, absent-response-ID, default-EndTurn, and
  delta-only-call fallbacks with typed required/optional fields. Unknown reasoning
  parts survive opaquely; malformed required completion data is a protocol error.

### Phase-specific evidence

- [ ] Pure refusal, mixed content/refusal, tool-loop refusal, structured-output
  refusal, and resumed refusal fixtures preserve usage and cannot retry or
  become empty success.
- [ ] Hosted-search action, sources, annotations, and answer survive a tool-loop
  continuation and a persisted resume.
- [ ] Missing/malformed delta fixtures are repaired by authoritative completion
  data or fail with a typed diagnostic.
- [ ] Interleaved calls prove call two cannot complete call one. Exact duplicate
  frames are idempotent; conflicting duplicates, delta-only calls, and missing
  authoritative completion cannot execute.
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
- [ ] Universal Gates A-D pass and `STATE-01`/`EVT-01` through `EVT-07` close.

## P5. Conversation and Codex turn semantics

**Status:** [ ] Not started; **findings closed:** `STATE-02`, `STATE-03`,
`ROLE-01`, `CODEX-01`, `CODEX-02`, `TRANS-01`; **dependencies:** P2-P4 and
D3/D8/D9.

### What this phase fixes

Local removal of managed Developer context does not remove it from a
`previous_response_id` chain. Norn also ignores the backend's explicit
`end_turn` decision and `x-codex-turn-state`, so continuation and sticky-routing
semantics are inferred or omitted. A stored thread does not return replayable
encrypted reasoning, so resetting its anchor after local compaction can silently
lose reasoning continuity. Repository `NORN.md`, rules, and profile bodies also
receive inconsistent System/Developer authority based on discovery path.
Account identity is also not represented in turn affinity, so later account
routing cannot prove that provider anchors and Codex turn state stay with their
owning credential.

### Difference after the phase

Each backend has an explicit state strategy. Replaceable dynamic context cannot
accumulate invisibly in a provider thread. Top-level instructions are resent
where required. `end_turn` controls continuation explicitly, and Codex turn
state is reused only inside its owning user turn, never across turns, agents, or
sessions. Dropping/cancelling the turn also owns and terminates its HTTP producer,
so sticky state cannot outlive the turn through an orphaned task. Every valid
anchor transition preserves reasoning or starts a semantically fresh thread, and
repository sources follow the D8 role-authority matrix.
Provider anchors and Codex turn state are additionally bound to one opaque
credential identity and cannot cross an account switch.

### Work checklist

- [ ] Implement D3 consistently across loop state, provider capabilities,
  compaction, persistence, resume, and request construction.
- [ ] Implement D8 across root/nested `NORN.md`, rules, workspace/user profiles,
  dynamic harness context, product instructions, and child/fork prompt assembly.
- [ ] Keep ChatGPT/Codex `store:false` replay distinct from public Responses
  threading and prove both wire shapes.
- [ ] Resend top-level instructions with a response anchor while ensuring
  replaceable Developer context has the intended effective lifetime.
- [ ] Preserve `response.completed.response.end_turn` as typed terminal metadata
  and define its interaction with calls, refusals, and no-output continuation.
- [ ] Capture `x-codex-turn-state` from headers/metadata, replay it within the
  same turn, and clear it at the turn boundary.
- [ ] Bind response anchors and Codex turn state to the opaque P2 credential
  identity. A mismatch fails before request construction rather than carrying
  account-scoped state into another account.
- [ ] Make the returned stream/turn session own the HTTP producer and cancel all
  header, body, and SSE waits on receiver drop, user cancellation, or timeout.
- [ ] Reset or invalidate anchors whenever local compaction/state replacement
  makes provider history semantically incompatible.
- [ ] Preserve replayable reasoning for every supported anchor reset, or reject
  local replay and use server compaction/a semantically fresh thread. Never claim
  continuity after reconstructing a stored thread without its reasoning state.

### Phase-specific evidence

- [ ] A two-turn threaded test proves the second request cannot see the first
  request's replaced environment, rules, mode, timestamp, or prompt-command data.
- [ ] Stateless and threaded tests prove their intended effective context and
  top-level instruction behavior.
- [ ] Root and nested repository context, rule/profile bodies, user input, and
  trusted operator policy produce the exact D8 roles for root, spawn, and fork.
- [ ] `end_turn:Some(false)`, `Some(true)`, and `None` each have explicit tested
  loop semantics.
- [ ] Concurrent-agent and resume tests prove turn state never crosses user-turn,
  agent, session, cancellation, or error boundaries.
- [ ] Account-affinity tests prove anchors and turn state never cross credential
  identities, including explicit account selection and resumed sessions.
- [ ] A conflicting second turn-state value follows a documented tested rule
  rather than silently replacing or leaking the first value.
- [ ] A controllable local server promptly observes producer/socket termination on
  receiver drop, cancellation, and timeout, with no surviving task.
- [ ] Compaction and anchor-reset tests show local and provider-visible history
  remain semantically aligned.
- [ ] Stored-thread compaction tests prove reasoning continuity survives every
  allowed reset, while a backend without replay material fails before mutation.

### Review and exit gate

- [ ] Prompt/state and Codex-backend reviewers approve authority and lifetime rules.
- [ ] A Fable state-machine reviewer returns `READY` after multi-turn adversarial tests.
- [ ] Universal Gates A-D pass and all six findings close.

## P6. Transport, retry, and usage

**Status:** [ ] Not started; **findings closed:** `TRANS-02`, `USAGE-01`,
`NF-3`, `NF-5`, `ROUTE-01`; **foundation for:** `CACHE-02`; **dependencies:**
P5, D4, and D10.

### What this phase fixes

HTTP and in-stream rate limits have different retry paths, mapped errors do not
immediately stop the producer, and failed/missing/cache-write usage is discarded
or collapsed to zero. P5 has already made cancellation own the producer; this
phase makes retry, terminal network shutdown, and attempt accounting consistent.
It also records whether applicable current product terms and an authoritative
product/contract interpretation permit account switching in response to
exhaustion. Only if permission and technical safety are both established can an
explicitly allowed account be selected without crossing turn, execution,
workspace-policy, or account-state boundaries. Arbitrary failures are never
rotation signals.

### Difference after the phase

One component owns retry budget and attempt classification regardless of whether
failure arrives as an HTTP status or SSE event. Every attempt has one terminal
outcome. Observed provider usage distinguishes absent, zero, successful, failed,
cancelled, cache-read, and cache-write values; attempts without terminal usage
remain explicitly unknown rather than being called billed or estimated silently.
If D10, applicable terms/product guidance, and live validity evidence permit
automatic account selection, it occurs only before request dispatch or after a
typed provider result that guarantees rejection before execution and state
mutation. Absence of observed output is not such proof. It uses an explicit
trusted allowlist and starts with clean account-scoped turn state. Otherwise
automatic rotation remains unsupported while P2 manual selection remains.

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
- [ ] Resolve `ROUTE-01` under D10. Recheck the governing terms at implementation
  time and obtain an authoritative product/contract determination before
  implementing exhaustion-triggered switching. Account identity is pinned for a
  dispatched request and turn. Only pre-dispatch selection, or a typed
  account-specific outcome guaranteeing no execution or state mutation, may
  trigger a candidate change; arbitrary 401/429/timeout/5xx input and absence of
  observed output are insufficient.
- [ ] Restrict any candidate account set to a trusted user allowlist. Repository
  or model input cannot broaden it. Changing account clears Codex turn state,
  clears or disables prior cache affinity until P8 supplies the final
  account-isolated cache policy, and requires explicit authorization when
  resuming a session owned by another account.
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
- [ ] Account-routing fixtures prove no second dispatch unless the first was
  prevented or authoritatively rejected before execution/state mutation, no
  failover on generic transport/auth failures, correct turn-state clearing,
  cache-affinity disabling, trusted allowlist enforcement, and explicit resume
  authorization. If rotation is unsupported, the capability is absent and the
  typed exhaustion result tells the operator to select another account.

### Review and exit gate

- [ ] Async/concurrency, reliability, and usage/accounting reviewers approve.
- [ ] A Fable adversarial reviewer inspects terminal races, retry ownership, and
  usage claims and returns `READY`.
- [ ] Universal Gates A-D pass and transport/usage findings close.

## P7. Request, schema, and model controls

**Status:** [ ] Not started; **findings closed:** `MODEL-01`, `ROLE-02`,
`TOOL-01`, `REQ-01`, `SCHEMA-01`, `STRUCT-01`; **dependencies:** P0-P2, P4,
P6, and D5.

### What this phase fixes

Request construction overrides catalog reasoning-summary defaults, advertises
parallel capability while forcing serial calls, can lower schemas into dangling
`$ref` values, unconditionally emits a reasoning shape for unknown/non-reasoning
models, ignores catalog-selected apply-patch/search envelopes, silently collapses
compatible Developer into System, and implements structured output only through
a synthetic tool. Tool-backed slash commands can also forge an assistant call
without local dispatch or a matching result.
More generally, advertised capability is not consistently tied to complete
request/stream/persistence/replay support.

### Difference after the phase

One immutable request profile is resolved from backend, API shape, and model
before serialization. Unsupported combinations fail locally. Schema lowering is
validated and never emits dangling references. Parallel calls are enabled only
after ID-based correlation is proven. Structured output follows D5 by API shape
rather than an accidental universal workaround. Catalog tool types control the
whole wire/dispatch/replay path, compatible role downgrade is explicit, and slash
commands cannot invent provider history.

### Work checklist

- [ ] Use the P1 baseline to decompose any touched over-limit
  builder/config/request file before adding behavior.
- [ ] Resolve backend/model defaults and capabilities once before payload assembly.
- [ ] Honor catalog reasoning effort/summary defaults, supported effort values,
  summary/encrypted-replay support, and newer typed reasoning controls. Do not
  send reasoning fields to an unknown/non-reasoning model without explicit
  trusted capability configuration. Represent Norn-disabled parallelism
  separately from provider capability.
- [ ] Correlate all call completion by stable item/call identity before enabling
  parallel tool calls for a capable model.
- [ ] Preserve or inline required `$defs` during schema lowering and validate the
  result locally; reject unlowerable schemas with a typed diagnostic.
- [ ] Implement D5 for native `text.format` and intentionally synthetic-tool cases.
- [ ] Resolve `apply_patch_tool_type` and `web_search_tool_type` from the same
  immutable request profile and carry the selected envelope through parsing,
  dispatch, output echo, persistence, and replay.
- [ ] Preserve Developer on compatible backends that support it. For backends
  that cannot, apply the D8-approved explicit downgrade/rejection rather than
  silently serializing Developer as System.
- [ ] Replace `SlashCommandHandler::Tool` transcript fabrication with normal
  authorized local dispatch/persistence/result echo or a user-role model request.
- [ ] Reject raw provider-option collisions with Norn-owned typed fields.
- [ ] Advertise a hosted/native capability only when its request, stream,
  persistence, replay, and user-surface path is complete.

### Phase-specific evidence

- [ ] Payload snapshots cover Codex subscription, public Responses, trusted
  configured backends, reasoning/non-reasoning models, service tiers, and tools.
- [ ] Unknown models and every catalog reasoning effort/summary/tool-envelope
  combination either serialize the approved shape or fail locally before I/O.
- [ ] `$defs`, nested `$ref`, union, unsupported, and adversarial schemas either
  produce valid deterministic output or a typed local rejection.
- [ ] Randomized/interleaved parallel calls correlate every output by ID.
- [ ] Native and synthetic structured-output cases have explicit, tested selection
  and equivalent schema/error guarantees where both are supported.
- [ ] Compatible role snapshots prove Developer is preserved or follows the
  explicit downgrade contract. Slash-command tests cover dispatch success,
  rejection, cancellation, persistence, and resume without an orphan call.
- [ ] Unsupported capability and raw-option collision tests fail before network I/O.

### Review and exit gate

- [ ] Provider/catalog, JSON Schema, and tool-protocol reviewers approve.
- [ ] A Fable API-shape reviewer compares payloads with pinned official schemas
  and returns `READY`.
- [ ] Universal Gates A-D pass and all six findings close.

## P8. Prompt-cache measurement and policy

**Status:** [ ] Not started; **findings closed:** `CACHE-01` through `CACHE-05`;
**dependencies:** P4, P6, P7, D6, and D10.

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
- [ ] Namespace cache identity by the opaque selected credential without
  exposing or fingerprinting account IDs, emails, tokens, or other reusable
  identity data. Account switches never reuse local cache affinity accidentally.
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
- [ ] Compare private ChatGPT/Codex and public Responses separately. Record actual
  timestamps and throttle each key to the current guide's approximately 15
  requests/minute per-key guidance (reverified at execution); do not burst a
  20-call loop and call the resulting routing/cache behavior representative.

### Phase-specific evidence

- [ ] Offline payload tests prove byte-stable instructions, tool ordering/schema,
  item ordering, and key selection when inputs are stable.
- [ ] Persistent, ephemeral, spawned, and forked executions prove intended
  uniqueness, inheritance, collision resistance, and lifetime without leaking a
  raw key or a durable low-entropy hash.
- [ ] Two account identities prove cache affinity is isolated across explicit
  selection and any D10-approved rotation without exposing an account identifier
  in keys, telemetry, or evidence.
- [ ] The live experiment records backend/model, request number, keyed
  pseudonyms, ordered item types, input/output tokens, cached reads, cache-write
  presence/value, first-event latency, completion latency, and a locally
  generated experiment-row ID. It does not record an upstream request header.
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

Populate only the `Phase base` cell at Gate A. Update the remaining cells only
after the phase's final fix-round review.

| Phase | Phase base | Implementation commit(s) | Finding evidence and full-gate results | LOC/bypass policy report | Domain reviewer | Fable verdict | Status |
|---|---|---|---|---|---|---|---|
| P0 | `41ea210` | | | | | | [ ] |
| P1 | | | | | | | [ ] |
| P2 | | | | | | | [ ] |
| P3 | | | | | | | [ ] |
| P4 | | | | | | | [ ] |
| P5 | | | | | | | [ ] |
| P6 | | | | | | | [ ] |
| P7 | | | | | | | [ ] |
| P8 | | | | | | | [ ] |
| P9 | | | | | | | [ ] |

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
