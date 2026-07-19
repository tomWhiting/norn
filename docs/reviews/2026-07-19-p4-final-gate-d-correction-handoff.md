# P4 final Gate D correction handoff

**Date:** 2026-07-19

**Status:** READY FOR COORDINATOR-ONLY SAME-REVIEWER CONFIRMATION; not P4
acceptance

**Subsequent result:** Same-reviewer confirmation
[`0095f5c`](2026-07-19-p4-final-gate-d-correction-review.md) closes MAJOR-1 and
MINOR-2 and returns corrected P4 Gate D `READY`. The remediation plan records
owner-authorized P4 acceptance; the handoff below remains the historical
pre-verdict submission.

**Historical P4 verdict:** `NOT READY` at
[`80f0e36`](2026-07-18-p4-final-gate-d-review.md)

**Product correction:** `ab26632c00a92bd8f2effd0462ae0cd419ebe1a6`

**Product correction tree:** `d337970c4a8dce893f8244a63e095559347f7396`

**Evidence-scanner correction:** `180759f506d9ca365999dab645794a8fc833a44f`

**Evidence-scanner tree:** `c588d96639cd6e7725255779afd445d479cbcd69`

**Retained evidence commit:** `8faf1f4cbc5c75254877a77a9db688304f429eaa`

**Tracking plan:**
[`RESPONSES-API-REMEDIATION-PLAN.md`](../RESPONSES-API-REMEDIATION-PLAN.md)

## Verdict requested

Perform the narrow same-reviewer confirmation prescribed by the original P4
review. Do not repeat the whole panel unless the correction opens a new seam.
Confirm whether `MAJOR-1` and `MINOR-2` are closed and return one corrected
P4-only verdict. P4 remains unaccepted until that verdict is recorded.

The expected verdict, if the source and evidence below reproduce, is:

> `MAJOR-1 CLOSED; MINOR-2 CLOSED; corrected P4 Gate D READY.`

If either claim does not hold, return `NOT READY` with the precise remaining
defect. This handoff does not pre-issue the verdict.

## Correction scope

The historical review at `80f0e36b3b6ff14bb22a3e45bb67951713ec3a1b`
found one product defect and one wording defect. The product correction is the
single commit `80f0e36..ab26632`. The following commit, `180759f`, changes only
the redaction evidence scanner and its tests so retained evidence can be
rescanned without mistaking exact redaction-rule identifiers for private prompt
content. Contextual private prompt content remains rejected. The five generated
artifacts are retained by `8faf1f4`.

No P3 product source changed. P3 remains accepted at whole-phase verdict
`06be7c7` for source `7f47218`; this correction and requested verdict apply only
to P4.

## MAJOR-1 correction

Successful and incomplete terminal responses now validate every identity with
accumulated core preview deltas against the authoritative terminal output-item
set. The covered channels are output text, refusal, reasoning text, and
reasoning summary text. If a core preview identity is absent from
`response.completed` or `response.incomplete`, reconciliation returns the typed,
payload-free `CoreDeltaAbsentFromTerminal` error.

This closes the reported path in which bare preview text for an announced item
could survive an empty terminal `output` array and become answer text,
persistence, or replay. Tests cover all four core channels across successful and
incomplete terminal authority, including the exact mapper-level exploit, and
also preserve the valid case where terminal authority supplies the item without
an intermediate channel-done event.

`response.failed` deliberately preserves the provider's authoritative failure
instead of replacing it with the new reconciliation error. The regression
fixture uses an empty terminal `output`, pins a typed 503-class provider failure,
and proves that no canonical response item or `Done` event is published in that
case. Authoritative output items carried by a failed response remain preserved
before the typed failure.

## MINOR-2 correction

The claim is narrowed to the behavior actually provided:

> Exact duplicate sequences processed before terminal delivery remain
> raw-observable but are canonical and actionable no-ops. The first terminal
> outcome closes the request; direct post-terminal mapper input fails closed.
> Conflicting duplicates and identity rebinding fail closed.

No product behavior was changed merely to satisfy broader wording. The direct
mapper regression continues to require `PostTerminalFrame` for an exact terminal
retransmit. A real provider-stream regression with duplicate terminal frames
requires exactly one raw terminal event, one canonical item set, one `Done`, one
usage result, one response ID, and no error.

## Regenerated machine evidence

The final runner used the primary repository's normal `<repo>/target` directory
and native-host loopback fixtures. No OS temporary build directory and no
worktree-local `target` directory were used.

