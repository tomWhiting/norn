# Responses API remediation plan

- **Status:** Active; P0 accepted, P1 Gate A complete and Gate B foundation
  next. The focused independent
  correction review committed at `7ce29d7` returned `READY` for source head
  `e1bf7f2` and the full `41ea210..e1bf7f2` seam sweep. It independently
  reproduced Gate C 38/38, distributions 830/830, the full-range policy, and
  zero-error attestation, and closed GD-1 through GD-18. P1 uses `2917c8e` as
  its prospective base. The owner has deferred D0 remote merge enforcement to
  P1 exit; checked-in local enforcement and retained evidence remain mandatory,
  and no remote-protection claim is permitted while D0 is open. P2
  implementation candidate is complete through source `448353d`; retained Gate C,
  the live A/B/A experiment, the P1 dependency, and independent P2 acceptance
  remain open.
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
  final code range, machine/policy evidence, failed-first-run disclosure,
  owner dispositions, residual boundaries, and reviewer packet are recorded in
  the [`whole-phase Gate D handoff`](reviews/2026-07-14-p0-whole-phase-gate-d-handoff.md).
  The resulting
  [`whole-phase Gate D review`](reviews/2026-07-14-p0-whole-phase-gate-d-review.md)
  is the controlling `NOT READY` verdict for the correction round.
  The replacement exact-head evidence and focused review scope are recorded in
  the [`P0 correction handoff`](reviews/2026-07-14-p0-correction-gate-d-handoff.md).
  The final
  [`P0 correction Gate D review`](reviews/2026-07-15-p0-correction-gate-d-review.md)
  is the controlling `READY` verdict and accepts P0.
  The subsequent
  [`P0 acceptance evidence supplement`](reviews/2026-07-15-p0-acceptance-supplement.md)
  makes the exhaustive manual Rust-policy, writer-family, and sensitive-data
  inspections explicit without changing or expanding that verdict.
  The superseded [`P0 final candidate`](reviews/2026-07-14-p0-final-candidate.md)
  remains the historical `bfa0b8e` record. The
  [`historical Gate C handoff`](reviews/2026-07-11-p0-gate-c-handoff.md) is
  retained for the earlier review chronology. A separately
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

P0's scoped application of these invariants is accepted in the evidence ledger
and final review at `7ce29d7`. The program-wide boxes below remain open because
P1-P9 and final integrated closure are incomplete; they must not be blanket-
checked merely because P0 passed.

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
staged prepublication enforcement, post-mutation/completion feedback, a
checked-in local clean-checkout gate, and the eventual D0-selected remote
enforcement mechanism. Advisory output alone is never acceptance evidence for
this program.

## Universal phase gate

Every phase must satisfy all four gates below.

Gates A-D below are the active P1 dashboard. P1's prospective base is
`2917c8e`; none of P0's checked gates or retrospective exceptions carries into
this phase.
The prior automated workspace Gate C, full-range policy, 750-observation
distribution, and mechanical attestation remain historical evidence at
`13d661c`; the `c6bf1e2` whole-phase review invalidated them as P0 acceptance
evidence. Replacement machine evidence has now been generated from clean exact
source head `e1bf7f2` under the main repository's ignored `target/` lanes.
The P0 machine and reviewer results remain historical evidence in its ledger
row. Every P1 Gate B-D item below has been reset and must be satisfied against
the P1 base.

### Gate A: entry and design

- [x] All dependency phases are complete.
- [x] The exact phase-base commit is recorded before implementation so every
  diff, LOC, lint, and added-line audit has one reproducible comparison range.
  P1's phase base is the accepted P0 closure commit `2917c8e`.
- [x] The phase's owner decisions are recorded before implementation. The D0
  entry disposition permits local P1 work while remote enforcement remains an
  explicit exit blocker; it does not waive any local gate or evidence.
- [x] Its finding IDs, invariants, production touch points, and defect-regression
  or measurement/design evidence method are agreed by the implementer and
  domain reviewer.
  Ratified artifacts are the
  [`P1 Gate A contract`](reviews/2026-07-15-p1-gate-a-contract.md),
  [`repository-policy contract`](reviews/2026-07-15-p1-policy-contract.md), and
  [62-row evidence preregistration](reviews/evidence/p1/finding-traceability.jsonl).
  The independent
  [`Gate A review`](reviews/2026-07-15-p1-gate-a-review.md) returned `READY` on
  2026-07-15 without making an implementation or test claim.
- [x] Any live credential use, external call, or billable experiment has
  separate owner approval before it runs. P1 permits official documentation
  retrieval but no live provider request or credential experiment.

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

Tom approved that exact P0-only exception on 2026-07-14, as recorded in
`DECISIONS-2026-07.md` section 11. The two boxes remain unchecked because their
historical timing claims are still false; the recorded exception now satisfies
the P0 evaluation rule above.

### Gate B: implementation

- [ ] Confirmed-defect regressions fail for the documented reason on the reviewed
  baseline; measurement/design work has its pre-registered baseline and contract.
- [ ] The production fix is complete across request, stream, persistence,
  replay, loop behavior, and user surface wherever the capability crosses them.
- [ ] Replaced paths and temporary scaffolding are deleted in the same phase.
- [ ] Changed production files satisfy the file-size and module-structure rules.
- [ ] The finding-to-test traceability table is updated.

P0 evidence note: the candidate traceability matrix is recorded in
`2026-07-12-p0-traceability.md` and includes the later D1D/SEC-14 row. The
historical audit still cannot prove each confirmed regression failed for the
documented reason on `41ea210`; passing candidate tests do not retroactively
prove that claim. Tom approved the P0-only Gate B disposition on 2026-07-14:
retained source or positive-characterization proof plus exact candidate
regressions may substitute where no native pre-fix executable state exists.
The native `openat` red-green sequence remains required, and the unchecked
historical box is not relabelled as true.

### Gate C: machine verification

- [ ] Phase-specific tests pass.
- [ ] Every crate touched in the round passes its complete integration surface
  with `cargo test -p <crate> --tests`; a focused `--lib` run cannot substitute
  for this per-round fence. Concurrency-sensitive cases additionally use the
  distribution requirements below.
