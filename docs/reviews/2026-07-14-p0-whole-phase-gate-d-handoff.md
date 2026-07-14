# P0 whole-phase Gate D handoff

**Date:** 2026-07-14
**Phase base:** `41ea210d24ec0653480be3a097b15adcb1e4bfb0`
**Final tested candidate:** `13d661c906f00e9ff54541e8362086874e98af1c`
**Pinned-toolchain evidence package:** `564af2d`
**Status:** ready for independent whole-phase Gate D; P0 is not accepted

## Why this candidate exists

The external round review accepted its scoped changes but recommended pinning
the repository's floating Rust toolchain. Tom approved that recommendation and
the two required P0-only historical-process exceptions on 2026-07-14. Commit
`13d661c` pins Rust 1.94.0 with Clippy and rustfmt and removes the unproven Rust
1.85 minimum claim from the README. It changes no Rust source or intended Norn
product behavior.

The prior `bfa0b8e` evidence remains historical and unmodified. This package
reruns rather than relabels the complete gate at the pinned candidate.

## Owner dispositions

Tom approved the exact decisions recorded in `DECISIONS-2026-07.md` section 11:

1. P0 alone may use a retrospective Gate A exception for owner decisions and
   evidence-method agreement that were not durably recorded before
   implementation. The historical boxes remain unchecked, and later phases
   receive no exception.
2. P0 alone may use retained source or positive-characterization proof plus
   exact candidate regressions where Git has no native pre-fix executable
   state. The native `openat` red-green evidence remains required. No current
   technical or review requirement is waived.
3. The repository gate toolchain is Rust 1.94.0. No lower MSRV is claimed.

## Retained evidence chain

All files remain byte-for-byte under one directory so their internal basenames
remain exact.

| Artifact | SHA-256 | Result |
|---|---|---|
| [`gate.json`](evidence/2026-07-14-p0-toolchain-13d661c/gate.json) | `878165a0fccce565cf318dda939a24cec823618d8071dcd737f821692509d911` | Sandboxed precursor: 28/35 |
| [`policy.json`](evidence/2026-07-14-p0-toolchain-13d661c/policy.json) | `1b87da66daf353d424eb13c638eacc9e3cfbf372dccdb70c4020f449773b8325` | Policy pass paired with sandboxed precursor |
| [`gate-native.json`](evidence/2026-07-14-p0-toolchain-13d661c/gate-native.json) | `b75fae12d37c8414b4ed1a83234054294cb1b094e80f16cb3e200946ca3b69d1` | Native-host Gate C: 35/35, 9,205 Rust tests |
| [`policy-native.json`](evidence/2026-07-14-p0-toolchain-13d661c/policy-native.json) | `1b87da66daf353d424eb13c638eacc9e3cfbf372dccdb70c4020f449773b8325` | Full-range policy pass |
| [`distributions-native.json`](evidence/2026-07-14-p0-toolchain-13d661c/distributions-native.json) | `0153db4715a8b5cc72181a9c1deb7988312ce75ac508d8359fed297f155b4c14` | 750/750 observations, 1,170 Rust tests |
| [`attestation-native.json`](evidence/2026-07-14-p0-toolchain-13d661c/attestation-native.json) | `19ba8818a0b82367271dfb0ef4ac80e3e1577f84502999bd37f474cf16d2cc9f` | Pass, zero errors |

The native workspace battery contains 4,624 passing tests and no failures. The
policy covers 333 changed Rust files, including 62 test-only files, and reports
zero production files above 500 LOC, zero thin-entrypoint violations, zero
prohibited added-line matches, and 97 conservative writer candidates.

## Failed precursor classification

The first clean run executed inside a restricted sandbox. The implementer
observed loopback `bind` fail with `PermissionDenied` throughout the failing
output. The retained artifact proves seven failed runner cases: the workspace,
Norn, and CLI integration batteries plus four exact HTTP/OAuth non-disclosure
sentinels. It also proves 69 failures in each of the broad workspace and Norn
batteries and three in the CLI battery. Formatting, strict Clippy, workspace
check, doctests, policy, repository integrity, and the other runner cases
passed.

The evidence schema retains byte counts and SHA-256 output digests rather than
raw failure text or every broad-suite failed test identity. It therefore does
not independently prove that every broad-suite failure was listener-dependent;
the `PermissionDenied` cause is an implementer observation, not a claim derived
from the retained JSON alone.

The exact runner was then executed on the native host with Cargo offline and
the same environment-sanitization contract. All 35 cases passed. The failed
artifact is retained because an environmental false start is still part of the
evidence chronology; it is not described as a product regression.

## Review packet and qualification

The final integrator must be a fresh rigorous Fable reviewer who implemented
none of P0 and was not a primary approver of its scoped rounds. The reviewer
receives all of the following:

- the original
  [`Responses implementation source review`](2026-07-10-responses-api-implementation-review.md);
- the current
  [`remediation plan`](../RESPONSES-API-REMEDIATION-PLAN.md) and this handoff;
- the complete `git diff --no-ext-diff 41ea210...13d661c`, not only the final
  toolchain commit;
