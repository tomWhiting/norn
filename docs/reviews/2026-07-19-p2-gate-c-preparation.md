# P2 Gate C and live A/B/A preparation

**Status:** deterministic evidence machinery prepared; no Gate C result, live
credential experiment, Gate D verdict, or P2 acceptance is claimed.

## Frozen scope

- D14 base: `6669b9d81acc0d57ab8eb9056b5fb7abb3ee1907`.
- P2 implementation and fixture candidate:
  `fcd1b30647123f1db8ff5d93efffc9fa818fba02`.
- Accepted integration anchor when this package was prepared:
  `2ee67c5b708c6eb4f57bb9ffb494960a49869de0`.
- The runner mechanically inventories the dedicated auth product paths changed
  by P2 and requires their blob identities to be unchanged at the integration
  anchor and evidence-package commit. It tests the exact P2 candidate from a
  Git archive below the shared repository `target/`; it does not re-gate or
  alter accepted P3/P4.

## Deterministic evidence package

`docs/reviews/evidence/p2/p2_contract.json` binds 15 exact phase cases to
`AUTH-01..07` and `CONFIG-01..02`, and pre-registers nine concurrency-sensitive
tests at 20 runs each: 180 observations with a fixed denominator. The runner
also executes the complete `norn` and `norn-cli` integration surfaces, strict
workspace Clippy, fmt, workspace/all-target tests, doctests, diff check, the
syntax-aware full-range policy audit, and redaction self-tests.

Run only from a clean evidence-package commit:

```bash
python3 -I -S -B docs/reviews/evidence/p2/run_p2_final_evidence.py \
  --gate target/evidence/p2-gate.json \
  --policy target/evidence/p2-policy.json \
  --distributions target/evidence/p2-distributions.json \
  --redaction target/evidence/p2-redaction.json \
  --output target/evidence/p2-machine-attestation.json
```

The machine attestation deliberately records `phase_acceptance: false` and
`live_aba: required_not_included`. A green deterministic run therefore cannot
be presented as P2 acceptance.

## Credentialed A/B/A protocol

The live runner is opt-in and performs no action unless all three conditions
are supplied: the exact approval phrase, a valid alias for account A, and a
different valid alias for account B. It builds the frozen P2 source into the
shared repository target, then performs this sequence:

1. Browser-login account A into a named Norn-owned slot.
2. Force-refresh A, reconstruct the provider from durable storage, and verify
   bearer and account headers are available without sending the dummy request.
3. Browser-login account B into a different named slot.
4. Force-refresh and durably reload B.
5. Force-refresh and durably reload A again after B's login and refresh.

The second named login rejects a duplicate remote identity, so successful
completion cannot be manufactured by using the same account under two aliases.
The retained JSON contains only step labels and booleans. It contains no alias,
account identifier, credential, request header, raw child output, or hash of
private material.

The harness can be compiled against the frozen source without credentials or
network dispatch by running:

```bash
python3 -I -S -B docs/reviews/evidence/p2/run_p2_live_aba.py --validate-only
```

With the operator present to select the two browser accounts:

```bash
NORN_P2_LIVE_APPROVAL=I_APPROVE_P2_LIVE_ABA_CREDENTIAL_USE \
NORN_P2_LIVE_ACCOUNT_A=<unused-local-alias-a> \
NORN_P2_LIVE_ACCOUNT_B=<unused-local-alias-b> \
python3 -I -S -B docs/reviews/evidence/p2/run_p2_live_aba.py \
  --output target/evidence/p2-live-aba.json
```

The runner does not automatically log either account out. Automatic cleanup
would revoke credentials and could destroy evidence needed for review. The
operator decides whether to keep or explicitly log out the two named slots
after the review.

## Remaining acceptance work

1. Commit this evidence package without changing P2 product paths.
2. Run and retain the deterministic bundle from that clean commit.
3. With explicit operator/browser participation, run and retain the A/B/A
   artifact. A failed or absent artifact blocks the simultaneous named-account
   claim and P2 acceptance.
