# P1 repository-policy contract candidate (2026-07-15)

**Status:** Ratified implementation contract under the
[`P1 Gate A READY review`](2026-07-15-p1-gate-a-review.md). It defines P1's
shared repository policy but does not claim that the implementation exists.

**Phase base:** commit
`2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d`, tree object
`9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582`. The origin generator and gate
require both exact objects.

**Trust statement:** Local P1 enforcement is deterministic and tamper-evident
for accidental, stale, or partial policy changes. It cannot resist a coordinated
change to the checker, gate, lock, and manifests in the same unprotected branch.
D0 supplies that remote review/enforcement trust boundary and remains a P1-exit
blocker. This document makes no remote-protection claim.

## Authority boundary

The new `norn-policy` crate is a pure, synchronous evaluator. It accepts owned,
immutable data and returns stable findings. It does not:

- read the filesystem or Git repository;
- execute Git, Cargo, a compiler, a language server, or another process;
- access the network, environment, credentials, or provider state;
- render terminal output or choose a process exit status; or
- mutate source, policy, baseline, or evidence files.

CLI and runtime adapters own snapshot acquisition. Renderers own human/JSON
output. The gate owns command execution and exit status. Provider, request,
streaming, transcript, auth, and tool wire behavior remain unchanged in P1;
the declared exception is mutation-policy enforcement for first-party
`write`, `edit`, and `apply_patch` operations.

Repository policy may tighten Norn's built-in hard rules. It may never raise a
limit, remove a prohibited construct, change failure to advice, shrink an
inventory/redaction scope, or select an executable. Unknown fields and unknown
schema versions fail.

## Canonical evaluation

There is one semantic operation:

```text
evaluate(owned_snapshot, origin_ledger, governance, policy) -> findings
```

`OwnedSnapshot` is a repository-relative, sorted map of normalized path to
owned bytes plus the Cargo/workspace and policy files needed for analysis. A
finding has a stable code, repository-relative path, byte span where one
exists, deterministic message fields, and algorithm version. It contains no
absolute path or source snippet.

Repository checking and runtime checking do not implement separate rules:

- The CLI adapter supplies a complete current snapshot and the base Git-tree
  snapshot.
- A runtime mutation constructs a complete immutable overlay snapshot from the
  current owned snapshot plus all proposed creates, edits, and deletions.
- Incremental evaluation is only a projection/cache of the canonical result.
  For every affected finding class, tests compare it byte-for-byte with full
  evaluation. A class without that proof always runs full evaluation.
- Any Rust module declaration, `#[path]`, `include!`, Cargo manifest, policy,
  phase lock, baseline, governance, writer registry, evidence schema, or gate
  entrypoint change invalidates all dependent cached facts.
- Task-complete and stop always run a full repository evaluation, including
  when the modified-file accumulator is empty. Shell, editor, and external
  process changes are caught there and by the local gate.

P1 introduces a process-wide `WorkspacePolicyCoordinator`; the existing
per-batch `ToolEffect::Write` scheduling is not a workspace lock. One shared
`Arc` is installed in the root tool context and forwarded into every spawned or
forked child context. It serializes first-party snapshot, staged evaluation,
and publication for a canonical workspace root across all loops in that Norn
process.

External editors and other Norn processes do not honor this coordinator.
Adapters compare file identity/content stamps after acquisition and again
immediately before publication; a detected change returns typed
`RepositoryChangedDuringSnapshot` with `committed: false` and no invented retry
count. This is detection, not a claim of cross-process exclusion. The caller
may start a fresh evaluation. The clean-checkout gate requires no concurrent
mutation.

## Runtime enforcement lifecycle

The existing runtime post-check occurs after bytes are committed, `write` uses
report mode, and `AllowBrokenAst` can demote ordinary AST gate behavior. P1 does
not pretend that seam can prevent a mutation.

P1 adds a distinct hard staged-mutation check:

1. The tool completes argument, confinement, read-before-write, and patch
   resolution checks.
2. `write`, `edit`, or `apply_patch` constructs the entire proposed mutation in
   memory. A patch proposal includes all creates, modifications, and deletions.
3. The runtime adapter overlays the proposal on its owned repository snapshot
   and calls the canonical evaluator before publication.
4. Any hard finding or invalid required policy returns a typed failure with
   `committed: false`. No file in the proposal is published.
5. Only a passing proposal reaches the existing atomic publication path.
6. After publication, the ordinary diagnostic/post-check surface may report the
   same findings for observability, but it is not the enforcement boundary.

