# P0 whole-phase correction review handoff

- **Campaign date:** 2026-07-14
- **Published (Australia/Melbourne):** 2026-07-15
- **Status:** correction candidate ready for focused independent review; P0 is not accepted
- **Campaign base:** `41ea210`
- **Controlling review:** `c6bf1e2` / `2026-07-14-p0-whole-phase-gate-d-review.md`
- **Corrected source head:** `e1bf7f2ab79157a3ee7f49270c6e7ad61794a077`
- **Correction range:** `c6bf1e2..e1bf7f2`
- **Initial documentation/evidence package:** `7648159529370275badf76a228e11f3bd5f61330`
**Full P0 range for the deferred seam sweep:** `41ea210..e1bf7f2`

This handoff does not promote scoped reviews or machine evidence into a phase
verdict. A fresh reviewer must inspect and reproduce the corrected failure
paths, finish the deferred whole-diff seam sweep, and return `READY` or
`NOT READY`.

## Correction summary

The correction range addresses GD-1 through GD-18 from the controlling review:

- `08108db` closes structural runtime findings: descriptor admission, thin
  module boundaries, pinned dependency/spawn evidence, and CLI print-error LOC
  headroom.
- `3d5ff8c` closes MCP transport and live-control findings: provenance,
  liveness, reconnect/quarantine behavior, bounded stdio/JSON/SSE input,
  optional full-request deadlines, redirect refusal, safe stderr categories,
  startup status hydration, and enumerated TUI/runtime failure paths.
- `e4542ad` replaces the evidence machinery with path-free schema-v3 gate and
  distribution artifacts, real executable fingerprints, exact self-test and
  module-reachability inventories, qualified failure identities, and
  repository-local path enforcement.
- `fc312f4` reconciles the owner-approved MCP defaults and environment policy.
- `17a3bb8` removes a policy-prohibited panic from the remote-error fixture.
- `0e8fb49` corrects two test assumptions exposed by the first exact-head gate:
  one workspace-path substring filter and one stale private-local MCP source
  expectation.
- `e1bf7f2` accepts Cargo 1.94's backtick rerun hints across split stdout/stderr
  and pins the evidence self-test total at 43.

The MCP follow-up also closes three seams found after the first implementation:
SSE chunks emit one bounded event at a time and count ignored fields, the
optional HTTP deadline covers nested protocol handling, and hostile
`Content-Type` values never enter rendered failures. Exact-CRLF, independent
fingerprint, and no-implicit-30-second fixtures pin the related contracts.

## Retained evidence

The final exact-head regeneration kept its builds, scratch paths, and clean
worktree beneath the main repository's ignored `target/` tree. Its build
directories were removed after the JSON artifacts were validated rather than
retaining disposable build output.

| Artifact | SHA-256 | Result |
|---|---|---|
| [`2026-07-14-p0-correction-failed-policy-17a3bb8.json`](evidence/2026-07-14-p0-correction-failed-policy-17a3bb8.json) | `0074a2f5df38d6a0e0be2eb5e28e84deaf4fef5218cc21db44684c8ffa255b7a` | Full-range policy pass at the first correction head |
| [`2026-07-14-p0-correction-failed-gate-17a3bb8.json`](evidence/2026-07-14-p0-correction-failed-gate-17a3bb8.json) | `5d7f9b0acf382570f5ebdaa17938a9e601f336814e45957e8d8e5a874c5b0050` | Failed-first-run evidence: 35/38 cases, 7,921 Rust executions |
| [`2026-07-14-p0-correction-policy-e1bf7f2.json`](evidence/2026-07-14-p0-correction-policy-e1bf7f2.json) | `e61acb565377989891250329f6e10f9187fe46f36294f2db65d715d2680e7abe` | 359 changed Rust files, 65 test-only files, 97 writer candidates, policy pass |
| [`2026-07-14-p0-correction-gate-e1bf7f2.json`](evidence/2026-07-14-p0-correction-gate-e1bf7f2.json) | `833492dcf2db2c7873330baa1aca13ed70a573febc4796f74ce965f35ba28225` | Gate C 38/38, 9,299 Rust executions |
| [`2026-07-14-p0-correction-distributions-e1bf7f2.json`](evidence/2026-07-14-p0-correction-distributions-e1bf7f2.json) | `c07c2f9aeb620edb36e4742499bf90270f4de153f760305f739b5338b9cf7bac` | 830/830 observations, 1,250 Rust executions |
| [`2026-07-14-p0-correction-attestation-e1bf7f2.json`](evidence/2026-07-14-p0-correction-attestation-e1bf7f2.json) | `ae752d86583d46f16c7a938ec0cfdd4682a51019a1ec80e1e48b4a82929c3c8b` | Pass, zero errors; binds gate, distributions, and policy to `e1bf7f2` |