Two precursor runs are invalidated and are not part of the retained bundle. A
sandboxed run could not bind the required loopback fixtures and therefore failed
the affected HTTP/OAuth tests. A native-host run at `ab26632` passed the product
gate, policy, and 60/60 distributions, but the redaction stage correctly withheld
attestation after its scanner misclassified 214 exact
`private_prompt_content` schema keys in the previously retained redaction report.
Those four intermediate documents were not committed. Commit `180759f` fixes
that exact self-scan false positive, retains negative private-content coverage,
and the complete five-artifact runner was then executed again from the start.

All five artifacts bind to evidence source
`180759f506d9ca365999dab645794a8fc833a44f`, tree
`c588d96639cd6e7725255779afd445d479cbcd69`, over phase base
`a90b730091bccaeaa03ba98c3b31425e40e32dac`.

| Artifact | SHA-256 |
|---|---|
| [`final gate`](evidence/p3-p4/2026-07-19-p4-correction-gate-180759f.json) | `5da34ced37cf0481ec5e5cdc6cddf6cb1ed1ad30b2e803fb62455a73b1f85547` |
| [`policy`](evidence/p3-p4/2026-07-19-p4-correction-policy-180759f.json) | `2ecce2709d7affb52fc8a8c9cef8faf5526352e48d1e2b65d959064ae63df22b` |
| [`distributions`](evidence/p3-p4/2026-07-19-p4-correction-distributions-180759f.json) | `2c752d3c688fa53529bb1f8a3f7b8ed1454c909b0bd9c3eb53fc29c7d397cecb` |
| [`redaction`](evidence/p3-p4/2026-07-19-p4-correction-redaction-180759f.json) | `2f59f1538b9c86e8b7d5e0f8c8761412869696a32ad00b354d5d5479e046bc37` |
| [`attestation`](evidence/p3-p4/2026-07-19-p4-correction-attestation-180759f.json) | `46a88a081e0deff61f441d59723d0f515166f96567a1d0cf0aee4fbc17b69475` |

| Gate leg | Result |
|---|---|
| Pinned Rust 1.94.0 formatting | pass |
| Strict workspace, all-target, all-feature Clippy with `-D warnings` | pass |
| `norn` complete test surface | 4,042/4,042 |
| `norn-cli` complete test surface | 551/551 |
| `norn-tui` complete test surface | 700/700 |
| Workspace all-target, all-feature tests | 5,371/5,371 |
| Workspace all-feature doctests | 8/8 |
| Isolated redaction sentinels | 25/25 |
| Exact-range diff check and syntax-aware policy | pass |

The distribution evidence reports 20/20 for each of three repeated cases,
60/60 total. The policy covers 299 changed Rust files, including 78 test-only
files, and reports zero violations. The redaction report covers 219 records and
reports zero findings. The single-process attestation reports `passed: true`,
zero errors, and binds the other four artifact hashes. The final gate binds 360
changed paths through its NUL-delimited inventory hash. These command counts
overlap and are not a unique-test total.

## Unchanged boundaries

- P3 remains accepted at `06be7c7`; this correction does not reopen it.
- P4 remains unaccepted pending the requested same-reviewer confirmation.
- D15 is unchanged. The authenticated real-wire conformance test was not run;
  it remains mandatory at D7/P9 before overall integrated Responses acceptance.
- D15 is not approval to use credentials or incur spend. Those approvals remain
  required at the point of the live test, and a skipped live test is not a pass.
- The original P4 review's non-blocking observation ledger remains carried
  unless the confirming reviewer explicitly changes it.

## Narrow confirmation checklist

1. Verify `80f0e36..ab26632` is the product-only correction and that the typed
   guard covers all four core preview channels for successful and incomplete
   terminal authority.
2. Re-run or inspect the exact exploit path and confirm it emits raw evidence,
   then `CoreDeltaAbsentFromTerminal`, with no canonical item or `Done`.
3. Confirm valid terminal authority without an intermediate channel-done event
   remains accepted.
4. Confirm the guard does not mask `response.failed`: authoritative failed-output
   items remain preserved, while the empty-output fixture emits no item or
   `Done` before the typed provider failure.
5. Confirm the duplicate wording matches both direct mapper behavior and the
   provider-stream duplicate-terminal fixture; do not infer universal
   post-terminal idempotence.
6. Verify `180759f` is evidence-scanner-only, its false-positive exception is
   exact-rule-identifier scoped, and the negative private-content sentinel still
   fails as intended.
7. Reproduce the five SHA-256 hashes, source/tree binding, 360-path inventory,
   gate counts, 60/60 distribution, zero policy violations, 219/0 redaction
   result, and zero-error attestation.
8. Confirm strict formatting and Clippy evidence used the primary repository
   target and did not rely on an OS temporary or worktree-local build directory.

No broader P4 re-review or P3 reopening is requested unless one of these checks
identifies a new production seam.