The hard staged check is not a `PostValidateMode`. `AllowBrokenAst`,
`ForceGate`, `RejectBrokenAst`, profiles, repository conventions, model input,
and tool arguments cannot disable, demote, exclude, or change its scope. The
existing flags retain only their current generic syntax meaning outside the
repository-policy decision.

Policy state is explicit:

- `Absent`: no phase-lock marker exists; generic Norn behavior is unchanged.
- `Ready`: every required policy input is present, strict, and internally
  consistent; staged checks run.
- `Invalid`: a marker exists but an input is absent, unreadable, unknown,
  inconsistent, or stale; staged source/policy mutations and task completion
  fail persistently until a valid atomic proposal repairs the complete set.

There is no `Option`/`None` fallback from `Invalid` to the older advisory
`CONVENTIONS.toml` rules. A multi-file patch may repair policy inputs only when
the complete staged overlay validates. P1 removes the overlapping Norn-specific
LOC and bypass activations from `CONVENTIONS.toml`; generic diagnostics remain
available for repositories without a required policy profile.

## Rust production reachability

The evaluator discovers only local packages named by the root Cargo workspace
`members`/`exclude` rules. It parses manifests as strict data and applies Cargo
target discovery without invoking Cargo.

Production roots are:

- library and proc-macro targets, explicit or implicit `src/lib.rs`;
- binary targets, including explicit entries, `src/main.rs`, and Cargo's
  enabled `src/bin` discovery;
- explicit or auto-discovered example targets; and
- enabled package build scripts, including implicit `build.rs`.

Integration-test and benchmark target roots are test-only. A module reachable
from any production root is production even if it is also reachable from a
test/benchmark root. An `.rs` file beneath a package source/target directory
that is neither production-reachable nor classified under an explicit
test/benchmark root is an `UnclassifiedRustSource` failure, not an invisible
way around the policy.

Conditional compilation uses a three-valued abstraction with `test = false`:

- `cfg(test)` is false and `cfg(not(test))` is true.
- Other target, feature, and build predicates are possible unless the manifest
  or expression proves them false for every production configuration.
- `all`, `any`, and `not` propagate true/false/possible exactly.
- An item is excluded only when its predicate is definitely false.
- A possible `cfg_attr` evaluates both branches and unions reachability and
  attributes. A definitely true/false `cfg_attr` evaluates its selected branch.
- Malformed/unsupported predicates fail rather than being treated test-only.

Module resolution supports inline modules, normal `mod name;` lookup, literal
`#[path = "..."]`, and literal `include!("...")`. Every resolved path remains
beneath its package/workspace authority and is read without following a
symlink.

One non-literal class is admitted only through a strict generated-include
registry: a build-script `OUT_DIR` include. Each entry pins the source call
site, enclosing item, normalized invocation digest, Cargo target, generator
path/hash, complete repository input path/hash set, output basename/schema, and
owner. The base registry includes only
`crates/norn/src/model_catalog.rs`'s exact
`include!(concat!(env!("OUT_DIR"), "/model_catalog_generated.rs"))`, generated by
`crates/norn/build.rs` from `assets/models.json`. The invocation/enclosing module
count as production source; generated `OUT_DIR` bytes do not become repository
source LOC. The generator and inputs are production-hashed and writer-inventoried.
Any change or unregistered generated include fails until explicitly reviewed.

All other non-literal include/path selection, conflicting possible paths,
missing files, symlinks, parse errors, or resolution cycles fail closed. A file
shared by production and test paths is production.

The origin ledger records production item identities and projection hashes. If
a current item with the same normalized identity/content moves behind a
test-only predicate, the evaluator reports `ProductionHiddenAsTest`. Genuine
removal is permitted; reclassification is not a way to reduce production LOC.

## LOC and module shape

The exact LOC algorithm is versioned and pins Tokei 14.0.0 Rust semantics:

1. Parse the complete file and compute byte ranges proved test-only.
2. For LOC only, replace every non-newline byte in excluded ranges with a space,
   preserving byte positions and line endings.
3. Count Rust code lines with pinned Tokei; blank/comment-only lines are not
   production LOC.
4. For production-content identity, serialize repository path plus ordered
   production syntax/token spans, normalize CRLF to LF, omit proved test-only
   spans entirely, and hash with SHA-256. Test-only line additions therefore do
   not change the production projection hash.

Limits are hard: 200 production LOC for `lib.rs` and `main.rs`, 500 for every
other production Rust file. A production `mod.rs` additionally permits only:

