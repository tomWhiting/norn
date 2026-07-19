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