The corrected gate includes formatting, strict workspace Clippy with
`-D warnings`, workspace check, workspace all-target tests, complete tests for
`norn`, `norn-cli`, and `norn-tui`, doctests, exact phase diff check, 43/43
evidence self-tests, full-range policy, 25 non-disclosure sentinels, one
separately classified model-facing sentinel, and repository-integrity checks.

The full-range policy reports:

- zero added unwrap, expect, panic, suppression, ignored-test, debt-marker,
  `todo!`, or `unimplemented!` matches;
- zero production files over 500 LOC;
- zero thin-entrypoint or production-`mod.rs` shape violations; and
- the complete conservative inventory of 97 filesystem writer candidates.

## Failed-first-run classification

The first exact-head gate at `17a3bb8` is retained rather than overwritten. It
failed three runner cases: workspace all-targets, `norn` tests, and `norn-cli`
tests. The JSON mechanically retains those runner identities, failed-test
names, counts, and output digests, but not raw diagnostics. The implementer
observed the following causes in the captured output and then reproduced each
affected fixture directly:

- five diagnostics Unix-socket fixtures exceeded macOS `SUN_LEN` because the
  repository-local evidence target name was unnecessarily long;
- one search fixture treated the repository-local `TMPDIR` ancestor's
  `/target/` component as if every result belonged to the fixture's ignored
  `target` directory; and
- one CLI fixture still expected private `settings.local.json` MCP servers to
  have shared-project provenance and remain approval-gated.

The next run used the shorter repository-local `target/build/g` path. The search
test now strips its fixture root before component-aware ignore comparison, and
the CLI test proves private-local provenance and direct non-gated startup. The
failed artifact retains the test names, but its target identities are marked
incomplete because its parser predated Cargo 1.94 backtick support. That parser
defect is fixed and covered at `e1bf7f2`; the failed artifact remains unchanged.

## Owner disposition

Tom approved removal of the six superseded schema-v2 artifacts from current
`HEAD` on 2026-07-15, as recorded in `DECISIONS-2026-07.md` section 12. They
contained local operator paths, ambient variable names, fixed sanitized build
values, and value hashes, but no values of the removed ambient variables and no
credentials. The immutable historical inventory is:

| Artifact removed from current `HEAD` | Historical package | SHA-256 |
|---|---|---|
| `evidence/2026-07-14-p0-final-distributions-bfa0b8e.json` | `e9b02d0` | `c625f79668441f59c27a5b168a0aba1180562aac146d9965db2d428b95c37d9a` |
| `evidence/2026-07-14-p0-final-gate-82e44f4.json` | `e9b02d0` | `95e126ba12e558d049c91791289ffa2f2622abc57a7f9cc4c20ef71263541b6d` |
| `evidence/2026-07-14-p0-final-gate-bfa0b8e.json` | `e9b02d0` | `d42429d66a0e4c68fee5cb45ba09e074fcf51d40da26eecfe0ed519090e81856` |
| `evidence/2026-07-14-p0-toolchain-13d661c/distributions-native.json` | `564af2d` | `0153db4715a8b5cc72181a9c1deb7988312ce75ac508d8359fed297f155b4c14` |
| `evidence/2026-07-14-p0-toolchain-13d661c/gate-native.json` | `564af2d` | `b75fae12d37c8414b4ed1a83234054294cb1b094e80f16cb3e200946ca3b69d1` |
| `evidence/2026-07-14-p0-toolchain-13d661c/gate.json` | `564af2d` | `878165a0fccce565cf318dda939a24cec823618d8071dcd737f821692509d911` |