- an external module declaration with no inline body;
- a `use` declaration with explicit visibility (`pub`, `pub(crate)`,
  `pub(super)`, or `pub(in ...)`); and
- attributes/doc comments attached to an otherwise permitted item, subject to
  the separate suppression rules.

Whitespace and comments are inert. Inline module bodies, private imports,
functions, types, constants, statics, macros, `include!`, expressions, and any
other named top-level syntax node fail `ModuleShape`. Parser-version or LOC
algorithm changes require an explicit migration that retains old/new results
and independent review; silently regenerating a baseline is forbidden.

## Origin ledger and governance

Computed origin facts and human governance are separate inputs.

`policy/origin/p1-computed.json` does not exist at Gate A. Gate A ratifies its
algorithm and exact base commit/tree. The first implementation foundation
generates the ledger and phase lock, then obtains an independent review of their
exact contents and digests before later implementation commits proceed. They
become immutable at that reviewed foundation boundary. The ledger contains:

- schema and analyzer versions;
- base commit and exact base tree object ID;
- normalized hard-policy digest and source-inventory digest;
- every base production file's LOC and production-projection hash;
- the multiset of prohibited-debt fingerprints; and
- the generated writer-operation inventory and stable operation IDs.

The CLI reconstructs only those computed facts from the immutable base Git tree
and requires exact equality. It never claims to reconstruct governance.

`policy/governance/legacy.toml` maps origin IDs to owner, due phase, remediation
record, and writer-family classification. It is strict reviewed metadata. The
immutable origin entry is never deleted when debt is fixed. Current active
exceptions are derived as `origin - resolved`; a current file at/below its limit
or a removed debt occurrence becomes resolved. No new active exception can be
added.

An active legacy over-limit exception is valid only while its current
production LOC and production-projection hash exactly equal the immutable
origin values and the active phase is strictly earlier than its due phase. Any
growth, any production edit (including a reduction that remains over limit), a
missing owner/remediation, or reaching/passing the due phase fails. Reaching the
applicable LOC limit resolves the active exception; the immutable origin record
remains as audit history. Test-only edits are permitted only when the production
LOC/projection remain identical.

`policy/phase-lock.json` pins the active phase (`P1`), base commit/tree,
schema/analyzer versions, and normalized digests of policy, governance, writer
registry, contract pins, evidence schemas, source-finding registry, and gate
command manifest. The CLI accepts no alternate base, phase, policy path, scope,
advisory mode, or exclusion option. The first P1 foundation commit and its lock
digest are recorded in the plan ledger and reviewed before later work.

The monotonic tightening lattice is:

- numeric maxima may decrease, never increase;
- prohibited constructs and analyzed roots/sinks may be added, never removed;
- failure cannot become advice or success;
- an evidence/redaction schema may add required restrictions, never loosen one;
- a due phase may move earlier, never later; owner/remediation cannot become
  empty;
- origin exceptions may resolve, never appear or reactivate; and
- phase/base/tree/algorithm identity changes only through a versioned migration
  carrying old/new reports and a new independent review.

Changing both checker and manifests can still defeat local comparison. The
lock is tamper-evident, not a signature; D0's remote required review/check is the
eventual enforcement boundary.

## Prohibited-debt universe

The versioned debt registry covers, at minimum:

- Rust `allow` and `expect` attributes at every scope;
- `ignore` on tests and impossible/dead-code-hiding cfg expressions including
  `cfg(any())`;
- newly underscore-prefixed named parameters or bindings (bare `_` is distinct);
- `.unwrap()`, `.unwrap_err()`, `.expect()`, and `.expect_err()` calls;
- `panic!`, `todo!`, `unimplemented!`, and `unreachable!`;
- literal `TODO`, `FIXME`, and `HACK` markers in added source/evidence (negative
  validator tests assemble fragments at runtime);
- Cargo/workspace lint-level relaxation or removal;
- Clippy/Rust command-line allow flags, reduced target/scope flags, and
  `RUSTFLAGS`/encoded-rustflags suppression in checked scripts/config; and
- production items newly hidden as tests or tests excluded from the gate.

Each occurrence fingerprint contains analyzer version, repository path, target
kind, construct kind, enclosing module/item identity, normalized AST/token
digest, scope digest, and ordinal within an identical-occurrence multiset. Full
digests, not truncated display IDs, decide equality. A collision remains a
multiset entry and cannot collapse another occurrence.