- the retained evidence directory and every linked scoped/corrective review;
- the official `openai/codex` source pinned at
  [`325cf161940c4be5d5792dc09940624ba7543b44`](https://github.com/openai/codex/tree/325cf161940c4be5d5792dc09940624ba7543b44),
  resolved from the official remote's `HEAD` on 2026-07-14; and
- the exact pinned Codex
  [request builder](https://github.com/openai/codex/blob/325cf161940c4be5d5792dc09940624ba7543b44/codex-rs/core/src/client.rs),
  [Responses SSE parser](https://github.com/openai/codex/blob/325cf161940c4be5d5792dc09940624ba7543b44/codex-rs/codex-api/src/sse/responses.rs),
  [response-item model](https://github.com/openai/codex/blob/325cf161940c4be5d5792dc09940624ba7543b44/codex-rs/protocol/src/models.rs),
  and [login server](https://github.com/openai/codex/blob/325cf161940c4be5d5792dc09940624ba7543b44/codex-rs/login/src/server.rs).

The 2026-07-10 source review originally linked mutable `main` URLs. The pinned
revision above is an immutable current review input; it is not retroactively
described as proof of the exact `main` state observed on 2026-07-10. P1 still
owns the broader public-documentation and official-source snapshot contract for
all later phases.

## Reproduction commands

Run from a clean detached checkout at `13d661c`. The target and output paths
must be absolute and outside the checkout. The test process must be permitted
to bind loopback listeners; it makes no live provider request, and the runner
sets Cargo offline. The retained distribution is Darwin/macOS evidence and its
runner requires the target directory to reside on APFS; another platform may
perform additional review but cannot reproduce this artifact's host contract.

```sh
OUT=/absolute/path/outside-the-checkout

python3 docs/reviews/evidence/run_p0_integrated_evidence.py \
  --target-dir "$OUT/gate-target" \
  --output "$OUT/gate.json" \
  gate \
  --policy-output "$OUT/policy.json"

python3 docs/reviews/evidence/run_p0_integrated_evidence.py \
  --target-dir "$OUT/distribution-target" \
  --output "$OUT/distributions.json" \
  distributions \
  --concurrency-runs 50 \
  --other-runs 20

python3 docs/reviews/evidence/attest_p0_evidence.py \
  --gate "$OUT/gate.json" \
  --distributions "$OUT/distributions.json" \
  --output "$OUT/attestation.json"
```

Do not overlap the gate and distribution processes. The distribution runner is
serial so its harness does not manufacture descriptor pressure.

## Required independent review

The qualified final reviewer or panel must perform all of the following rather
than promoting the scoped round review to a whole-phase verdict:

1. Record the exact clean documentation-packaging commit supplied for review,
   then inspect its fixtures and retained evidence for credentials, real
   account identifiers, private prompts, reusable turn state, and raw cache
   keys. Do not run the source-head attester from this later packaging commit.
2. At a clean detached `13d661c`, independently regenerate and inspect the
   syntax-aware policy across all 333 changed Rust files, including the 62-file
   test-only classification, LOC, entrypoint, and every prohibited added-line
   category.
3. Reconcile all 97 writer candidates against
   [`2026-07-12-p0-artifact-writer-inventory.md`](2026-07-12-p0-artifact-writer-inventory.md).
4. Answer every still-applicable D1E adversarial question. Trace descriptor
   permits through acquisition, transfer, success, spawn failure, timeout,
   cancellation, foreground adoption, transport drop, and shutdown; challenge
   false refusal and live-limit revalidation; and verify the Chiron W8-to-W3
   lifecycle across initial start, duplicate start, initialization failure,
   crash, restart, shutdown, and unmanaged construction paths. Use
   [`2026-07-13-p0-descriptor-admission-candidate.md`](2026-07-13-p0-descriptor-admission-candidate.md).
5. Adversarially reproduce D1D startup provenance, precedence, approval
   fingerprinting, zero activation while pending, hostile negotiation and
   pagination, cancellation invalidation, pool/view separation, collisions,
   and real child dispatch using
   [`2026-07-14-p0-d1d-d1e-correction-candidate.md`](2026-07-14-p0-d1d-d1e-correction-candidate.md).
6. Answer all five live-MCP adversarial questions in
   [`2026-07-14-p0-mcp-live-candidate.md`](2026-07-14-p0-mcp-live-candidate.md),
   including generation/runtime pairing, stale watcher behavior, contextual
   interleaving, disclosure surfaces, and approval/provenance reuse.
7. Recheck credential destinations, redirects, automatic workspace commands
   and reads, project-originated trust, and every residual classification in
   the superseded [`P0 final candidate`](2026-07-14-p0-final-candidate.md).
8. Inspect the complete `41ea210...13d661c` implementation/test diff and the
   pinned official-source inputs, then rerun the integrated gate, distribution,
   policy, and attester from the clean exact source head. Resolve every
   phase-owned or newly exposed defect with a checked correction round.
9. Return one fresh whole-phase verdict: `READY` or `NOT READY`. Only a `READY`
   verdict permits the P0 acceptance boxes and evidence ledger to be populated.

The prior
[`2026-07-14 P0 round review`](2026-07-14-p0-round-review.md) accepted its own
scope but explicitly desk-verified and spot-checked D1E, D1D startup, and live
MCP rather than adversarially reproducing them. It is input to this review, not
a substitute for it.