- [ ] `cargo fmt --all -- --check` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace --all-targets` passes.
- [ ] `cargo test --workspace --doc` passes.
- [ ] `git diff --check <phase-base>...HEAD` passes. Running bare
  `git diff --check` on the required clean checkout is not evidence.
- [ ] The syntax-aware repository policy command covers every relevant Rust item,
  reports exact production LOC and module/entrypoint shape, enforces the legacy
  baseline, and fails on prohibited additions.
- [ ] The same policy semantics run as a non-downgradable staged hard failure
  before first-party mutation publication, in post-mutation/completion feedback,
  and in the checked-in local clean-checkout gate. Remote enforcement is not
  claimed while D0 remains open.
- [ ] A `git diff --no-ext-diff 2917c8e...HEAD` added-line audit reports zero
  campaign-added unwrap, expect, panic, suppression, ignored-test, or
  unresolved-marker uses.
- [ ] The checked-in evidence-redaction validator passes across every P1 fixture
  and retained evidence artifact;
  no credential, real account identifier, private prompt content, reusable turn
  state, or raw cache key is present.

Gate D invalidated the original two test claims: the convergence regression
failed 6/10 isolated reviewer runs and 19/20 subsequent independent repetitions.
The original single passing workspace invocation remains a historical
observation, not stability evidence. Subsequent scoped evidence recorded 50/50
for each macOS-sensitive concurrency regression and 20/20 distributions for
D1E, startup/live MCP, prior corrections, and PTY behavior. Those scoped runs
are now incorporated into the exact 750-observation final distribution rather
than being used as substitutes for an integrated gate.
The first integrated workspace attempt is also retained honestly: it exposed PTY
allocation and resize-harness nondeterminism, which was corrected before the
green `f788823` rerun. The later final Gate C attempt at `82e44f4` is retained as
34/35: it exposed a delegated-browser test race between file creation and
sentinel contents. `bfa0b8e` corrects that test contract; the final result is
35/35 plus 750/750 repeated observations. Any `all`, `every`, or `complete`
coverage claim still requires the exact mechanically generated inventory it
quantifies.

H-1 subsequently pinned the repository toolchain to the already-proven Rust
1.94.0 at `13d661c`. A sandboxed clean run was retained as 28/35. The implementer
observed loopback `bind` failures with `PermissionDenied` in its failing output;
the hash-only artifact independently proves the seven failed runner cases, not
that diagnosis. The exact native-host rerun passed 35/35 and 9,205 Rust test
executions. The independently
attested 750/750 distribution and unchanged 333-file/97-writer policy are bound
to the same clean head. The full chain and failure classification are in
`2026-07-14-p0-whole-phase-gate-d-handoff.md`.

The correction chain supersedes those results for the active candidate. Its
first exact-head attempt at `17a3bb8` passed 35/38 runner cases and exposed two
test-fixture assumptions plus one stale provenance expectation; that artifact
also demonstrated that Cargo 1.94 backtick rerun hints were not yet qualified
by package/target. The corrected source head `e1bf7f2` passed 38/38 Gate C cases
and 9,299 Rust test executions. Its repeated suite passed 830/830 observations
and 1,250 Rust test executions. The full-range policy covers 359 changed Rust
files, 65 test-only files, and 97 writer candidates with zero prohibited
additions, over-500 production files, module-shape violations, or
thin-entrypoint violations. The mechanical attestation binds all three
artifacts to the same clean head with zero errors.

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
| P0. Credential and workspace authority containment | [x] Accepted by focused Gate D review `7ce29d7` on 2026-07-15 | Repository data cannot select credential/backend/process authority, escape the immutable workspace root, or create non-private artifacts. |
| P1. Contract and enforcement baseline | [ ] Gate A complete at base `2917c8e`; Gate B foundation next; D0 remote enforcement deferred to exit | The program has executable contracts and protected quality gates. |
| P2. OAuth lifecycle correctness | [ ] Implementation candidate through `448353d` complete; retained Gate C, live A/B/A, P1 dependency, and independent acceptance open | Login, refresh, storage, and logout fail safely; named-account selection is evidence-backed and explicit. |
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

An open owner decision blocks Gate A unless the owner explicitly records an
entry disposition that defers it to phase exit. A deferred decision still
blocks phase acceptance and cannot be represented as implemented evidence.

| ID | Required decision | Due | Status / record |
|---|---|---|---|
| D0 | Remote CI/merge-enforcement platform and required-check wiring. The checked-in local gate is phase work, not a substitute for remote enforcement. | P1 exit | [ ] Open for P1 exit. On 2026-07-15 the owner declined GitHub Actions for now and authorized P1 to proceed locally. No GitHub Actions workflow or remote-protection claim may be introduced under that ruling. P1 must build and retain evidence from one deterministic local clean-checkout gate, then revisit D0 before `READY` to select another remote mechanism, approve GitHub Actions later, or make a new explicit enforcement decision. |
| D1 | Exact compiled OAuth authority/path allowlist, redirect, workspace filesystem/command, and private-artifact policy. Custom trusted-proxy and repository-command consent are out of P0 scope. | P0 | [x] Decided 2026-07-10 and refined by the P0 threat model and provisional review round: accept no override or normalized `https://chatgpt.com[:443]/backend-api/codex[/]`, discard accepted input in favor of the compiled URL, and follow no redirects on credential-bearing clients. Both CWD layers are untrusted even when gitignored. Direct/raw provider authority, backend-selecting aliases, command-bearing hooks/rules, workspace skill shell, convention process categories, and model-selected profile prompt commands are rejected before use. One immutable launch root uses no-follow workspace reads/enumeration on supported descriptor-capable Unix targets; repository symlinks are unsupported, and Redox, ESP-IDF, and non-Unix workspace input fail closed. Debug-dump and private-artifact hardening are implemented on the same supported target class, including descriptor-pinned ancestor traversal, foreground and process spools, task storage, and removal of relative sensitive-data fallbacks; unsupported targets fail before artifact I/O. Pre-existing private-artifact links/non-regular entries are rejected, and read-only reopen hardens traversed legacy directories and regular files. Under a concurrent same-UID final-name replacement, the portable invariant is descriptor confinement: the replacement is never followed, no outside target is read/written/deleted, and the operation either fails or affects only the raced entry inside the pinned private root; POSIX does not provide portable strict rejection/serializability against that actor. Response headers remain fully redacted under D1, and redirect refusal follows the structural policy in `DECISIONS-2026-07.md` section 7. Custom API-key endpoints require HTTPS except loopback HTTP. No trusted proxy or implicit project-command consent is admitted. The final review and phase gates passed at `7ce29d7`. |
| D1A | Non-disclosing representation for unknown provider terminal discriminators: equality semantics, keying, identifier lifetime, output size, and deterministic test control. | P0 | [x] Decided 2026-07-11: known values retain typed mappings; an unknown exact byte sequence is represented only by its terminal category and a domain-separated full HMAC-SHA-256 tag under an OS-random process-lifetime key. The key and raw value are never persisted or logged. A deterministic key seam is crate-private and compiled only under `cfg(test)`; production exposes no public, configuration, or environment override. OS-random initialization failure is a typed fail-closed diagnostic error, never a fixed-key fallback. Tags support equality only within one process and are not cross-run fingerprints. Raw value and byte length are not exposed. |
| D1B | Location and access policy for fetched and other session-derived artifacts. | P0 | [x] Decided 2026-07-11: fetched documents are private session-owned artifacts beneath the trusted user-level Norn session store, never workspace files. P0 establishes a typed active-session artifact scope and migrates new fetch writes without pre-empting P3 transcript-format, historical-reference, fork-copy, or broad storage-migration decisions. Generic model file access may read/search only the active artifact subtree; it does not gain authority over credentials, indexes, raw child transcripts, or other sessions. |
| D1C | File-descriptor exhaustion mitigation introduced by descriptor-pinned private storage and persistent agent sinks. | P0 | [x] Mandatory per the 2026-07-11 post-review owner ruling: the official CLI raises its soft `RLIMIT_NOFILE` only to a finite OS-provided ceiling, reports inherited/effective limits and a labelled descriptor snapshot through `doctor`, and preserves typed `EMFILE` versus `ENFILE` diagnostics across the P0 private/session/process boundary. Library embedders do not receive an implicit process-global mutation. Structural descriptor sharing or lazy reopen remains an explicitly owned follow-up rather than being misrepresented as solved by a higher limit. `RLIMIT_CORE=0` remains a separate open decision because it also affects spawned user commands. |
| D1E | Structural descriptor closure after the owner rejected residual Norn-owned `EMFILE` risk. | P0 | [x] Decided, implemented, and independently accepted at `7ce29d7`. Idle session/history/process retention and eager spool-root probing are removed; cancellation-safe adoption owns process groups until spool attachment commits; and the process-wide fail-fast authority covers active/scalable process, spool, session, diagnostic, persistent stdio, LSP, HTTP, OAuth callback/browser, read/search, Rhai, debug, ordinary one-shot configuration, discovery, task, and write/edit/patch families. The former arbitrary transient headroom is replaced by exact observer reserve and typed filesystem/subprocess/HTTP permits. The final distribution and Gate D review cover selected descriptor-retention/admission, cancellation, live-transport release, and OAuth launcher permit lifecycles. This item does not claim that Norn can prevent unrelated embedder or operating-system-wide exhaustion. |
| D1D | Complete `NornSettings.mcp_servers` as the layered MCP client surface: user, shared project, private project-local, per-agent, CLI, and live-session scopes with remembered shared-project approval and dynamic tool-catalogue refresh. | P0 | [x] Owner decision confirmed by Tom on 2026-07-13 and attributed in `DECISIONS-2026-07.md` section 10; implementation and its complete startup/live-control fixture matrix were independently accepted at `7ce29d7`. Precedence is `session > CLI > local > project > user`; same-name entries replace wholesale. Only shared checked-in project definitions require definition-bound remembered approval; user-owned private, CLI, and live-session input is direct operator configuration. Root, variant, and spawned agents select views from the connected pool without treating MCP roots as confinement. Startup consumption, live add/remove/enable/disable/reload, contextual roots, and provider-visible tool refresh are implemented. |
| D2 | Existing session policy: explicit version rejection or an offline one-shot migration. Record format versioning, crash atomicity, idempotency, backup/recovery, old-binary behavior, and treatment of irrecoverably lossy history. | P3 | [ ] Open |
| D3 | Threaded-state policy: decide replaceable Developer context and whether/how local compaction may reset an anchor without losing stored reasoning. Select a genuinely replaceable surface, lossless replay contract, fresh-thread transition, or disable threading/local replay. | P5 | [ ] Open |
| D4 | Single retry owner and existing configured attempt/budget semantics for HTTP and in-stream failures. | P6 | [ ] Open |
| D5 | Native `text.format` versus synthetic tool policy by API shape, catalog-selected apply-patch/search envelopes, and local-dispatch versus user-request semantics for tool-backed slash commands. | P7 | [ ] Open |
| D6 | Pre-register the cache experiment: ratify or replace the proposed 20-iteration design; approve public/private backends, models, spending, warm-up, key isolation/reuse, an approximately 15 requests/minute per-key ceiling rechecked against current guidance, concurrency, retention/cooldown, service tier, output/effort controls, randomization, primary measures, and statistical treatment. | P8 | [ ] Open |
| D7 | Approve credentials, spending, redaction, and retention for final live Codex and public Responses conformance. Without approval P9 is blocked, not passed by a skipped test. | P9 | [ ] Open |
| D8 | Ratify the source-to-wire-role matrix for product System policy, trusted operator Developer policy, repository `NORN.md`/rules/profiles, user input, and compatible backends that cannot preserve Developer. | P5 | [ ] Open |
| D9 | OAuth credential ownership and explicit named-account policy: Norn-managed storage; file-backed foreign `$CODEX_HOME/auth.json`; import/migration semantics; OS-keyring scope; static/embedder ownership; trusted selection; unknown expiry; durable recovery-journal policy; accepted `provider.auth` spellings and required/forbidden companion fields; and isolated-account validity. | P2 | [x] Decided 2026-07-16. P2 implements explicit Norn-owned named accounts while retaining `$NORN_HOME/auth/auth.json` as the legacy `default` slot. A private versioned catalog maps case-insensitively unique shell-safe aliases to opaque random storage IDs below `$NORN_HOME/auth/accounts/`; aliases are never paths. Successful named login becomes active for newly constructed providers; `auth use` changes only future providers; deleting the active account clears selection and never auto-selects another. Before P5, resume requires explicit trusted account selection. Project/local/model/tool input cannot select an account. The live A/B/A refresh experiment is required before P2 acceptance, but does not block implementation. P2 adds no foreign-file import or OS-keyring surface and never reads, locks, hardens, copies, or deletes ambient `$CODEX_HOME`; those capabilities are out of P2. Restart-safe refresh recovery uses a private versioned, token-free, no-TTL marker under the same Norn-owned transaction lock; ambiguous same-lineage outcomes block replay until durable state proves commit or external advancement. Explicit `provider.auth` accepts only `oauth` and `api_key`; `env`, blank, and unknown values are rejected. Omitted mode preserves current backend defaults. Explicit OAuth forbids an API-key environment field; explicit API-key mode requires one; incompatible backends reject invalid combinations before environment, credential, provider, or network access. Automatic account rotation remains outside P2. |
| D9A | Credential-transaction timing policy: the default acquisition deadline and the positive polling cadence used by portable timed file-lock acquisition. Both must have explicit owner-approved values, and the cadence must become programmatically overridable like the deadline. | P2 | [x] Decided and implemented 2026-07-15. The default acquisition deadline is 30 seconds and the inter-process polling cadence is 25 milliseconds; both are programmatically overridable. Both must be positive and are rejected before credential filesystem access when invalid. The deadline bounds acquisition only, not the duration of a transaction after the lock is held. Source commit `455990a`; retained 20-iteration deadline and two-process convergence distributions are recorded in `docs/reviews/evidence/2026-07-15-p2-credential-lock.json`. |
| D10 | Automatic account rotation policy: applicable product/contract permission, eligible exhaustion signals, trusted candidate allowlist, pre-request rejection proof, turn/session affinity, state reset, cache-isolation handoff, and resume authorization. | P6 | [ ] Open until authoritative current terms/product guidance permits the behavior and P3/P5 establish transcript replay, account-scoped state, and turn affinity. The current [OpenAI Terms of Use](https://openai.com/policies/terms-of-use/) prohibit circumventing rate limits or restrictions, so exhaustion-triggered rotation is unsupported unless OpenAI or the governing contract explicitly establishes that this use is permitted. Even then, switching occurs only before dispatch or after a typed provider outcome proving no execution or state mutation; absence of observed output is insufficient. P6 otherwise keeps `ROUTE-01` unsupported. |

## Accepted boundary and operator guidance

P0 now rejects working-directory attempts to select credential, backend,
endpoint, ambient-secret, process, and automatic-file-read authority, and it
stores covered session/process artifacts privately. Codex OAuth remains valid
only for the compiled canonical Codex destination. Custom Responses endpoints
must use explicit trusted user-level or CLI API-key configuration and HTTPS,
except for a loopback-only local service. Until P2 closes `CONFIG-01`, do not
treat `provider.auth` as an authority selector; use the validated provider and
credential configuration surfaces.

## P0. Credential and workspace authority containment

**Acceptance:** [x] Accepted by the focused independent Gate D review committed
at `7ce29d7` on 2026-07-15.
**Implementation status:** the original 33 work items, F1, D1B, D1C,
D1E structural descriptor closure, D1D startup and live control, and the GD-1
through GD-18 correction implementation are complete at source head `e1bf7f2`.
The historical Rust candidate runs through `13d661c`, with its raw evidence in
`564af2d`; the P0-only Gate A/Gate B dispositions remain recorded. The fresh
whole-phase review at `c6bf1e2` returned `NOT READY` and invalidated that earlier
candidate as acceptance evidence. Replacement Gate C, distribution, policy,
and attestation artifacts now pass at `e1bf7f2`. The owner-approved GD-15
current-head deletion and historical provenance record are complete. The final
review independently reproduced the machine evidence, completed the deferred
seam sweep, and accepted D1E and the D1D fixture matrix. D1, D1A, D1B, D1C,
D1D, D1E, and the retrospective dispositions are closed for P0.

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
fixes were subsequently accepted by whole-phase Gate D at `7ce29d7`.

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
  the external reviewer accepted the slice in the
  [`D1B review`](reviews/2026-07-12-p0-fetch-artifact-review.md). Its two
  non-blocking code findings were corrected in `9f35cfd`; whole-phase Gate D
  subsequently accepted the result at `7ce29d7`.
- [x] Implement D1C for the official CLI, `doctor`, and typed P0
  private/session/process error paths. Record descriptor-allocation boundaries
  and do not claim coverage beyond the inventory. Implementation, retained
  descriptor inventory, focused tests, and the non-goals are recorded in the
  [`D1C correction record`](reviews/2026-07-12-p0-nofile-correction.md).
- [x] Implement D1E structural descriptor closure under the stricter 2026-07-13
  owner ruling. Independent acceptance is recorded below and at `7ce29d7`.
  - [x] Remove descriptor retention from idle session sinks, session artifact
    stores, completed-process spools/managers, and finalized foreground output;
    verify lazy reopens against their originally bound inode before mutation.
  - [x] Admit in-process index-lock contenders before opening the private root
    and lock file; preserve the caller's total lock deadline across both local
    admission and cross-process locking.
  - [x] Replace production cleanup `kill` subprocesses with direct
    process-group signalling that needs no new descriptor under pressure.
  - [x] Preserve the original configured index-lock deadline in typed timeout
    reporting while polling only the residual cross-process budget; the
    formerly failing integration case passes 20/20.
  - [x] Re-exec low-`RLIMIT_NOFILE` regressions for 128 retained session sinks,
    128 retained spools, and a registry of 200 completed real processes.
  - [x] Govern active descriptor-heavy work through one process-wide weighted
    authority, including foreground/background processes, watch filters, shell
    hooks, persistent stdio MCP/extensions, and active/idle HTTP sockets.
  - [x] Treat an open-time synthetic interrupted-tool result as a provider
    response-thread boundary: the first healed request fully replays without a
    stale `previous_response_id`, then normal threading may resume from the next
    response. The durable-state request fixture passes 20/20; no live-provider
    or literal-SIGKILL claim is made.
  - [x] Remove idle descriptors from private line logs and eager process-spool
    probing; use a cancellation-safe adoption guard so a process group cannot
    survive cancellation before spool attachment commits.
  - [x] Replace transient emergency headroom with exact observer reserve and
    typed filesystem, subprocess, and HTTP admission; cover recursive walks,
    one-shot configuration/read/write paths, OAuth storage, task transactions,
    diagnostics, LSP, and TUI discovery without nested permit acquisition.
  - [x] Independently accept the candidate proof of permit transfer and release
    on success, spawn failure, timeout, cancellation,
    foreground-to-background adoption, transport drop, and shutdown under
    repeated low-limit runs; reconcile the complete inventory.
- [x] Implement the D1D startup slice under the section 10 ruling: preserve source scope through
  resolution; redact secret-bearing definition state; remember approval for a
  canonical project plus normalized shared-project definition; connect selected
  startup servers; install server-qualified tools per agent; and prove an
  unapproved project definition causes no process or network activity. Candidate
  evidence is recorded in `2026-07-13-p0-d1d-mcp-startup-candidate.md`; external
  acceptance is recorded at `7ce29d7`.
- [x] Re-pin the D1D empty-extension integration regression at
  `resolve_invocation`, the shared production validation boundary used before
  builder assembly. The focused test passes 18/18 and the complete touched
  `norn-cli` integration surface passes; exact suite counts and commands are in
  `2026-07-14-p0-g1-correction.md`. This slice alone closed status-report finding
  G-1; the later whole-phase review accepted D1D.
- [x] Add live list/inspect/add/remove/enable/disable/reload, child-only
  connections beyond the startup pool, dynamic roots, and request-boundary
  provider-tool refresh as a separate reviewable MCP slice. The serialized
  controller publishes an immutable runtime/generation pair; CLI and TUI share
  one redacted `/mcp` parser; children derive per-agent views from the complete
  pool at each request boundary; contextual roots and calls are serialized per
  shared client; and `tools/list_changed` rediscovery retains the prior pair on
  failure. Watchers use weak actor senders and are aborted when their client is
  removed. Repeat-distribution and policy evidence is retained with the slice.
- [x] Close the final provider/request/SSE/tool-result disclosure and OAuth
  callback/browser lifecycle gaps in `e218c9c`; remove the remaining prohibited
  test-result extraction in `8299df0`; add an exact, adversarially checked
  evidence contract in `82e44f4`; and correct the delegated-browser sentinel
  race in `bfa0b8e`. The exact retained `bfa0b8e` record is
  [`2026-07-14-p0-final-candidate.md`](reviews/2026-07-14-p0-final-candidate.md),
  and the superseding pinned-toolchain record is the
  [`whole-phase handoff`](reviews/2026-07-14-p0-whole-phase-gate-d-handoff.md).
- [x] Delete or demote `session_file_path` and
  `resolved_session_file_path`; no production-compatible raw path derivation may
  remain beside the validated replacement. Both helpers are now `cfg(test)` and
  crate-private, with no production re-export.
- [x] Make `PrivateRoot` ancestor-creation behavior, identifier names, and
  documentation agree. Do not silently relabel behavior whose missing-mount
  semantics require a policy decision. The identifier/doc correction and
  retained policy boundary were independently accepted in the
  [`openat correction review`](reviews/2026-07-12-p0-openat-correction-review.md).
- [x] Add body-never-read/non-disclosure sentinels for specialized 401 and 429
  responses, plus loop-level timeout and lossless `try_send` to awaited `send`
  handoff regressions. Raw stalled-body fixtures prove the specialized statuses
  return from headers without waiting for or rendering the body; loop tests pin
  timeout-exit sweeping and the capacity-one retained-message handoff.
- [x] Regenerate production LOC with one syntax-aware method that excludes
  test-only items wherever they occur. Rebuild the bypass and artifact-writer
  inventories with exact commands and inputs. The provisional code-head
  snapshot at `37c806a` records 122 changed Rust files, no production file over
  500 LOC, no thin-entrypoint violation, zero added bypass matches, and 88 raw
  writer candidates. The scoped D1D regeneration over `5015e79..a949af1`
  likewise records zero changed production files over 500 LOC, zero
  thin-entrypoint violations, and zero added bypass matches. The final full-P0
  regeneration at `f788823` covers 227 changed Rust files and 92 writer
  candidates. The final regeneration at `bfa0b8e` supersedes those mutable
  totals: 333 changed Rust files, 62 test-only files, zero over 500, zero
  thin-entrypoint violations, zero added policy matches, and 97 enumerated
  artifact-writer candidates.
- [x] Complete the attainable baseline-evidence audit and finding-to-test
  traceability records,
  then correct rather than append to the invalidated Gate C handoff. The exact
  candidate matrix now exists in the
  [`traceability record`](reviews/2026-07-12-p0-traceability.md), but it honestly
  labels historical source proof where no pre-fix executable run was retained;
  the [`baseline-evidence audit`](reviews/2026-07-12-p0-baseline-evidence-audit.md)
  proves Git contains only one native defect-red/corrected-green sequence and
  that a P0-only Gate B process exception is unavoidable. The D1D/SEC-14 row is
  populated, and Tom approved the exact P0-only exception on 2026-07-14 without
  relabelling the missing historical red runs. Independent acceptance followed
  at `7ce29d7`.

### Whole-phase Gate D correction ledger

The review that opened this correction ledger is
[`2026-07-14-p0-whole-phase-gate-d-review.md`](reviews/2026-07-14-p0-whole-phase-gate-d-review.md),
committed as `c6bf1e2`. The final focused review at `7ce29d7` independently
verified every checked correction, completed the deferred seam sweep, and
returned `READY`.

- [x] **GD-1:** classify private project-local MCP settings separately from
  shared-project settings so they cannot acquire remembered approval.
- [x] **GD-2:** charge recursive-walk descriptor weight for every search mode.
- [x] **GD-3:** reject dead MCP clients during reuse and report liveness loss.
- [x] **GD-4:** preserve and trace typed control-plane causes without rendering
  untrusted remote error text or configured secret values.
- [x] **GD-5:** retry one failed tool-list refresh through reconnection, then
  atomically quarantine only the failed selected server if recovery fails.
- [x] **GD-6:** impose a positive per-server inbound limit across stdio, JSON,
  and complete SSE wire events. The exact 10 MiB default is derived from the
  official MCP TypeScript SDK v1 rather than invented locally; streaming and
  one-event-at-a-time fixtures cover pre-growth rejection, ignored SSE fields,
  multi-event chunks, and exact CRLF boundaries.
- [x] **GD-7:** remove the invented 30-second MCP timeout. A positive
  `request_timeout_ms` is optional, absence means no Norn-imposed timeout, and
  an explicit deadline covers the complete logical HTTP or stdio request. A
  paused-time client fixture advances past the former 30-second value.
- [x] **GD-8:** close the `mod.rs` production-logic policy gap, including
  production reachability through a second path alias, with checked-in fixtures.
- [x] **GD-9:** retain an HTTP MCP session identifier only from a successful
  response.
- [x] **GD-10:** retain only constant-space stderr content/completion categories
  without raw text, lengths, counts, or hashes; bind the observation task and
  descriptor permit to the transport lifecycle; and carry the safe summary
  through connection errors, runtime failures, and tracing.
- [x] **GD-11:** hydrate startup runtime statuses so live inspection and reuse
  reflect already connected servers.
- [x] **GD-12:** close every enumerated MCP/TUI failure path and explicitly
  dispose of abnormal front-task cancellation after actor enqueue.
- [x] **GD-13:** correct the descriptor-admission record's revalidation order.
- [x] **GD-14:** pin `claude_runner` to the revision whose spawn shape supports
  the descriptor proof.
- [x] **GD-15:** the corrected runner emits path-free evidence without removed
  ambient variable names, with local executable hash rebinding and exact-key
  attestation. Under `DECISIONS-2026-07.md` section 12, the six superseded
  schema-v2 reports are removed from current `HEAD` and preserved as immutable
  historical commit/hash references in the active correction handoff.
- [x] **GD-16:** refuse redirects for MCP HTTP and HTTP extension clients.
- [x] **GD-17:** retain structurally parsed failing-test identities, cover
  truncated output and Cargo 1.94 backtick rerun hints across split stdout and
  stderr, and run the parser tests inside Gate C.
- [x] **GD-18:** split CLI print-error logic out of the 499-line orchestrator.
- [x] The correction evidence runner pins the exact Python self-test module
  inventory and test count, hashes the actual pinned `cargo`/`rustc` binaries,
  rechecks their versions, and uses only fixed path-free failure codes.
- [x] Retained evidence rejects embedded Unix, Windows-drive, UNC, and
  `file://` paths; failed Rust tests are qualified by sanitized Cargo
  package/target identity, including doctests and truncated-output handling.
- [x] Policy reachability is grouped by file identity and covers external
  `#[path]` aliases plus checked-in literal `include!` targets; the checked-in
  fixture set includes production, test-only, shared, and case-alias controls.
- [x] Evidence worktrees, Cargo targets, outputs, policy scratch, independent
  reproduction, and the macOS `openat` reproducer resolve through the main
  repository's ignored `target/` lanes and use the canonical paths they check.
- [x] A read-only adversarial follow-up closed three pre-gate MCP seams: SSE
  chunks cannot batch unbounded parsed events or hide unbounded ignored fields,
  HTTP deadlines cover nested protocol handling, and hostile `Content-Type`
  values never enter rendered failures. Independent fingerprint, exact-CRLF,
  and no-implicit-30-second fixtures close the accompanying test gaps.
- [x] Apply the owner disposition for six superseded schema-v2 artifacts that
  retain historical local paths or ambient variable names. The active
  correction handoff enumerates all six paths, introducing commits, and hashes.
  They are deleted from current `HEAD`; already-pushed history is not purged.
- [x] Regenerate the complete gate, distributions, policy result, and
  attestation from the corrected exact head. All worktrees, build outputs,
  scratch data, and evidence must remain in ignored siblings under this
  repository's `target/` tree; external temporary directories are prohibited.
  Source head `e1bf7f2` passed Gate C 38/38, distributions 830/830, the
  359-file policy, and mechanical attestation with zero errors.
- [x] Obtain one focused independent correction review over these failure paths
  and the deferred whole-diff seam sweep. Review `7ce29d7` returned `READY`.

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
  recorded separately as non-blocking downstream integration evidence; this is
  not part of Norn's clean-checkout P0 acceptance claim.
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
- [x] Complete independent acceptance of the MCP fixture matrix. The startup
  candidate proves five-scope precedence, definition-bound approval/revocation,
  approved-project activation, zero activation while approval is pending,
  private-local stdio, independent failure isolation, protocol negotiation,
  ping, HTTP session/version headers, JSON/SSE POST, and end-to-end root/spawn
  selection. The live candidate adds serialized mutations, per-agent child
  views, contextual roots, catalogue refresh, watcher release, and descriptor
  release. The implementation gaps named by the earlier checklist are closed;
  independent adversarial reproduction and whole-phase acceptance are not.
- [x] URL tests cover HTTP, userinfo, case, trailing dots, default/non-default
  ports, lookalike hosts, path variants, redirects, and canonical URLs.
- [x] Capability/payload snapshots prove explicit and implicit canonical Codex
  selection have identical backend semantics.
- [x] Trusted API-key custom and compatible endpoints retain their intended,
  explicitly tested behavior; remote HTTP is rejected and loopback HTTP remains
  supported.
- [x] Debug-dump permission/symlink tests, OAuth feature-surface inspection, and
  the exact final manifest prove tested credential/provider-controlled
  sentinels do not reach the named ordinary error, log, Debug, or model-facing
  surfaces. Trusted opt-in raw dumps and structured correlation IDs are
  explicitly outside a blanket redaction claim.
- [x] Response-header dump fixtures prove the D1/NF-4 correlation decision is
  exact: every response-header value remains redacted and no credential, cookie,
  redirect target, turn state, or account metadata is exposed.
- [x] Malformed SSE, `response.failed`, and error-status sentinels prove provider
  text and control bytes never enter logs/errors; stalled generic error-status
  fixtures retain the existing typed timeout behavior while the body is
  streamed and discarded. Specialized 401 and redirect fixtures prove their
  response bodies are not disclosed; code-path inspection confirms those
  responses are dropped without draining.
- [x] A stalled 429 response body or explicit read-observer fixture proves the
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
- [x] TUI history uses the trusted Norn root and a narrow private line-log
  capability with `0700`/`0600` mode healing, no-follow regular-file opens,
  inter-process record locking, torn-tail recovery, corrupt-backing demotion,
  and prompt/draft Debug non-disclosure. Focused evidence is recorded in the
  [`history correction`](reviews/2026-07-12-p0-history-artifact-correction.md).
- [x] A complete private/session artifact-writer inventory names every writer,
  its ownership and lifetime, root, mode, no-follow/atomicity behavior, and model
  read surface. The
  [`classification`](reviews/2026-07-12-p0-artifact-writer-inventory.md) and raw
  JSON include fetched documents, TUI history, build output, user-directed
  writers, the foreign OAuth store, cleanup/read false positives, and the
  non-final Bash/process layout. The final 97 raw rows are retained at
  `bfa0b8e`; the five semantic additions after the historical 92-row snapshot
  are classified as operator-directed MCP settings mutation.
- [x] A retained concurrency evidence script records 50/50 successful session
  convergence runs plus primitive-level same-name-create, `O_EXCL`, and
  persistent-failure cases on the affected macOS platform.
- [x] A no-external-diff audit reports zero campaign-added unwrap, expect, panic,
  suppression, ignored-test, or unresolved-marker uses.

### Gate D-disposed residuals

The final reviewer verified these boundaries and classified none as a reachable
P0 defect. They remain explicit non-claims or later-phase compatibility and
design work rather than being silently promoted into the accepted outcome:

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
- MCP clients are active with source provenance and live mutation. Only shared
  checked-in project definitions require remembered approval; user-owned
  private, CLI, and live-session scopes are direct operator input. Dynamic
  contextual roots and active-SSE catalogue refresh are implemented. Idle
  standalone HTTP GET notification listening, reconnect/resumption, HTTP
  session DELETE, MCP OAuth, sampling, resources, and prompts remain outside
  this slice and are not claimed.
- Redox, ESP-IDF, and non-Unix workspace input deliberately fail closed. This
  protects the trust boundary but is a release compatibility limitation until
  equivalent no-follow filesystem primitives are implemented and reviewed.
- The macOS OAuth browser path keeps the authorization URL out of argv and the
  child environment by delivering it to fixed `/usr/bin/osascript` over stdin.
  The test proves command/JXA construction, not an end-to-end `NSWorkspace`
  invocation. Linux and the named BSD targets use fixed trusted opener paths
  but still expose the URL in process argv. Windows and other targets return a
  typed unsupported browser-login result.
- The retained Gate C is Darwin/macOS evidence. Cargo `--all-targets` covers
  target kinds on that host; it does not prove Linux/BSD/Windows compilation or
  runtime behavior. The APFS distribution is intentionally host-gated.
- The panic-conversion sentinel proves only a bounded structured model-facing
  failure. Rust's default panic hook may still write the panic payload to
  process stderr; the case is not classified as a secret/non-disclosure
  sentinel.
- Trusted opt-in raw debug JSONL intentionally contains request and wire data,
  and valid tool/call/item IDs remain structured correlation data. P0's exact
  manifest proves named sentinels are absent from the tested ordinary surfaces;
  it does not claim universal erasure of raw protocol data or identifiers.
- Evidence SHA-256 values and the attester provide content binding and
  deterministic mechanical validation, not a signature, trusted timestamp,
  transparency log, or authenticated independent execution provenance.

### Review and exit gate

**Current gate state:** the whole-phase review returned `NOT READY` at
`c6bf1e2`. It independently confirmed the credential-security core and rejected
the phase on the bounded GD-1 through GD-18 correction set recorded above. Its
own loaded Gate C run was red and exposed that the v2 evidence schema could not
name the failing tests; quiet reruns were green, but neither observation replaces
corrected exact-head evidence. The prior `13d661c` gate, distributions, policy
result, and attestation remain historical input only. Source head `e1bf7f2` now
has a fresh path-free evidence chain: Gate C 38/38, distributions 830/830,
full-range policy pass, and zero-error attestation. GD-15's historical-artifact
disposition is complete. The focused correction review at `7ce29d7`
independently reproduced the complete machine chain, inspected the deferred
seams, closed GD-1 through GD-18, and returned `READY`; P0 is accepted.

- [x] A security reviewer threat-models every credential destination, redirect,
  automatic working-directory command, and eager working-directory file read.
- [x] A provider/config reviewer verifies trust cannot originate in project data.
- [x] A Fable adversarial reviewer returns `READY` before this phase ships
  independently of later protocol work.
- [x] Existing fmt, strict Clippy, workspace tests, doc tests, diff check, and a
  reviewer-verified syntax-aware LOC/bypass inspection pass. P0 does not wait for
  the broader P1 policy infrastructure.
- [x] Universal Gates A-D pass and all P0-owned findings have closure evidence.
  For P0 only, the explicit owner-approved retrospective exception described in
  Gate A may substitute for its two unsatisfied historical timing requirements.

## P1. Contract and enforcement baseline

**Status:** [ ] Gate A complete and independently `READY` at phase base
`2917c8e`; Gate B foundation not yet implemented; **findings supported:** all;
**dependency state:** P0 accepted. D0 remote enforcement is owner-deferred to
P1 exit and still blocks acceptance, not local implementation.

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
deterministic checked-in clean-checkout gate. Staged mutation enforcement,
post-mutation/completion feedback, and the repository command use the same
policy semantics. D0 selects the remote enforcement mechanism before P1
acceptance. Later phases fail mechanically when they add/worsen debt or violate
campaign rules. No provider behavior changes in this phase.

### Work checklist

- [x] Ratify this plan and the source review's finding IDs and severity.
- [x] Record the public Responses documentation revision and official Codex
  source commit used as the conformance contract.
- [ ] Build sanitized fixtures for text, multiple assistant phases, encrypted
  reasoning, function/custom calls, refusal, hosted search and annotations,
  compaction, unknown reasoning parts/items, interleaved and duplicate call
  completion, malformed terminal data, `end_turn`, turn-state headers/metadata,
  failures, rate limits, incomplete streams, and cache usage.
- [x] Add a traceability preregistration mapping each confirmed defect to a
  planned regression
  and each unproven/design finding to its baseline and pre-registered contract.
- [ ] Add one syntax-aware policy implementation used by staged first-party
  mutation checks, post-mutation/completion feedback, and a repository command
  that fails on LOC, entrypoint/module shape, bypass, ignored-test, and
  unresolved-marker violations.
- [ ] Update or replace the contradictory `CONVENTIONS.toml` LOC/bypass path so
  there are not two authorities with different test-code semantics.
- [ ] Add policy fixtures, including inline `#[cfg(test)]` code, changed
  over-limit files, new/worsened debt, suppressions, and logic-bearing `mod.rs`.
- [ ] Replace the P0 writer-family seed regex with a reproducible method that
  distinguishes roots/openers, downstream handle mutations, cleanup, and false
  positives while mapping every operation to one owned artifact family.
- [ ] Create an explicit legacy baseline containing file, verified production
  LOC, owner, due phase/remediation record, and baseline identity. The checker
  fails on a new entry, growth, production edits, or an overdue entry. A touched
  active exception resolves only after the file is at or below 500 production
  LOC; its immutable origin record remains as audit history.
- [ ] Add a checked-in evidence-redaction validator for fixtures and evidence.
- [ ] Wire every Gate C command, the policy checker, and the redaction validator
  into one checked-in local gate running from a clean checkout. The eventual
  D0-selected remote mechanism must invoke that same entrypoint without weaker
  flags or omitted legs.
- [ ] Record toolchain, baseline commit, test counts, exact gate commands, and
  full verification results.

### Phase-specific evidence

- [ ] Every source-review finding has exactly one closure owner and the correct
  confirmed-defect or measurement/design evidence class.
- [ ] Redaction-validator negative fixtures prove credentials, account IDs,
  private prompt text, reusable turn state, and raw cache keys fail the gate.
- [ ] Policy-checker tests prove violations return non-zero, inline test items
  are excluded from production LOC, and an unchanged active baseline exception
  may remain only before its recorded due phase.
- [ ] The checker catches every prohibited bypass form and a logic-bearing
  `mod.rs` fixture, and post-mutation feedback reports the same result.
- [ ] The local gate runs all Gate C commands from a clean checkout and fails
  rather than advises. Before P1 acceptance, D0 records how the same entrypoint
  is enforced remotely or supplies a new explicit owner decision.
- [ ] Baseline commands pass. A pre-existing failure is fixed, not waived.

### Review and exit gate

- [ ] Security/auth, request/state, and streaming/item domain reviewers approve
  the fixture coverage for their original review areas.
- [ ] A fresh Fable architecture reviewer returns `READY` on the contract,
  shared policy implementation, local gate, D0 exit disposition, and phase
  ordering.
- [ ] Universal Gates A-D pass and P1 evidence is recorded.

## P2. OAuth lifecycle correctness

**Status:** [ ] Implementation candidate complete through source `448353d`; focused
OAuth and CLI gates are green, but no retained Gate C or P2 acceptance claim;
**findings targeted:** `AUTH-01` through `AUTH-07`, `CONFIG-01`, `CONFIG-02`;
**dependencies:** P1; D9 and D9A are decided. Checked work items below mean the
implementation and focused source fixtures are present; they do not substitute
for retained Gate C evidence or independent review. Gate A selected the
named-account branch. Its live-validity experiment and the P1 dependency must
close before P2 acceptance. Automatic account rotation is not in P2.

### Interim-review correction ledger

The independent interim review committed at `86d95aa` reviewed source head
`289d841`. These dispositions describe the correction working state; they are
not phase acceptance or retained candidate evidence.

The independent correction review committed at `7536436` returned `READY` for
the complete correction range through source `455990a`. That verdict closes
the seven interim findings and D9A only; it is not P2 acceptance and does not
close the P1 dependency, phase evidence, or exit gates. The later D9 decision
and implementation candidate are recorded below.

- [x] `P2-1` and `P2-2`: D9A records owner-approved defaults of 30 seconds for
  lock acquisition and 25 milliseconds for inter-process polling. Both are
  programmatically overridable; zero values fail before credential filesystem
  access. The acquisition deadline does not cap a transaction after lock
  ownership is established.
- [x] `P2-3`: an owned worker is observed by a structural supervisor; panic,
  abort, or completion-channel loss wakes every waiter with a typed
  `Indeterminate` outcome. Ambiguous dispatch blocks replay only in the live
  manager at this historical correction snapshot; source `4d51a36` adds the
  process/restart-durable journal described below.
- [x] `P2-4`: reclassified as a false premise for the current storage design.
  Descriptor-relative no-follow root opening rejects a symlinked credential
  root before registry insertion, lock creation, or authority I/O; no
  canonicalization that follows the rejected link was added.
- [x] `P2-5`: the process gate now uses non-poisoning synchronization rather
  than silently recovering a poisoned standard mutex.
- [x] `P2-6`: cancellation after accepting a callback returns the generic HTTP
  400 failure page, including partial-request and classification/claim seams.
- [x] `P2-7`: the default `.norn` product directory has one shared source.

Focused working-state validation on 2026-07-15 used the repository's normal
`target/`: `cargo fmt --all -- --check` passed; strict workspace/all-target
Clippy with `-D warnings` passed; the OAuth library slice passed 175/175; the
added-line policy scan found no lint suppression or prohibited panic/unwrap
calls; and every changed Rust file in this correction is at or below 492
physical lines. The checked-in D9A runner additionally recorded 20/20
process-local deadline passes and 20/20 two-process refresh-convergence passes
against source commit `455990a`. Except for that retained D9A distribution,
these are implementation checks, not retained Gate C evidence.

The decision-independent lifecycle fixture slice committed at `5c9d434` adds
sanitized login/refresh-to-durable-reload-to-final-header chains, recursive
final-state foreign-`CODEX_HOME` sentinels for login commit, refresh, status,
doctor, provider auth, and logout, and a real-file status/doctor matrix. The
matrix covers every top-level local state, all 11 malformed reasons reachable
from file-backed ChatGPT classification, both refresh-candidate reasons, and
both unknown-expiry reasons; the reserved `MixedCredentialKinds` reason is not
fabricated because this classifier cannot emit it. Exact-source working-state
validation used the repository's normal `target/`: OAuth tests passed 181/181,
`norn-cli` library tests passed 465/465, strict workspace/all-target Clippy and
fmt passed, the scoped forbidden-pattern scan returned no match, and all 12
changed Rust files are below 500 physical lines (largest 464). An independent
read-only source audit found no blocker or major issue after its proof-scope,
target-gating, disclosure-sentinel, and matrix-inventory findings were fixed.
These remain implementation checks, not retained Gate C or P2 acceptance.

The independent implementation-candidate review committed at `c4965e0`
returned `READY` for source `4d51a36` with no blocker, major, or minor finding.
It identified one owner-disposition observation: the provider-auth matrix lived
in `norn-cli`, so a library embedder could miss the settings-to-auth policy even
though concrete provider constructors still enforced their destination/auth
invariants. Source `448353d` closes that boundary by moving the single pure
matrix into the public `norn` configuration API, exposing canonical resolution
on `ProviderSettingsResolved`, and reducing the CLI implementation to a backend
adapter. Library matrix tests pass 3/3, the public embedder fixture passes 2/2,
the CLI adapter tests pass 4/4, the existing early-rejection slice passes 28/28,
and strict workspace/all-target Clippy plus fmt pass. This correction still
requires the final retained gate and independent acceptance review.

### What this phase fixes

The original implementation read the account ID from the wrong JWT shape, hid
credential-state failures, coordinated only callers sharing one manager, could
report success before durable ownership, acknowledged browser completion too
early, and made remote revoke a prerequisite for local logout. The candidate
addresses those Norn-owned default and named-account paths. It also
separates Norn's writable `$NORN_HOME/auth/auth.json` from foreign
`$CODEX_HOME/auth.json`, so Norn no longer races or mutates the Codex CLI file.
Foreign import/migration and OS-keyring integration are explicitly outside P2.
Remaining phase work is the P1 dependency disposition, the live A/B/A validity
experiment, retained candidate gates, and independent P2 acceptance.

### Difference after the phase

Norn-created credentials produce the required account metadata. One coordinator
owns each credential identity inside a process; cooperating Norn processes serialize
refresh, and a non-cooperating write to the Norn-owned file is detected rather
than knowingly overwritten. Corruption and unknown expiry are distinct local
states; refresh conflict and successful-but-undurable refresh are distinct
operation outcomes. Static credentials cannot rotate without an acknowledged
owner sink. Named accounts can be logged in, listed, explicitly selected,
inspected, and removed without
overwriting another named account or touching the Codex CLI's foreign
credential. The selected identity is pinned when a provider
starts; selection changes affect new providers only. Until P5 persists account
affinity, resume requires an explicit trusted account choice and never consumes
the active-account default. Browser success means the
credential and its directory entry are durable, and logout always removes the
local credential while reporting remote revoke separately. P2 makes no
automatic-rotation claim.

### Work checklist

- [x] Parse the namespaced Codex auth claim and retain only provider-shape
  compatibility justified by sanitized fixtures; reject conflicting account
  identifiers.
- [x] Remove unused serialization authority from `IdTokenClaims` and stop
  silently converting claim-parse failures into empty metadata.
- [x] Replace the existing eight-day unknown-expiry fallback with an explicit
  typed `Unknown` local classification; `last_refresh` does not invent expiry.
- [x] Centralize absolute trusted Norn auth-root resolution in one library-owned
  typed resolver used by CLI and library callers. The resolver selects
  `$NORN_HOME/auth` or the trusted-home default and ignores `$CODEX_HOME`.
- [x] Introduce typed ownership for the Norn-managed root and make ownerless
  static/embedder credentials non-refreshable rather than stranding rotation.
- [x] Keep foreign-file import/migration and Codex OS-keyring integration out of
  P2. `$CODEX_HOME/auth.json` remains foreign and untouched; Norn exposes no
  import surface and never scrapes or shells out for keyring credentials.
- [x] Share one coordinator per credential storage identity inside a process.
  Use reclaimable registry entries rather than a permanent global cache.
  Symlinked roots fail during no-follow root opening before coordinator
  registration or authority I/O rather than aliasing one storage identity.
- [x] Implement the per-credential reload-lock-refresh-save transaction for
  cooperating Norn processes with atomic durable storage, a caller-overridable
  acquisition deadline, and explicit lock failure behavior.
- [x] Close D9A by recording owner-approved positive defaults for the bounded
  lock acquisition deadline and its portable polling cadence, expose the
  cadence programmatically, and reject zero values before filesystem access.
- [x] Detect a lock-ignoring writer changing the Norn-owned credential during
  refresh and return a typed conflict without knowingly overwriting it. No Norn
  lock is taken against the Codex CLI's foreign credential.
- [x] Never report refresh success when a rotated credential was not durably
  accepted by its owner. Static credentials require an acknowledged persistence
  sink or have refresh disabled.
- [x] Preserve typed load, parse, refresh, persistence, and permission errors.
- [x] Do not silently fall back to a stale token after a required refresh or an
  indeterminate/undurable persistence outcome.
- [x] Retry a post-dispatch HTTP status only when its semantics prove
  non-acceptance: `408` and `425` are transient; explicit client rejection is
  permanent except unsourced `429`; `429`, redirects, and server/gateway errors
  are indeterminate and do not replay in the live manager.
- [x] Supervise each shared refresh worker so abnormal task termination wakes
  every live waiter with a typed indeterminate outcome rather than hanging.
- [x] Mark a refresh lineage indeterminate before authority dispatch and use the
  durable token-free journal to block same-lineage replay across managers,
  processes, and restarts until durable state proves convergence or external
  lineage advancement.
- [x] Use private no-follow regular-file credential storage with atomic
  replacement, file fsync, parent-directory fsync, and durable deletion.
- [x] Add a versioned named-account index whose user
  alias maps to an opaque storage identifier; aliases are never filesystem paths.
- [x] Add explicit `auth login --name`, `auth list`,
  `auth use`, `auth status [name]`, and `auth logout [name]` surfaces with
  deliberate all-account behavior.
- [x] Enforce ambient `$CODEX_HOME` as foreign and non-authoritative: it cannot
  redirect default Norn storage, and its `auth.json` is never mutated, locked,
  permission-hardened, or deleted by the default or named CLI/provider login,
  refresh, status, or logout paths. A trusted explicit library root is
  an ownership declaration, not an import or path-discovery surface.
- [x] D9 does not approve import in P2, so no path makes the foreign Codex file
  a shared writable store or silently duplicates a rotating refresh token.
- [x] Pin the selected identity for a provider/run. Before P5 adds persisted
  affinity, every resume requires explicit trusted account selection and rejects
  the Norn active-account default; it never silently chooses a replacement.
- [x] Make account selection trusted-only: explicit CLI, Norn-owned active
  selection, or trusted user configuration. Project/local settings, model
  aliases/profiles, prompts, and tools cannot select or rotate accounts.
- [x] Close `CONFIG-01` and `CONFIG-02` with the D9-approved library-owned typed
  auth/source/account matrix, including whether `oauth`, `env`, and `api_key` are
  distinct or aliases and which companion fields each requires or forbids. Do
  not invent compatibility semantics during implementation. Validate before
  environment lookup, credential loading, provider construction, or network I/O.
- [x] Replace CLI status and doctor booleans with one library-owned credential
  state evaluator distinguishing missing, malformed, access-expired,
  refresh-candidate, locally-valid, and unknown states. Refresh conflict and
  undurable persistence remain typed operation outcomes. List/status remain
  local, side-effect-free, remotely unverified, and free of token or identity
  disclosure. Doctor may add an explicit optional active probe without changing
  the local classification contract.
- [x] Require a private, token-free, no-TTL durable recovery marker for
  ambiguous refresh dispatch and commit publication. It must block same-lineage
  replay across processes and restarts until durable state proves commit or
  external advancement.
- [x] Implement the recovery marker and its crash/fault/restart matrix, including
  an actual child-process restart after an accepted request whose authority
  response is withheld.
- [x] For default and named login flows, delay browser completion until exchange,
  durable credential save, and any named catalog publication succeed. Own and
  join the worker outcome so cancellation cannot leave a surprise credential
  write.
- [x] Always clear local credentials during logout and report remote revocation
  as an independent result; make the local deletion durable.

### Phase-specific evidence

These boxes remain unchecked until a retained candidate run proves the complete
claim. Notes in parentheses identify fixtures already present in source; they
are not pass claims.

- [ ] Redacted real-shape JWT fixtures cover namespaced and supported fallback
  claim sources through login, any approved import, refresh, and final header
  application without storing a usable token. (Namespaced/fallback/conflict and
  provider-header fixtures are present. Source `5c9d434` adds sanitized
  namespaced login-response and refresh chains through durable reload and final
  headers, plus non-disclosing decoder conflicts. No import branch is approved,
  and fallback/import end-to-end chains plus retained execution remain open.)
- [ ] Two separately constructed providers in one process share a coordinator
  and cannot refresh the same lineage twice. (Registry and single-flight
  fixtures are present; retained candidate execution is pending.)
- [ ] Two real OS child processes targeting one Norn-managed identity perform
  one effective refresh exchange and converge on identical durable state. (The
  child-process fixture is present; retained candidate execution is pending.)
- [ ] A scripted lock-ignoring writer replacing Norn's `auth.json` during
  exchange is detected without overwrite, and mutation sentinels prove
  `$CODEX_HOME/auth.json` bytes, metadata, and surrounding files remain
  unchanged across login, refresh, status, and logout. (The Norn conflict and
  resolver non-authority fixtures are present. Source `5c9d434` adds recursive
  final-state inventory, byte, symlink-target, timestamp, permission, and Unix
  identity sentinels after login commit and refresh, and after each CLI
  status/doctor/provider/logout checkpoint. The sentinels prove no artifacts
  remain, not absence of transient access; retained candidate execution and the
  structural foreign-path inventory remain open.)
- [ ] Successful exchange followed by save, rename, fsync, permission, or owner
  sink failure is not returned as ordinary success. (Persistence and ownerless
  static failure fixtures are present; the complete fault matrix and retained
  run remain open.)
- [ ] Static credentials cannot strand a rotated refresh token: refresh is
  rejected without an owner sink, and any approved sink failure remains
  visible. (The no-owner rejection fixture is present; no sink interface has
  been approved.)
- [ ] Corrupt, unreadable, partial, permission-denied, symlink, non-regular,
  rename, fsync, and delete cases surface the correct typed state. (Focused
  malformed, link/non-regular, conflict, and durable-delete fixtures are
  present. A manager-level symlink-root fixture proves zero authority requests,
  zero target or mode mutation, and no lock creation; the complete matrix and
  retained run remain open.)
- [ ] Browser exchange/save/cancellation failures cannot render a final success
  page or perform an unowned later write. (Commit-order, cancellation, and
  storage-failure fixtures are present, including cancellation while
  transaction acquisition is pending and accepted-stream cancellation returning
  the generic HTTP 400 page; retained candidate execution is pending.)
- [ ] Panic, abort, completion-channel loss, and ambiguous post-dispatch
  disconnect wake every live refresh waiter with `Indeterminate`; one live
  manager does not replay the lineage, and a changed durable lineage permits
  recovery. (Deterministic abort, channel-loss, and disconnect fixtures are
  present; panic takes the same `JoinError` terminal branch. A real child-process
  restart and withheld-response fixture covers process/restart durability;
  retained candidate execution remains open.)
- [ ] Revoke network/authority failure still deletes the local credential. (The
  local-first revoke fixture is present; retained candidate execution is pending.)
- [ ] Before P2 acceptance, the owner-approved experiment verifies whether
  logging in and refreshing account B
  leaves account A authenticated and independently refreshable, not merely
  unchanged on disk. A failed redacted result blocks the supported P2 claim and
  returns it for owner disposition.
- [ ] Two named accounts coexist; switching affects only new providers; logout
  of one leaves the other unchanged; aliases and duplicate identities fail
  safely. (Coexistence, future-provider switching, exact-generation logout,
  alias collision, and duplicate-identity fixtures are present; retained
  candidate execution remains open.)
- [ ] Status and doctor produce the same local classification for every fixture;
  any doctor active-probe result is reported as separate remote evidence. (Both
  use the same library evaluator. Source `5c9d434` adds a real-file
  cross-command matrix covering every top-level state, all 11 reachable
  file-backed malformed reasons, both refresh reasons, both unknown-expiry
  reasons, symlink/non-regular/unreadable entries, and observational mode
  preservation. Retained candidate execution is pending.)
- [ ] Resume tests prove no pre-P5 session consumes an implicit active-account
  default after account selection changes. (The explicit-account resume guard
  and selection-change fixtures are present separately; the joined retained
  scenario remains open.)
- [ ] The typed auth/source/account matrix rejects every invalid combination
  before reading an environment variable or credential. (The exhaustive pure
  library matrix, public embedder API, CLI-equivalence, and side-effect-order
  fixtures are present; retained candidate execution remains open.)

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
  (53 stream events and 28 output-item variants at the 2026-07-15 P1 contract
  retrieval) and Codex-specific
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
- [ ] Resolve the accepted P0 follow-up for the pre-existing invented
  `EXTENSION_TIMEOUT = 30s` in `integration/extensions.rs`: make its timeout
  policy explicit and configurable rather than silently retaining an arbitrary
  transport default.
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

### Current candidate snapshot

This progress snapshot is not acceptance evidence and does not populate the
ledger prematurely.

| Phase | Current implementation | Retained candidate evidence | Work still required before acceptance |
|---|---|---|---|
| P0 | Accepted source head `e1bf7f2`; packaging through `1096628`; final review `7ce29d7` | Gate C 38/38 and 9,299 Rust test executions; distributions 830/830 and 1,250 Rust test executions; 359-file/65-test-only/97-writer policy pass; mechanical attestation pass; independent reproduction, deferred seam sweep, and acceptance supplement complete | None; accepted 2026-07-15 |
| P1 | Gate A complete at base `2917c8e`; Gate B foundation not yet implemented | Ratified public/Codex and repository-policy contracts; exact 62-row preregistration; independent Gate A `READY` | Implement and independently review the executable foundation, complete and verify P1, then resolve D0 before acceptance |
| P2 | Implementation candidate through `448353d`: Norn-owned default and named OAuth accounts, trusted selection and provider pinning, a public library-owned provider-auth matrix, durable restart-safe refresh recovery, foreign `CODEX_HOME` non-authority, durable login/logout, status/doctor classification, and failure matrices are present in source | Implementation review `c4965e0` is `READY` for source `4d51a36`; retained D9A distributions are 20/20 for the process-local deadline and 20/20 for two-process convergence; current focused checks are 216/216 OAuth and 483/483 CLI plus the `448353d` library 3/3, public API 2/2, CLI adapter 4/4, and early-rejection 28/28 slices with strict workspace/all-target Clippy, fmt, diff, forbidden-addition, and source-size checks; no complete retained P2 candidate gate bundle | Independently review the library-boundary correction, resolve the P1 dependency, run the live A/B/A validity experiment, execute and retain the complete candidate gates, then obtain P2 acceptance |

| Phase | Phase base | Implementation commit(s) | Finding evidence and full-gate results | LOC/bypass policy report | Domain reviewer | Fable verdict | Status |
|---|---|---|---|---|---|---|---|
| P0 | `41ea210` | Source range `41ea210..e1bf7f2`; evidence/docs `d06e4fc`, `7648159`, `c029de5`, `1096628` | [`P0 traceability`](reviews/2026-07-12-p0-traceability.md); Gate C 38/38; distributions 830/830; attestation pass; independent reproduction in `7ce29d7`; [`acceptance supplement`](reviews/2026-07-15-p0-acceptance-supplement.md) | 359 changed Rust files; 65 test-only; 97 writers; zero bypass, over-500, module-shape, or thin-entrypoint violations | External domain seats A-C, whole-diff seam seat, and three read-only supplement seats, 2026-07-15 | [`READY`](reviews/2026-07-15-p0-correction-gate-d-review.md), `7ce29d7` | [x] Accepted |
| P1 | `2917c8e` | | | | | | [ ] |
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
- [ ] Every Responses-program active exception in the P1 legacy LOC baseline is
  resolved; immutable origin records remain, and unrelated entries have not
  grown, changed production behavior, or reached their recorded due point.
- [ ] Final independent Fable verdict is `READY`.
- [ ] Owner release approval is recorded.