Direct links in the historical handoffs are replaced by commit/hash references.
The path-free schema-v3 gate/distribution artifacts and path-free schema-v2
policy/attestation artifacts above remain the current machine evidence.
This current-head deletion does not purge the files from pushed Git history;
the package commits preserve reviewer retrieval and exact content provenance.

No other P0-blocking owner disposition remains. The Gate A and Gate B
retrospective exceptions are already approved in `DECISIONS-2026-07.md`; their
historically false checkboxes remain unchecked by design. `RLIMIT_CORE=0`
remains a separately held, non-P0 decision because process-level inheritance
would also disable core dumps for user commands spawned by Norn.

## Reproduction

Use a clean detached worktree inside the main repository's ignored `target/`
tree. Do not use `/tmp`, `/private/tmp`, the operating-system temporary
directory, or an external Cargo target. Do not overlap Cargo processes. The
native run must be permitted to bind local Unix and loopback sockets.

```sh
MAIN_REPO="$(git rev-parse --show-toplevel)"
REVIEW_WT="$MAIN_REPO/target/worktrees/p0-correction-review"

mkdir -p "$MAIN_REPO/target/worktrees"
mkdir -p "$MAIN_REPO/target/evidence/p0-review"

git worktree add --detach "$REVIEW_WT" e1bf7f2
cd "$REVIEW_WT"

python3 docs/reviews/evidence/run_p0_policy_evidence.py \
  --base 41ea210 \
  --head e1bf7f2 \
  --output "$MAIN_REPO/target/evidence/p0-review/policy.json"

python3 docs/reviews/evidence/run_p0_integrated_evidence.py \
  --target-dir "$MAIN_REPO/target/build/p0g" \
  --output "$MAIN_REPO/target/evidence/p0-review/gate.json" \
  gate \
  --policy-output "$MAIN_REPO/target/evidence/p0-review/policy.json"

python3 docs/reviews/evidence/run_p0_integrated_evidence.py \
  --target-dir "$MAIN_REPO/target/build/p0d" \
  --output "$MAIN_REPO/target/evidence/p0-review/distributions.json" \
  distributions \
  --concurrency-runs 50 \
  --other-runs 20

python3 docs/reviews/evidence/attest_p0_evidence.py \
  --gate "$MAIN_REPO/target/evidence/p0-review/gate.json" \
  --distributions "$MAIN_REPO/target/evidence/p0-review/distributions.json" \
  --policy "$MAIN_REPO/target/evidence/p0-review/policy.json" \
  --output "$MAIN_REPO/target/evidence/p0-review/attestation.json"
```

Gate and distribution targets must be fresh directories. Reviewers should
remove disposable build directories after retaining and hashing the JSON
artifacts if disk pressure matters.

## Required focused review

The focused reviewer must:

1. Verify the GD-1 through GD-18 correction paths in
   `c6bf1e2..e1bf7f2`, including failure and cancellation behavior rather than
   only success fixtures.
2. Re-run the complete evidence chain at exact source head `e1bf7f2` and report
   distributions with denominators, not a final sample.
3. Manually inspect all 359 changed Rust files for LOC and bypass-policy
   agreement with the machine inventory.
4. Manually inspect fixtures and retained evidence for credentials, real
   account identifiers, private prompts, reusable turn state, raw cache keys,
   local paths, and ambient secret-variable names.
5. Reconcile all 97 writer candidates and the D1E descriptor-permit lifecycle,
   including success, failure, timeout, cancellation, adoption, and shutdown.
6. Re-run the complete MCP startup/live-control matrix, transport bounds,
   deadlines, stderr non-disclosure, reconnect/quarantine, generation/runtime
   pairing, and TUI failure paths.
7. Complete the whole-diff seam sweep that the controlling review explicitly
   deferred, using `41ea210..e1bf7f2` and the pinned official Codex source
   inputs from the historical whole-phase handoff.
8. Confirm the completed GD-15 deletion, historical commit/hash provenance, and
   path-free replacement evidence are reflected in current `HEAD`.
9. Return one focused whole-phase verdict: `READY` or `NOT READY`.

P1 must not start until the focused review returns `READY`. Gate D, P0
acceptance, the final evidence-ledger row, and the P0 roadmap checkbox remain
open until that verdict is recorded.
