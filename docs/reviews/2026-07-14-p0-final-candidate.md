# P0 final candidate and evidence package

**Date:** 2026-07-14
**Phase base:** `41ea210d24ec0653480be3a097b15adcb1e4bfb0`
**Tested code head:** `bfa0b8e96ec6501feb85fff5b369b82db2d11656`
**Evidence package commit:** `e9b02d0`
**Status:** automated implementer Gate C complete; manual inspection, owner dispositions, and independent Gate D remain open

## Scope

This record supersedes the mutable status summary in the Gate C handoff. It does
not rewrite the historical scoped review records and does not approve P0.

The final code round after `be8cb48` contains four logical commits:

| Commit | Review unit |
|---|---|
| `e218c9c` | Provider/request/SSE/tool-result non-disclosure, OAuth callback and browser-launch lifecycle, and descriptor-accounting corrections |
| `8299df0` | Removal of prohibited panic-style test-result extraction |
| `82e44f4` | Exact, independently checkable Gate C, distribution, policy, and attestation machinery |
| `bfa0b8e` | Deterministic delegated-browser completion sentinel after the first final gate exposed a test race |

The evidence package is a later documentation-only commit. Every machine result
below is bound to the clean tested code head, not silently attributed to the
packaging commit.

## Retained evidence chain

| Artifact | SHA-256 | Result |
|---|---|---|
| [`2026-07-14-p0-final-gate-82e44f4.json`](evidence/2026-07-14-p0-final-gate-82e44f4.json) | `95e126ba12e558d049c91791289ffa2f2622abc57a7f9cc4c20ef71263541b6d` | Failed first final gate: 34/35 runner cases |
| [`2026-07-14-p0-final-policy-82e44f4.json`](evidence/2026-07-14-p0-final-policy-82e44f4.json) | `fd07a1696119a838b26b78c4b1f4b61327b35ecc999d73a27f20db4359757ef6` | Policy pass at the failed-gate code head |
| [`2026-07-14-p0-final-gate-bfa0b8e.json`](evidence/2026-07-14-p0-final-gate-bfa0b8e.json) | `d42429d66a0e4c68fee5cb45ba09e074fcf51d40da26eecfe0ed519090e81856` | 35/35 runner cases; 9,205 Rust test executions |
| [`2026-07-14-p0-final-policy-bfa0b8e.json`](evidence/2026-07-14-p0-final-policy-bfa0b8e.json) | `2a15da2134979b32a8c95dc87d4a6c182c627369fc39ae921387d241feff6dc2` | Full-range policy pass |
| [`2026-07-14-p0-final-distributions-bfa0b8e.json`](evidence/2026-07-14-p0-final-distributions-bfa0b8e.json) | `c625f79668441f59c27a5b168a0aba1180562aac146d9965db2d428b95c37d9a` | 750/750 observations; 1,170 Rust test executions |
| [`2026-07-14-p0-final-attestation-bfa0b8e.json`](evidence/2026-07-14-p0-final-attestation-bfa0b8e.json) | `83c4621413ba756081e0ee649b35e7173e9451307d0cd16674afa0e264a05422` | Pass; no attestation errors |

The attester rejects duplicate/non-finite JSON, unexpected commands, test
identities, counts, run profiles, pass fields, tool identities, and artifact
controlled interpreters. It independently regenerates and semantically compares
the policy artifact. This is mechanical validation, not cryptographic
provenance: a fully fabricated self-consistent bundle remains indistinguishable
without a trusted signed execution service.

## Failed-first-run disclosure

The first final Gate C attempt at `82e44f4` failed only
`workspace_all_targets`. The workspace reported 3,320 passing and one failing
test:

`provider::openai_oauth::browser::tests::dropping_delegated_launcher_neither_waits_for_nor_terminates_child`

The test waited for the output file to exist. Shell redirection could create an
empty file before the delegated child wrote its completion marker, so the
assertion raced the child rather than testing the intended lifecycle property.
`bfa0b8e` waits for the exact sentinel contents while treating only partial
contents and `NotFound` as incomplete. It adds no retry to production code, no
ignore, no lint bypass, and no weakened assertion. A focused 20/20 distribution
passed before the complete final gate and the broader retained distribution.

## Automated Gate C

The corrected clean-head automated run passed all 35 exact runner cases. It covers:

- formatting, strict workspace/all-target Clippy, workspace/all-target check,
  workspace/all-target tests, touched-crate integration surfaces, workspace
  doctests, and eight `test-utils` authority doctests;