Widening is explicit: a larger lint set, broader attribute scope, more disabled
configurations/targets, broader command allowance, or broader ignored-test
scope is greater and fails. The same normalized construct at a different path,
item, or scope is a move and fails. Unrelated line shifts do not change an ID.
A changed production item under a legacy suppression fails even if the
suppression fingerprint itself is unchanged.

## Writer inventory

Writer analysis is a reproducible conservative inventory, not a claim that
syntax alone proves all possible runtime I/O. Its versioned sink registry
enumerates:

- standard-library, Tokio, rustix, tempfile, and project-private open/create,
  truncate, append, write, set-length, permission, flush/sync, persist,
  rename/publication, link, and remove operations;
- registered project wrapper functions and their handle/authority semantics;
- imported/aliased names and local handle propagation from opener/root calls to
  mutation/publication/cleanup calls; and
- raw macro token candidates for every registered sink name.

An unresolved alias, macro candidate, dynamic/generic sink, or new wrapper is an
`UnknownWriterSink` failure until reviewed. A generic primitive used by several
artifact families receives one `shared_primitive` classification plus explicit
inbound family edges; it is not falsely forced into one family. All other
operations map to exactly one family or one reviewed false-positive/cleanup
class.

Stable operation IDs use path, enclosing item, normalized call, operation kind,
and multiset ordinal. The generated full inventory ships with the evidence and
distinguishes roots/openers, handle mutations, publication, permissions,
durability, cleanup, shared primitives, and false positives. A new/changed
unknown sink, missing family edge, zero/multiple non-shared classifications, or
stale registry entry fails.

## Evidence redaction

P1 does not claim regex can recognize arbitrary private prose or every real
account identifier. It prohibits captured free-form provider/user content in
P1 artifacts. Only registered synthetic fixture strings are allowed.

Each retained artifact family has a versioned closed schema and string grammar.
The validator:

- rejects unknown artifact types, schema versions, fields, duplicate JSON keys,
  unknown enum values, and unregistered files under fixture/evidence roots;
- recursively scans decoded keys and values and separately scans raw bytes;
- permits only repository-relative paths, fixed public URLs, registered
  synthetic IDs/text, numeric observations, and hashes of already sanitized
  artifacts;
- requires every opaque synthetic value to carry generator/provenance metadata
  and a non-reusable sentinel class;
- rejects bearer/API-key/JWT shapes, emails, absolute paths, account/credential
  fields, reusable turn state, raw cache keys, control characters, and any
  unregistered free-form string; and
- never retains raw provider traffic, prompts, credentials, environment values,
  or hashes/fingerprints derived from those values.

Evidence writers validate the in-memory artifact before atomic publication.
The local gate validates the final fixture/evidence tree again. Negative tests
construct dangerous patterns from safe fragments at runtime; there is no
excluded unsafe-fixture directory.

## Traceability contract

[`finding-traceability.jsonl`](evidence/p1/finding-traceability.jsonl) is a
strict 62-row preregistration, not a placeholder. Every row records source ID,
source severity, owner, evidence class, campaign status, expectation class,
evidence method,
stable planned evidence ID, source evidence, target assertion, seams, and
fixture applicability plus planned fixture IDs. `accepted_p0` rows use
`not_applicable_accepted_p0` and an empty array; open rows use `planned` and one
stable planned fixture ID.

The gate extracts the source-review ID inventory and requires exact ordered set
equality, unique planned evidence IDs, the recorded owner/class/status totals,
and a registered evidence result before an owning phase can close a row.
`accepted_p0` rows bind to accepted P0 evidence; P1 does not reopen them.
`baseline_red` rows require the owning phase to prove the documented baseline
failure before accepting its corrected regression. `contract_target` rows use
the preregistered measurement, design, or enhancement method.

## P1 enforcement evidence IDs

P1's own enforcement work uses these stable preregistered records in addition
to the 62 source-finding rows:

| Evidence ID | Reviewed baseline | Required target evidence |
|---|---|---|
| `p1-policy-loc-001` | `CONVENTIONS.toml` is advisory and `loc.rs` counts whole files | cfg-aware 200/500/module-shape fixtures and repository/runtime parity |
| `p1-policy-lifecycle-001` | runtime policy checks occur after commit and can use report/demoted modes | write/edit/multi-patch staged failures prove `committed: false` under every flag combination |
| `p1-policy-origin-001` | no immutable computed origin/governance split exists | exact base/tree reconstruction, monotonic exception, due-phase, and migration fixtures |
| `p1-policy-writers-001` | P0's 97-row regex inventory is explicitly a seed | generated sink/handle/publication/cleanup inventory with zero unknowns |
| `p1-policy-redaction-001` | no shared closed-schema P1 artifact validator exists | prepublication and final-tree positive/negative validator fixtures |
| `p1-gate-local-001` | no checked-in deterministic P1 clean-checkout gate exists | green exact-source run plus dirty/tree/env/entrypoint failure fixtures and retained descriptor |

