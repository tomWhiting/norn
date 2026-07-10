# P0 Gate C external-review handoff

**Date:** 2026-07-11  
**Phase:** P0 - credential and workspace authority containment  
**Handoff status:** Ready for whole-phase Gate D review  
**Phase status:** Not accepted; the P0 roadmap and evidence-ledger entries
remain open

## Review range

- Phase base: `41ea210d24ec0653480be3a097b15adcb1e4bfb0`
- Gate C code head: `ebb82c8cf2224790f8150676a8acbef5df6ed85c`
- Code range: `41ea210...ebb82c8`
- Documentation range: review `41ea210...HEAD` after checking out the commit that
  contains this handoff.

The phase was assembled as the following logical commits, in order:

| Commit | Purpose |
|---|---|
| `cd91c39` | Enforce trusted workspace authority. |
| `4fbc716` | Harden Responses credentials and transport. |
| `0f110d5` | Protect persisted session data. |
| `7d121c9` | Record the initial P0 implementation evidence. |
| `aceea4b` | Remove strict-Clippy failures from P0 regressions. |
| `82a2708` | Isolate provider-authority rejection cases. |
| `461b37b` | Archive the provisional P0 review reports. |
| `1bfa1a9` | Reconcile remediation scope and review findings. |
| `7d7d559` | Define the portable private-artifact race boundary. |
| `cca0432` | Add the P0 security dependencies. |
| `864b473` | Confine private runtime artifacts. |
| `30e5126` | Seal configuration and provider authority. |
| `9406df8` | Harden Responses terminal diagnostics. |
| `ebb82c8` | Satisfy the strict P0 regression lint gate. |

The three provisional reports under
[`2026-07-11-remediation-review/`](2026-07-11-remediation-review/) review frozen
snapshot `7d121c9`. They are historical review input, not Gate D verdicts. The
subsequent credential/config, transport/streaming, and private-artifact closure
reviews each returned `READY` on the owned surface now committed in the code
range. Those scoped verdicts are also not a whole-phase verdict.

Each closure reviewer inspected the final content of its owned surface before
packaging. No code in an owned surface changed between its verdict and code head
`ebb82c8`; work in a different slice may have continued until that slice's own
review. After all three verdicts, the only subsequent operations were whole-file
staging, the logical commits listed above, Gate C reruns, and documentation.
This handoff is the durable record for the three scoped verdicts:

| Evidence label | Reviewer | Date | Reviewed content | Verdict |
|---|---|---|---|---|
| `P0-CRED-CONFIG-R2` | Independent credential/config closure reviewer | 2026-07-11 | Credential/config and child-authority surface byte-equivalent to `ebb82c8` | `READY` |
| `P0-TRANSPORT-R2` | Independent task `/root/p0_transport_final_review` | 2026-07-11 | Transport/streaming surface byte-equivalent to `ebb82c8` | `READY` |
| `P0-ARTIFACT-R2` | Independent task `/root/p0_artifact_final_review` | 2026-07-11 | Private-artifact surface byte-equivalent to `ebb82c8` | `READY` |

## Gate C results

The following commands were rerun after code head `ebb82c8` was created:

| Command | Result |
|---|---|
| `cargo fmt --all --check` | Pass. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pass with no warning. |
| `cargo test --workspace --all-targets` | Pass. This includes 3,134 `norn` library tests, 437 CLI tests, 674 TUI tests, 17 PTY tests, and the remaining workspace integration, trybuild, example, and macro targets. |
| `cargo test --workspace --doc` | Pass: four runnable `norn` doctests and two compile-fail API-boundary doctests; other crates have no doctests. |
| `git diff --no-ext-diff --check 41ea210...ebb82c8` | Pass. |

The all-target run printed two benign process-cleanup messages of the form
`kill: <pid>: No such process`; the command exited zero and every test target
passed.

## Policy audits

The committed range changes 91 Rust files. Every changed production file is at
or below 500 logical production LOC after excluding comments, blanks, and
`cfg(test)` code. Sixty-two files were below the limit by whole-file inspection.
Two over-500 files were test-only. The production prefixes of the remaining 27
files were extracted at their test-module boundary and checked with `tokei`.
None exceeded the limit.

The table below records selected boundary-near and new/refactored files. For a
whole file already below 500, the count is the conservative whole-file `tokei`
code count and can include test code. For an over-500 physical file, the count
is its production prefix through the test-module boundary. The mixed method is
deliberately conservative and is not presented as an exact ranking of production
LOC:

| Production file | Audit LOC |
|---|---:|
| `crates/norn/src/tools/task/disk.rs` | 498 |
| `crates/norn/src/tools/agent/spawn.rs` | 497 |
| `crates/norn/src/agent/builder.rs` | 490 |
| `crates/norn/src/profile/loader.rs` | 488 |
| `crates/norn-cli/src/print/orchestrator.rs` | 466 |
| `crates/norn/src/agent/assembly.rs` | 463 |
| `crates/norn/src/tools/skill.rs` | 460 |
| `crates/norn/src/runtime_init/base.rs` | 445 |
| `crates/norn/src/provider/auth.rs` | 426 |
| `crates/norn/src/util/secure_file.rs` | 409 |
| `crates/norn/src/process/manager.rs` | 403 |
| `crates/norn/src/util/private_fs.rs` | 396 |
| `crates/norn/src/session/manager.rs` | 389 |
| `crates/norn/src/provider/openai/sse.rs` | 381 |
| `crates/norn/src/util/private_fs_mutation.rs` | 173 |

