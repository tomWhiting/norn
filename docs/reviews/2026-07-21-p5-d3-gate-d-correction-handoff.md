# P5 D3 Gate D correction handoff

Date: 2026-07-21

Candidate branch: `codex/p5-d3-correction`

Requested narrow verdict after the owner approved and the candidate implemented
the stronger H1 disposition:
`R1 CLOSED; R2 CLOSED; H1 CLOSED; D3 READY as an implementation candidate`

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

This handoff supersedes the original candidate. Banner notes were added to the
original handoff and verdict; their substantive bodies, retained evidence, and
verdict remain unchanged as historical records.

## Exact correction targets

The owner-approved H1 product candidate is
`af8e7979c4dc4ac1543ab5e3c735b8e8f65e8a3b`. The source-bound evidence
candidate is `467041bfa565011babca9a85aa070739ab824aa5`, tree
`224022a8f0c475a184c0924f7fd4e25c64ccca23`, over corrected structural base
`61c7a528ee9468c0a9ae3698f8dd55cee262e1a2`.

R1, R2, H2, and H3 remain closed by the reconstructed history and `acfcb69`.
Commit `af8e797` adds the whole-group V1 commitment and its compatibility,
fork, strict-reader, offline-validation, and retry evidence. Commit `467041b`
changes only the source-bound evidence runner so it can attest those sentinels
and its own committed bytes. Commit `2b9e77a` retains the resulting evidence
JSON separately; it is not part of the source under test.

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
| H1 whole-group commitment | `af8e797` | Owner-approved product implementation and focused tests |
| H1 evidence runner | `467041b` | Source-bound runner adds 15 H1 and compatibility sentinels |

The split was reconstructed rather than waived or hidden by squash. At
`61c7a52`, both the library and test targets compile without any symbol from the
following commit. In addition to the all-target check, `cargo +1.94.0 test
--locked -p norn --lib --no-run` completes at that exact commit using the
repository target directory. At `97f63a5`, the tree is byte-identical to the
reviewed D3 feature tree.

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

The corrected split passed
`cargo check --locked -p norn --all-targets --all-features`. The feature commit
passed the same matrix and has the exact reviewed feature tree. No
per-commit-bisectability waiver is requested.

## H1 closure

The owner did not accept the weaker construction-only disposition. New
response publications now use
`ProviderEpochBoundaryReason::ResponseStatePublicationV1`, whose
`ResponsePublicationCommitment` stores the total group event count and a
lowercase SHA-256 digest. The domain-separated digest covers a canonical,
commitment-free projection of the boundary followed by every ordered suffix
event. It sorts JSON object keys, length-frames values, preserves array order,
and normalizes signed zero. A fixed direct-group vector pins the representation.

The production publisher seals the complete direct or response-audio group
before persistence. The response-publication append path, `EventStore` batch
append, and the low-level retry planner reject an uncommitted legacy
publication or an invalid V1 commitment before writing. Full provider-state
validation, strict session reading, and the offline strict validator
independently reverify committed groups, so a post-write suffix mutation also
fails closed on replay or inspection.

The retry behavior is bound at the actual durable seam:

- an exact V1 group can complete its exact fsynced prefix;
- the same durable boundary plus a changed unwritten suffix fails its unchanged
  commitment before mutation;
- recomputing the commitment changes the serialized boundary, so retry-prefix
  comparison rejects it; and
- a durable legacy prefix cannot be completed as a new publication.

Compatibility is deliberately asymmetric and does not change the strict
session format version. Complete legacy D3 groups remain readable. Incomplete
legacy groups still fail closed, and new writes cannot mint or extend them.
Pre-D3 readers reject the associated V1 boundary reason rather than
misinterpreting it. Unknown future response-publication reasons also fail
closed. Persistent fork seeding copies a complete group as one batch and
deliberately enriches a copied legacy boundary to V1 while preserving the
boundary identity, provenance, optional audio link, assistant payload, and
ordering.

`ProviderEpochBoundaryReason` is public. Adding the associated V1 variant is an
intentional Rust source-compatibility break for exhaustive downstream matches,
including Meridian: embedders must handle the new committed form rather than
silently treating it as the legacy unit variant. The serialized compatibility
and old-reader failure behavior are pinned separately.

This is an integrity commitment, not authentication. An actor able to rewrite
the complete stored group can recompute it. It also does not make a multi-row
append physically atomic or reconstruct a missing suffix after a crash; the
existing exact-prefix retry and fail-closed reopen rules still define those
cases.

## Hardening dispositions

The original review made H1-H4 owner-rulable because their reported product
exploits were blocked by current caller construction.

- **H1, implemented:** every new response-publication boundary commits the
  total canonical group length and digest. Exact-prefix retry may finish only
  the committed group; changed or recomputed suffixes fail before writes.
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
the source-bound run. H1 is covered by the retained canonicalization, tamper,
retry-prefix, strict/offline-validation, and fork/reader sentinels listed below.

## Retained correction evidence

| Artifact | SHA-256 | Result |
| --- | --- | --- |
| [`D3 runner`](evidence/p5-d3/run_p5_d3_evidence.py) | `6438c4978b95f64afdc902f6a9861b8a6f8afcf81c4bd23d8771b06d87ff1ae7` | Binds branch, base, source, tree, inventory, recursive Rust manifest, and its own committed bytes; repository `target` only |
| [`H1 commitment record`](evidence/p5-d3/2026-07-21-p5-d3-h1-commitment-evidence.json) | `a5dd19f7759bcf2f7ef93de89d68a6e270bb3a17298136d36dc489823ded9af3` | `64/64`, zero failures: `20 + 20 + 24` |

