# P5 D3 Gate D correction handoff

Date: 2026-07-21

Candidate branch: `codex/p5-d3-correction`

Requested narrow verdict: `R1 CLOSED; R2 CLOSED; D3 READY as an
implementation candidate`

Whole-P5 acceptance: explicitly out of scope

## Why this correction exists

Original Gate D review
[`7155196`](2026-07-21-p5-d3-gate-d-review.md) found the running D3 product
sound but returned `NOT READY` on two categorical repository requirements:

1. R1: non-identity fork filtering swallowed malformed reserved
   `response.audio.artifact` parse errors.
2. R2: the claimed mechanical split `b75e64b` neither built independently nor
   kept tracker-free durable-mark behavior in the feature commit that defined
   it.

This handoff supersedes the original candidate. The original handoff, evidence,
and verdict remain unchanged as historical records.

## Exact correction targets

The source-bound correction candidate is
`ef3cbbbfc1eec0c279dc848bcf155dddb5dd5725`, tree
`b5a692fd3c70ae2c043ed3521d5f06afa3d20757`, over corrected structural base
`61c7a528ee9468c0a9ae3698f8dd55cee262e1a2`.

Product corrections are in `acfcb69`. Commit `ef3cbbb` adds only the updated
source-bound evidence runner so the runner can attest its own committed bytes.
The evidence JSON is retained separately at `828fc81` and is not part of the
source under test.

### Reconstructed history

| Historical commit | Corrected commit | Verification |
| --- | --- | --- |
| `b75e64b` mechanical split | `61c7a52` | Independently builds across Norn targets/features; pre-D3 tracker-free behavior remains in the split |
| `5358bea` D3 feature | `97f63a5` | Exact tree `78702204b2130b67a71e19b08ad95f9101f82d6f`; helper, callers, behavior docs, and tracker-free tests land atomically |
| `96888c5` evidence runner | `9ab0100` | Patch replayed over corrected parent |
| `95dedf3` Rust-manifest fix | `2b6870b` | Patch replayed over corrected parent |
| `ef3b9c7` compatibility correction | `201f4b5` | Exact tree `975d6fd66aaeab3964c50e19d416207425680012` |
| `31553e8` driven headless correction | `362e13d` | Patch-identical replay; separate reviewed `READY` slice |
| `2ef1427` OAuth framing test fix | `e2a7d67` | Patch-identical replay |
| `beeb1f5` historical handoff | `b0b6796` | Historical content retained |
| `7155196` historical review | `8a90c68` | Historical `NOT READY` verdict retained |

The split was reconstructed rather than waived or hidden by squash. At
`61c7a52`, both the library and test targets compile without any symbol from the
following commit. At `97f63a5`, the tree is byte-identical to the reviewed D3
feature tree.

## R1 closure

`ContextFilter::apply` now returns
`Result<Vec<SessionEvent>, ContextFilterError>`. Identity filtering remains an
exact `Ok(events.to_vec())`. Both response-audio association scans use
`ResponseAudioArtifactLink::from_event(event)?`; only `Ok(None)` skips an
unrelated event.

The public typed error wraps `ResponseAudioReferenceError` transparently.
Regressions prove:

- a malformed reserved audio row fails as
  `ContextFilterError::ResponseAudio(ResponseAudioReferenceError::InvalidArtifactLink)`
  under a non-identity filter;
- identity filtering preserves the same malformed audit row byte-for-byte; and
- audio-shaped data under an unrelated custom discriminator remains opaque and
  retained.

The regressions live in the real sibling module
`agent/fork_context_filter_error_tests.rs`; no `include!` split or lint bypass
was introduced. A coordinator mutation restored both original swallowed-error
branches and the exact malformed-row test failed; restoring propagation made it
pass.

## R2 closure

The original split moved two `with_prompt_context_edits` calls before the
helper existed and silently changed the no-tracker behavior. The corrected
split instead retains its previous local `ContextEdits::new()` fallback. The
following feature commit atomically introduces:

- `with_prompt_context_edits`;
- both callers;
- the tracker-free durable-mark documentation; and
- both tracker-free suppression/presence regressions.

The corrected split passed `cargo check -p norn --all-targets --all-features`.
The feature commit passed the same matrix and has the exact reviewed feature
tree. No per-commit-bisectability waiver is requested.

## Hardening dispositions

The original review made H1-H4 owner-rulable because their reported product
exploits were blocked by current caller construction.

- **H1, scoped by construction:** Norn-managed publication mints one random
  boundary and can resubmit only the same group. Already-durable divergence is
  rejected and an orphan prefix fails closed. The low-level public
  `JsonlSink` is not advertised as a general guarantee against a caller
  resubmitting a different not-yet-durable suffix under the same boundary. Any
  future divergent-resubmission surface must first add and validate a durable
  canonical group length/digest. This correction deliberately does not expand
  the session format for an unexposed managed-runner behavior.