Added-line inspection over `41ea210...ebb82c8` found:

- Zero added `.unwrap()`, `.unwrap_err()`, `.expect()`, `.expect_err()`, or
  `panic!()` calls.
- Zero added or widened `#[allow]`, `#[expect]`, `#[deny]`, `#[ignore]`, empty
  `cfg(any())`, command-line lint suppression, or workspace lint relaxation.
- One `TODO` text match: `pattern = "TODO"` in the
  `tools/diagnostics_infra.rs` TOML fixture. It proves non-executing pattern
  checks survive workspace-convention sanitization; it is not an unresolved
  marker.
- Zero added `FIXME`, `HACK`, `todo!`, or `unimplemented!` markers.

The seven changed `mod.rs` files contain declarations, re-exports, or module
documentation only:

- `crates/norn-cli/src/config/mod.rs`
- `crates/norn/src/config/merge/mod.rs`
- `crates/norn/src/config/mod.rs`
- `crates/norn/src/profile/mod.rs`
- `crates/norn/src/provider/mod.rs`
- `crates/norn/src/provider/openai/mod.rs`
- `crates/norn/src/util/mod.rs`

## Secret-fixture inspection

Manual inspection of the complete committed diff found no plausible real
credential, account identifier, private prompt, reusable turn-state value,
response id, or cache key. Credential-looking values are explicit synthetic
sentinels such as `access-token-secret`, `dispatch-access-token`,
`dispatch-account`, `turn-state-secret`, and `must-never-authenticate-child`.
Environment names such as `OPENAI_API_KEY` and `PRIVATE_DEPLOYMENT_KEY` are
names only, not values. The only added high-entropy literal is the published
`hmac 0.12.1` checksum in `Cargo.lock`.

## Scoped closure reviews

- `P0-CRED-CONFIG-R2`: `READY`. The reviewer confirmed raw settings and merge
  authority are crate-internal, the public static-Codex constructor is sealed to
  the compiled Codex destination, and the real child variant/profile/fork paths
  cannot reconstruct backend or credential authority.
- `P0-TRANSPORT-R2`: `READY`. The reviewer confirmed keyed non-disclosing
  terminal discriminators, explicit redirect-policy refusal, complete response
  header-value redaction, and the documented split between generic error-body
  draining and specialized 401/429/redirect body drops.
- `P0-ARTIFACT-R2`: `READY`. The reviewer confirmed descriptor-pinned private
  roots, mode healing, no-follow traversal, no-replace publication, destination
  and source race confinement, complete artifact-family coverage, removal of
  relative sensitive-data fallback, and deletion of obsolete path-based
  permission scaffolding.

The scoped reviewers also recorded evidence limits for Gate D to assess. The
redirect regression exercises the real bounded client and `StreamExecutor`, but
not a complete public `Provider::stream` fixture, and its redirect body is
finite rather than stalled. Entropy failure is injected at the key-generation
seam rather than through the process-global terminal dispatcher. Redox and
ESP-IDF fail-closed branches were inspected but not compiled locally. OAuth
lifecycle findings remain assigned to P2; no scoped `READY` closes them.

## Gate D residuals

The external reviewer must verify these are honest limitations rather than
reachable defects in P0's claimed outcome:

- The portable same-UID race guarantee is descriptor confinement and
  outside-target preservation, not serializability against another process with
  the same authority.
- Workspace text reads remain unbounded pending an owner-approved size or
  streaming policy.
- The legacy raw provider-settings container still derives `Debug`; no reachable
  logging sink was found, but the type remains misuse-prone.
- Public `Scanner`, `scan_rule_dirs`, and `discover_skills` convenience APIs are
  trusted-input-only and are not used by secure runtime assembly for
  repository-controlled roots.
- `mcp_servers` remains a dormant merged surface. Future runtime wiring requires
  source provenance and explicit consent.
- Redox, ESP-IDF, and non-Unix workspace/private-artifact operations deliberately
  return typed `Unsupported`. Those cfg paths were source-reviewed but were not
  compiled locally because the targets are unavailable.
- The in-repository static-Codex public API fixture passes. A real Meridian
  dependency upgrade and request assertion remain downstream integration
  evidence, not Norn's clean-checkout Gate C.
- The separately reported exchange-changeset review artifact is absent and is
  not counted as evidence.

## Requested reviewer action

1. Review `41ea210...HEAD` from a clean checkout, with particular attention to
   the five logical closure commits from `cca0432` through `ebb82c8`.
2. Read the source review, remediation plan, July decisions, archived
   provisional reports, and this handoff.
3. Rerun the Gate C commands and independently inspect the LOC, bypass, marker,
   module-shape, and secret-fixture claims.
4. Threat-model the integrated credential destination, workspace command/read,
   static-Codex, terminal-diagnostic, and private-artifact boundaries.
5. Return `READY` only if the complete P0 outcome has no unresolved phase-owned,
   phase-introduced, or newly unowned defect at any severity. Otherwise return
   `NOT READY` with reproducible findings.

P1 must not begin until this whole-phase Gate D review returns `READY` and the
P0 evidence-ledger row is updated.
