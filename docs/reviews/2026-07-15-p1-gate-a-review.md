# P1 Gate A independent review (2026-07-15)

**Verdict:** `READY` for P1 implementation entry.

**Phase base:** commit
`2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d`, tree
`9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582`.

**Reviewed artifacts:**

- [`P1 Gate A contract`](2026-07-15-p1-gate-a-contract.md)
- [`P1 repository-policy contract`](2026-07-15-p1-policy-contract.md)
- [`62-row finding preregistration`](evidence/p1/finding-traceability.jsonl)
- P1 and universal-gate sections of the
  [`Responses remediation plan`](../RESPONSES-API-REMEDIATION-PLAN.md)
- [`Decisions`](../DECISIONS-2026-07.md) section 14

This is an entry/design verdict. It does not claim that P1 fixtures, policy
code, mutation integration, or the local gate have been implemented or tested.
D0 remains an explicit P1-exit blocker.

## Independent seats

### OpenAI/Codex source contract

A read-only source reviewer checked the public contract through the OpenAI
Developer Docs MCP and official pinned `openai/codex` source. The review
verified:

- 32 public input variants;
- 28 public output-item variants;
- 53 public SSE event names;
- 16 tool schemas and 18 accepted discriminator literals;
- roles, instructions, optional assistant phase, reasoning, compaction, usage,
  include, cache, status, and refusal semantics;
- Codex commit/file/blob pins, ancestry, turn state, metadata, client request
  choices, and explicit backend unknowns.

The initial source verdict was `NOT AGREED`. Corrections added missing input and
cache inventories, the Codex `common.rs` pin, exact nested usage fields, tool
aliases, source discrepancies, event-correlation requirements, and known-client
versus unproven-backend distinctions. A later schema conflict over assistant
phase was resolved directly through the Developer Docs MCP: phase is optional,
not nullable. The final contract preserves absent versus present values.

The final source disposition is `AGREED`. Exhaustive event payload structure is
not overclaimed: the first foundation must produce a reviewed sanitized all-53
schema extraction and hash before downstream fixtures can be accepted.

### Finding inventory and evidence method

A separate read-only reviewer mechanically compared the original finding index
with the plan and candidate registry. The final registry has:

- 62 rows and 62 unique source IDs in exact source order;
- no missing, extra, or multiply owned finding;
- phase totals P0/P2/P4/P5/P6/P7/P8 = 23/9/8/6/5/6/5;
- evidence classes: 55 confirmed defects, two gate findings, one measurement,
  two design items, one enhancement, and one accepted limitation;
- statuses: 23 `accepted_p0`, 39 `open`;
- expectations: 23 accepted evidence, 35 baseline-red, four contract targets;
- 62 unique planned evidence IDs and 39 unique future fixture IDs;
- explicit fixture applicability on every row; and
- exact source severity counts: five critical, 25 high, 24 medium, two low,
  two low/medium, and one each medium/gate, enhancement/security-sensitive,
  design, and informational.

Every row preregisters its source evidence, evidence method, target assertion,
current seams, and fixture applicability. Concrete repository seam paths were
validated as existing. Future surfaces are explicit tokens rather than fake
paths.

### Repository-policy architecture

A fresh context-free architecture reviewer initially returned `NOT READY`.
The correction contract now closes every reported blocker:

- hard first-party enforcement uses a non-downgradable staged prepublication
  path, not the current after-commit post-check;
- one process-wide workspace coordinator is introduced and shared with child
  loops; cross-process exclusion is not claimed;
- full and incremental checks are projections of one canonical evaluation;
- task-complete/stop scans run even with no tracked modified files;
- exact Cargo target, cfg, module, generated-include, LOC, module-shape, and
  production-hiding semantics are pinned;
- immutable computed origin facts are separate from reviewed governance;
- active legacy exceptions require unchanged production projections and must
  resolve before their due phase;
- debt fingerprints, writer sinks/dataflow, redaction schemas, and tightening
  rules are executable rather than prose-only;
- the gate freezes clean HEAD/tree at start and rechecks them at completion;
  no self-referential candidate commit is required; and
- local locks/gates are explicitly tamper-evident, not tamper-resistant, until
  D0 supplies remote enforcement.

The architecture correction verdict is `READY`.

### Overall Gate A

A final fresh reviewer compared the latest contracts with live lifecycle,
mutation, stop-hook, loop, persistence, user-surface, child-agent, and CLI
seams. It required explicit fixture non-applicability, severity, full touchpoint
ownership, the new coordinator, per-candidate gate cadence, optional phase
semantics, the exhaustive-schema handoff, and the foundation freeze boundary.
All were corrected.

The final overall verdict is `READY` with no remaining Gate A blocker.

## Entry invariants

- P1 changes no provider/auth/transcript/replay/Responses-tool/retry behavior.
  It deliberately changes first-party file-mutation policy enforcement.
- No live provider request, credential use, account experiment, or billable
  request is permitted in P1.
- D0 is deferred only for entry. P1 cannot be accepted without its exit
  disposition, and no remote-protection claim is allowed while it is open.
- The first implementation foundation installs the checked-in gate/manifest,
  generates the exact origin/phase lock and exhaustive public-schema extraction,
  and obtains their independent review before later implementation commits.
- From that foundation boundary, every logical implementation candidate runs
  and retains the exact current local gate, including red intermediate results.
- No lint suppression, ignored test, prohibited call/marker, reduced scope, or
  advisory substitute is an accepted implementation path.
- All scratch, build, and gate output stays under ignored repository `target/`.

## Verification performed

The review seats were read-only and ran no Cargo command. Documentation and
registry checks established:

- strict JSONL decoding and exact field/enumeration consistency;
- source-ID order/set equality;
- owner, status, severity, class, expectation, method, evidence-ID, and fixture
  totals;
- repository seam-path existence;
- exact base commit/tree identity; and
- clean `git diff --check`.

Gate B remains wholly unclaimed until the first executable foundation is
implemented and reviewed.