The JSON binds:

- base `61c7a528ee9468c0a9ae3698f8dd55cee262e1a2`;
- source `467041bfa565011babca9a85aa070739ab824aa5`;
- tree `224022a8f0c475a184c0924f7fd4e25c64ccca23`;
- 129-path NUL inventory, SHA-256
  `179ad01f37794f47220bcefd2d3d5e4263245f576782a4b401c208b263c17ec3`;
- 996-record recursive Rust manifest, SHA-256
  `c4076a0c879c6aaa6718784acf56d129bd5cfed5abd0b43933943677f5622388`;
- 20/20 independent-handle contention runs;
- 20/20 synchronized independent-process publication runs; and
- 24/24 exact H1 canonicalization, direct/audio tamper, replay, legacy,
  retry-prefix, strict/offline validation, fork enrichment, old-reader, R1,
  H2, and H3 sentinels.

Every invocation uses `--exact --test-threads=1`, observes exactly one test,
and retains exit status, duration, test counts, output byte count, and output
hash. The artifact keeps `d3_accepted:false` and `p5_accepted:false`.

The original 2026-07-20 record, the 49-observation correction record, the
original handoff, and review verdict are unchanged historical artifacts. The
H1 evidence is additive; it does not rewrite or retroactively broaden them.

## Coordinator verification

All commands use the repository `target` directory; no temporary build target
was used.

- Source-bound H1 retained runner: 64/64 (`20 + 20 + 24`).
- Evidence provenance, runner bytes, inventory, and recursive Rust manifest:
  pass at exact source `467041b` and tree `224022a`.
- `cargo fmt --all -- --check`: pass.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D
  warnings`: pass with no suppression.
- `cargo test --locked --workspace --all-targets --all-features
  --no-fail-fast --quiet`: pass, including Norn 4,213/4,213, CLI 518/518, and
  TUI 683/683 primary harnesses.
- `cargo test --locked --workspace --doc --quiet`: 8/8 doctests.
- `git diff --check`: pass.

The final gate chronology is retained rather than collapsed into the last green
sample. The first repository-local rebuild exhausted the 101 GiB `target`
directory while linking. With owner approval, `cargo clean` removed 119.3 GiB
from that repository target only; no temporary target was introduced. A
post-clean sandboxed sample then failed only where the sandbox prohibited
loopback listener creation. The first unrestricted sample passed every target
except the pre-existing process-manager test
`model_output_is_incremental_and_unknown_id_is_none`: under full-suite pressure
its fixed 100 ms sleep observed empty output. The exact all-feature test then
passed 20/20 in isolation, and the complete unrestricted workspace command
above passed on the next sample. None of the failed samples is reported as a
pass, and no product or fixture change was made to obtain the green rerun.

Across the H1 product range, the 1,071 added Rust lines contain zero added
`allow`/`expect`/`deny`/`warn`/`ignore` attributes, `unwrap`/`expect` shortcuts,
`panic!`/`todo!`/`unimplemented!`, `include!`, or empty `cfg(any())` bypasses.
No new file exceeds 500 lines and no touched file newly crosses 500. Three
already-oversized files grow slightly and remain disclosed: TUI schema render
`884 -> 885`, session events `1,003 -> 1,008`, and legacy integration tests
`1,769 -> 1,772`. The largest new H1 test module is 457 lines.

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
- H1 provides durable whole-group retry integrity, not storage authentication,
  physical multi-row atomicity, or reconstruction of a missing suffix.
- Complete legacy D3 groups remain readable but cannot be newly appended or
  used to complete a durable legacy prefix. Persistent fork copying enriches
  their boundary metadata to V1 in the child.
- The public associated enum variant is a disclosed source break for exhaustive
  Meridian/embedder matches; no binary/source compatibility beyond the pinned
  serialized and fail-closed reader behavior is claimed.
- Registered append still holds the global index lock across timeline scan and
  fsync; the earlier scale/quadratic-rescan observation remains open for later
  persistence work.

## Same-reviewer checklist

1. Confirm R1 propagates malformed reserved audio links typed while identity
   and unrelated custom data retain their distinct semantics.
2. Confirm `61c7a52` is independently buildable and `97f63a5` contains the
   durability behavior atomically with the exact reviewed feature tree.
3. Confirm H1 binds event count and every ordered group row, rejects both
   unchanged-commitment and recomputed-commitment divergent retries before
   writes, and is reverified by replay, strict reading, and offline validation.
4. Confirm complete legacy read, legacy-prefix refusal, V1 fork enrichment,
   pre-D3/unknown-reader fail-closed behavior, and the disclosed public-enum
   source break.
5. Confirm H2/H3 regressions bind monotonic closure, negative-provenance
   fallback behavior, and both target-ID shapes, and that H4's lock wording
   still matches the implementation.
6. Return `R1 CLOSED; R2 CLOSED; H1 CLOSED; D3 READY as an implementation
   candidate`; leave D8, D7/P9, and whole-P5 acceptance untouched.