- **H2, implemented:** `FilteredFork` closure is monotonic across later cuts.
  `NotStored` clears only legacy fallback candidates and preserves an older
  proven anchor. All three properties have direct regressions.
- **H3, implemented:** direct and response-audio publication shapes require
  `target_base.id == target`. Mismatched positional assistants fail local frame
  recognition, remain visible rather than hiding provenance in the prompt
  projection, and fail full provenance validation. Both shapes are tested.
- **H4, claim corrected:** the registered lock covers strict read,
  retry-prefix validation, provenance validation, writes, and fsync.
  Cadence/index counters update after release; later writers rederive this
  derived cache from durable timeline state under lock. No counter-update lock
  claim remains in the live plan or correction record.

Coordinator mutation checks removed the H2 closure scan, the `NotStored`
fallback clear, and the H3 target-ID check separately. Their exact sentinels
failed in every case; all guards were restored before the committed source and
the source-bound run.

## Retained correction evidence

| Artifact | SHA-256 | Result |
| --- | --- | --- |
| [`D3 runner`](evidence/p5-d3/run_p5_d3_evidence.py) | `de7be62051db25c5f983993d9fb644e353579b4cfa6f89978a2bc36fde03a9dd` | Binds branch, base, source, tree, inventory, recursive Rust manifest, and its own committed bytes; repository `target` only |
| [`49-observation record`](evidence/p5-d3/2026-07-21-p5-d3-correction-evidence.json) | `553b657651fa2ad44fc947263907fd75039f340940dbbbd3b606634ace149b0a` | `49/49`, zero failures |

The JSON binds:

- base `61c7a528ee9468c0a9ae3698f8dd55cee262e1a2`;
- source `ef3cbbbfc1eec0c279dc848bcf155dddb5dd5725`;
- tree `b5a692fd3c70ae2c043ed3521d5f06afa3d20757`;
- 111-path NUL inventory, SHA-256
  `1dabbae20a661c2a0b0c3cd2a3093ee3c92f395aafcc43f0d0a9a696546f1b6c`;
- 994-record recursive Rust manifest, SHA-256
  `629c089f35987579776bd915ebc6d0eafe8d103fab0e68cbd92414024da19d2f`;
- 20/20 independent-handle contention runs;
- 20/20 synchronized independent-process publication runs; and
- 9/9 exact fork, durability, R1, H2, and H3 sentinels.

Every invocation uses `--exact --test-threads=1`, observes exactly one test,
and retains exit status, duration, test counts, output byte count, and output
hash. The artifact keeps `d3_accepted:false` and `p5_accepted:false`.

## Coordinator verification

All commands use the repository `target` directory; no temporary build target
was used.

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `git diff --check`: pass.
- `cargo test --workspace --all-targets --all-features --no-fail-fast --quiet`:
  pass, including Norn 4,199/4,199, CLI 518/518, and TUI 683/683.
- `cargo test --workspace --doc --quiet`: pass, 8/8.
- Corrected retained runner: 49/49.
- All new source and test files remain under 500 lines; no lint allowance,
  `unwrap`, `expect`, `panic`, ignored test, or disabled-code bypass was added.

The final strict Clippy and doctest statements are recorded only after fresh
execution on this correction branch; the historical 4,192-test Norn count is
not carried forward.

## Out-of-slice observation

The pre-existing non-driven `stream-json` branch in
`norn-cli/src/print/orchestrator.rs` can let a renderer panic override the
primary run error's exit class. It predates and is outside both reviewed
headless slices. It remains explicit follow-up work and does not change the
driven-transport `READY` verdict or the D3 correction.

## Honest boundaries

- This requests D3 implementation-candidate confirmation, not D3 acceptance or
  whole-P5 acceptance.
- D8 role authority, broad volatile-context and concurrent-agent matrices,
  WebSocket state transport, P2 acceptance, and whole-P5 gates remain open.
- The authenticated public/Codex real-wire fixture remains mandatory at D7/P9.
- H1's low-level embedder boundary is explicit; this handoff does not claim a
  durable whole-group commitment that the format does not contain.
- Registered append still holds the global index lock across timeline scan and
  fsync; the earlier scale/quadratic-rescan observation remains open for later
  persistence work.

## Same-reviewer checklist

1. Confirm R1 propagates malformed reserved audio links typed while identity
   and unrelated custom data retain their distinct semantics.
2. Confirm `61c7a52` is independently buildable and `97f63a5` contains the
   durability behavior atomically with the exact reviewed feature tree.
3. Confirm H2/H3 regressions bind monotonic closure, negative-provenance
   fallback behavior, and both target-ID shapes.
4. Confirm H1 is scoped without a false whole-group commitment claim and H4's
   lock wording now matches the implementation.
5. Return the narrow verdict requested above; leave D8, D7/P9, and whole-P5
   acceptance untouched.