4. Run the redaction validator over the live artifact before handoff.
5. Obtain independent security/auth, concurrency/persistence, and adversarial
   Gate D review. Only a final `READY` verdict may check the P2 acceptance boxes.

No credential, browser login, token refresh, or external provider request was
used while preparing this package.

## First deterministic attempt

The first clean-main invocation on 2026-07-19 exited `2` after 0.412 seconds,
before any build or test, with `ignored worktree paths would invalidate P2
evidence`. It emitted no P2 evidence artifact and is not a Gate C observation.
The bootstrap had rejected every ignored path in the working tree, including
the mandated shared repository `target/`; clean main contained 44,290 ordinary
ignored entries.

The evidence-only correction removes that impossible global check. Tracked
changes and nonignored untracked paths still fail through `git status
--porcelain --untracked-files=all`; fixed support files remain hash-checked
against the evidence-package commit; Cargo still runs only an exact `git
archive` of `fcd1b30`; and every generated output remains confined below the
shared repository `target/`. A self-test creates an isolated Git fixture below
that target and proves an ignored `target/` entry passes while a nonignored
untracked `source.rs` fails. A corrected deterministic result remains pending.

## Second deterministic attempt

The next clean-main invocation reached every Gate C leg but did not reach the
distribution runner. It retained
`target/evidence/p2-gate.json` with SHA-256
`9341ca15b9370f300c18a1e0ce8b523287a866953f068c5a7028bdc3852de90b`.
The artifact records 24 checks, 6 passes, 18 failures, and 524.922 seconds of
summed check time. The passes were the public embedder selector (`1/1`), fmt,
strict workspace Clippy, doctests (`8/8`), redaction self-tests (`25/25`), and
the frozen-range diff check.

This is a retained failed harness observation, not a product verdict:

- Fourteen focused selectors exited successfully after running zero tests. The
  contract named source fixture modules rather than the complete compiled Rust
  test identities. The sole integration-test selector ran `1/1`.
- The exact-source `norn` fence recorded 3,471 passing executions and 107
  failures. Every directly rendered root panic in the retained run was an OS
  `PermissionDenied` while binding a loopback listener inside the execution
  sandbox; the remaining failures were dependent or aggregated failures in
  the same listener-backed targets. The `norn-cli` fence recorded 527 passes
  and three failures, all at the JSON-RPC test stub listener with the same OS
  error. The workspace fence repeated those failures and also reported the
  compile-fail target red. These sandbox results cannot satisfy Gate C and
  cannot establish a product regression.
- The policy leg exited with `IndexError`. The reused P0 scanner computes diff
  line numbers for `6669b9d..fcd1b30` but had read the later main-worktree file
  bodies. That scanner's valid precondition is that its worktree is the
  requested head.

The narrow evidence correction now uses the real fully qualified identities
for all 15 focused cases and all nine repeated cases. Before any gate case it
compiles each distinct frozen-source target with `--list` and requires every
declared identity to occur exactly once; a basename grep can no longer certify
a vacuous selector. The policy leg runs the package-pinned syntax-aware scanner
against files from a detached `fcd1b30` worktree under the repository's ignored
`target/worktrees/` lane. Cargo output and retained artifacts continue to use
the shared repository `target/`; `/tmp` is not used. The failed artifact above
must be replaced, not reinterpreted, by a clean corrected run.

The correction checks are non-vacuous. A compiled `--list` inventory of the
exact frozen source resolved all required identities exactly once: 17 unique
selectors within 3,554 Norn library tests, one within the two-test public API
target, and three within 482 CLI library tests. The historical policy check
then passed over 102 changed Rust files with no over-500 production file, thin
entrypoint violation, module-shape violation, or added-line policy match. Its
intermediate artifact is
`target/evidence/p2-policy-historical-check.json`, SHA-256
`192b076447311ffa173ebc7e9a11e7cba386ef1a2ad058eb45ca5f5e32989123`.
Two earlier policy correction probes emitted no artifact: the P2-era scanner
could not parse a statement-level `#[cfg(test)]`, and the first package-scanner
probe retained a rule path relative to the wrong worktree. Both premises now
fail in focused tests or explicit source-binding checks rather than during the
final gate.