- full-range `41ea210...bfa0b8e` diff and policy checks;
- 23 exact non-disclosure sentinels across MCP, provider request/stream errors,
  OAuth, debug metadata, tool results, and loop assembly/dispatch; and
- one separately labelled model-facing panic-conversion sentinel.

The aggregate is 35/35 runner cases, zero failures, 9,205 Rust test executions,
and unchanged clean repository state before and after execution.

## Repeated distributions

The serial distribution run contains 33 exact cases and 750 observations:

- three macOS/APFS concurrency cases at 50/50 each;
- descriptor retention/admission, cancellation, task, OAuth launcher, Gate D
  correction, and complete 17-case PTY surfaces at 20/20 each;
- MCP startup and live-control/catalogue/child-view cases at 20/20 each; and
- seven OAuth callback lifecycle cases at 20/20 each.

All 750 observations passed, representing 1,170 Rust test executions. The
runner executes Cargo cases serially to avoid manufacturing descriptor pressure
through the evidence harness itself. Each case records its exact command and
expected identities; every observation records result, duration, output digest,
test count, and observed identity.

## Policy and writer inventory

The syntax-aware full-range policy covers 333 changed Rust files, of which 62
are test-only. It reports:

- zero changed production files over 500 production LOC;
- zero thin-entrypoint violations;
- zero added unwrap, expect, panic, lint-suppression, ignored-test, empty-cfg,
  unresolved-marker, `todo!`, or `unimplemented!` matches; and
- 97 conservative production/build-script filesystem-mutation candidates.

The semantic file/text set increases by five rows from the historical 92-row
snapshot. The five new rows are the private root, lock,
temporary creation/cleanup, and atomic publication operations in
`config/mcp_patch.rs`. They are classified in the artifact-writer inventory as
explicit operator-directed MCP settings mutation, with private storage for user
and private-local scopes and confined workspace-document replacement for shared
project scopes.

## Explicit residuals

The following are not hidden inside a broad P0 pass claim:

- The retrospective Gate A timing exception and Gate B source-proof disposition
  still require explicit owner approval. No document can reconstruct missing
  historical red runs.
- Gate C's current-head manual secret inspection and reviewer-verified LOC/bypass
  reproduction remain open. The retained machine policy and attester do not
  substitute for that independent inspection.
- Whole-phase Gate D remains `NOT READY` until a fresh independent reviewer
  reproduces the evidence and returns `READY`.
- On macOS, OAuth authorization data is delivered to fixed `/usr/bin/osascript`
  over stdin and is absent from argv/environment. The test proves command and
  JXA construction; it does not invoke `NSWorkspace` end to end.
- Linux/BSD desktop opener APIs still receive the authorization URL in argv.
  Launcher lookup is restricted to fixed trusted system directories and a safe
  child environment, but argv non-disclosure parity with macOS is not claimed.
- Windows and other unsupported targets currently return a typed unsupported
  browser-login result. Cross-target compilation was not proven by the macOS
  Gate C run.
- The panic sentinel proves conversion to a bounded model-facing error. It does
  not claim that Rust's default panic hook cannot write the panic payload to
  process stderr.
- Trusted raw debug destinations and protocol-required tool/call identifiers
  are not covered by a blanket redaction promise. The proven property is that
  tested credential/provider-controlled sentinels and authorization URLs on the
  macOS path do not reach the exact user/model-facing surfaces in the manifest.
- The retained attestation is deterministic and adversarially checkable but is
  not signed execution provenance.
- P2 still owns `AUTH-01` through `AUTH-07`, `CONFIG-01`, `CONFIG-02`, and
  named-account work. The final P0 round fixes callback/browser lifecycle and
  disclosure defects; it does not silently pull the entire P2 scope into P0.

## Reviewer action

1. Review the logical code commits through `bfa0b8e`, then inspect the later
   evidence and documentation package separately.
2. Re-run the integrated runner and attester from a clean checkout at
   `bfa0b8e`; do not trust the summary tables alone.
3. Manually inspect the current-head fixtures/evidence for secrets and reproduce
   the syntax-aware LOC/bypass review across all 333 changed Rust files.
4. Reconcile all 97 writer candidates and the complete descriptor-permit
   inventory.
5. Challenge the explicit residual classifications rather than treating them
   as accepted owner decisions.
6. Return a whole-phase `READY` or `NOT READY` verdict. P1 does not begin until
   P0 is accepted.