Each baseline is a source characterization at `2917c8e`, not a claim that a
native failing test already exists. Gate B requires the characterization and
its target regression to be reviewed together before the row can close.

## Clean-checkout gate contract

The checked-in P1 gate has no weakening flags. It runs only when:

- the gate first proves the checkout clean, derives candidate commit/tree from
  that clean `HEAD`, and freezes both in the in-repository run descriptor;
- the recorded P1 base is an ancestor of that frozen candidate and has the
  expected base tree object;
- the source checkout has no tracked modifications, untracked non-ignored
  files, conflicted index entries, or dirty/uninitialized submodules;
- the exact checked-in gate entrypoint and fixed command manifest hashes match
  the phase lock; and
- suppression/override environment variables are absent. The gate itself sets
  `CARGO_INCREMENTAL=0`, a repository-local `TMPDIR`, and a repository-local
  `CARGO_TARGET_DIR` beneath ignored `target/p1-gate/`.

Commands run in one fixed order and Cargo commands are serialized. The manifest
contains the exact Gate C commands, policy check, added-line audit, fixture and
evidence validators, distribution runners, and final self-check. The evidence
records the entrypoint/manifest hashes, source commit/tree, base commit/tree,
toolchain, sanitized environment contract, command order, start/end status,
test counts, distributions, and sanitized artifact hashes.

At completion the gate re-reads `HEAD`, its tree, index/worktree status, and
submodule state. Any difference from the frozen start descriptor invalidates
the whole run. The candidate identity is therefore not stored inside its own
containing commit and needs no self-referential declaration.

Gate output is written only beneath ignored `target/p1-gate/evidence/`, outside
the clean source tree but inside the main repository. After a green source run,
validated artifacts may be copied into `docs/reviews/evidence/p1/` in a later
documentation-only packaging commit. The evidence names both source and
packaging commits; packaging is not described as a rerun. Any source change
invalidates the run.

The first post-Gate-A P1 foundation commit installs the gate entrypoint and
fixed manifest, even though incomplete implementation legs may initially make
the run red. From that boundary, every logical implementation candidate runs
the exact current gate and retains its full red/green descriptor under
`target/p1-gate/evidence/`; no candidate is handed to an external reviewer
without that evidence. This cadence is per candidate, not final-only. The
Gate-A documentation candidate precedes the executable foundation and does not
claim a gate run that cannot yet exist.

The local gate proves reproducibility, not protection against its own coordinated
modification. D0 must eventually make the exact entrypoint a required remote
decision or record a new owner disposition before P1 can be accepted.

## Planned implementation shape

```text
crates/norn-policy/src/
  lib.rs
  config.rs
  snapshot.rs
  engine.rs
  finding.rs
  baseline.rs
  rust/{mod,parse,cfg,modules,loc,shape}.rs
  debt/{mod,rust,manifest,text}.rs
  writers/{mod,scan,registry}.rs
  redaction/{mod,json,text}.rs
policy/
  repository.toml
  phase-lock.json
  origin/p1-computed.json
  governance/legacy.toml
  writer-families.toml
  evidence-schemas/*
```

`mod.rs` files contain declarations/re-exports only. Changed production files
remain below their applicable limits. The evaluator has no Git/process/runtime
dependency. CLI/runtime adapters live in their owning crates and share only the
owned snapshot/finding API.

Required tests include staged no-commit failure for write/edit/multi-file patch;
flag non-downgrade; `Absent`/`Ready`/`Invalid`; full/incremental result equality;
zero-modification stop scans; concurrent snapshot change; Cargo target/cfg/path/
include/shared-module matrices; 500/501 and 200/201 LOC; test-only hash stability;
production-to-test hiding; every module-shape/debt form; baseline monotonicity;
writer aliases/wrappers/macros/shared primitives; strict redaction schemas; all
62 traceability rows; clean-checkout rejection cases; and deterministic output.

Tests return `Result` or use explicit pattern assertions. They add no lint
suppression, ignored test, unwrap, expect, panic, unresolved marker, or gate
bypass.
