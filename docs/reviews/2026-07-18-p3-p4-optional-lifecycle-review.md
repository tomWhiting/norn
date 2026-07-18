# P3/P4 optional-shape and lifecycle evidence review

**Date:** 2026-07-18
**Reviewer:** Sable Nightwick (standing P3/P4 review coordinator)
**Handoff:** [`2026-07-18-p3-p4-optional-lifecycle-handoff.md`](2026-07-18-p3-p4-optional-lifecycle-handoff.md)
**Candidate source:** `56fd4dd626af0c66954a51932fc05395f3023622`
(tree `6f0f6dce…`), review range `624540d..56fd4dd`, packaged at `56ea604`
**Owner scope:** DECISIONS-2026-07 §17 (D11) — mechanically enumerable schema
inventory + equivalence-class coverage at exactly ten named lifecycle surfaces;
NOT a `659 × surfaces` Cartesian run.

## Verdict

**READY — for this evidence candidate only. This does not accept P3 or P4.**

The candidate faithfully implements the owner-ruled method in §17. The finite
official-contract enumeration is internally airtight, the success/failure corpus
boundary is clean and test-pinned, the 7×10 applicability matrix's 45 covered
cells prove their class-on-surface claims at full strength while all 25
`not_applicable` cells are honest architectural partitions, and the source-bound
gate reproduces byte-exact. No MAJOR or MINOR defect survived the panel.

Six OBSERVATIONS are recorded below; one (compaction) is for owner disposition,
the rest are precision/robustness notes. None blocks this candidate.

## Panel and method

Per the model-tiering discipline: **2 Opus area seats** (contract artifact +
corpus boundary; applicability matrix + anchor sufficiency) + **1 adversarial
seat** (sibling-defect hunt, runner-gaming, claim-boundary, evidence-integrity)
+ coordinator battery and direct verification. The adversarial seat began on
Fable and was re-run to completion on Opus 4.8 after a Fable credit exhaustion;
its verdict rests on the Opus run. All seats read-only, no cargo. Every seat
finding below was re-verified by the coordinator before adoption (evidence-claims
law).

## Coordinator mechanical verification (all reproduced)

| Check | Result |
|---|---|
| Four artifact SHA-256s (contract `7ea54c50…`, inventory `561f9cc0…`, runner `af001f32…`, gate `848e44f3…`) | all match handoff |
| Contract arithmetic | 274 = 149+111+14; 659 = 149·2+111·3+14·2; per-item aggregates recompute exactly; all 274 property records category/legal-states consistent |
| 28 item names + order | == `PUBLIC_OUTPUT_ITEMS` (response_contract.rs:271-300) |
| Matrix cells | 70 = 45 covered + 25 `not_applicable`; every covered cell has anchors; every N/A has a reason |
| Anchor resolution | all 44 catalog anchors resolve to real `fn` at 56fd4dd; surfaces section = exactly 63 refs → 44 unique tests |
| Range production edits | `loop/mod.rs`, `request.rs`, `response_stream_event.rs` are `#[cfg(test)] mod` decls only; `fork_tool.rs` change entirely inside `mod tests` (hunks at 1802+) |
| `fork_tool.rs` 668-line prefix | identical SHA `7a980918…` at 624540d and 56fd4dd (pre-existing debt, disclosed, unchanged) |
| Gate JSON bindings | head 56fd4dd / tree `6f0f6dce` / rust-manifest `c07dfbfa…` all reproduce; `os_temp_used:false` |
| Full battery at 56fd4dd (clean detached checkout, primary target) | fmt/clippy clean; **norn lib 4,011/4,011**; **40/40 focused singles**; **four 20/20 distributions** |

## Seat findings (coordinator-confirmed)

**Seat A — contract artifact + corpus boundary: SOUND.** Enumeration matches
norn's own validators field-for-field on spot-checks (required-non-nullable
fields the contract correctly *excludes* are confirmed non-nullable by
`known_item_schema.rs`; per-variant computer-action `keys` optionality honored by
`nested.rs:22-43`). Corpus boundary clean: the 6 populated executable forms, 12
nested computer/patch variants, and the required-null shell carrier are all
failure-only (`reconciler_tests.rs:101-174`, filtered out of success matrices at
`fixtures.rs:409/420/443/452`), never smuggled into a success count. Minimal
fixtures carry zero optional keys; 9 of 14 required-nullable fields nulled
directly, the other 5 sit under an absent `action`/empty `tools[]` and are
covered by populated/nested fixtures (disclosed).

**Seat B — applicability matrix + anchor sufficiency: SOUND.** All 25 N/A reasons
are architecturally true (verified against the code, e.g. `SessionEvent`
persists `ResponseTranscriptItem`, never the raw `ResponseStreamEvent` envelope,
so the event-envelope-on-persistence exclusions are honest). Every checked
covered anchor is sufficient at full strength: `opaque_strict_replay` does a
`FsyncPerEvent` persist → full-reload → compares the complete 53-entry
`ResponseTranscriptItem` vector *including provenance* → then `store:false`
replay proving exact provider JSON with coordinates stripped; the persistent
ForkTool anchor proves contiguity of the full inherited vector, exactly-once
opaque occurrence, and the public projection followed only by the fork's own
structured result. The split-causality claim (mapper proves raw retention +
typed classification; loop proves the typed failure cannot publish an ordinary
turn) is gap-free, with the one streaming-adapter seam openly disclosed in the
inventory's `uncertainties`. The three anchors unbound to matrix cells
(`manifest_parser`, `schema_validators`, `canonical_persist_resume`) are
correctly redundant completeness meta-checks.

**Adversarial seat — READY.** The sibling-defect hunt for each of the five
pre-freeze corrections held: no other optional-status enum carries an illegal
witness (output-item witnesses are all validator-guarded via
`every_minimal_public_item_passes_its_authoritative_schema`; the only unguarded
witness surface is stream-event shapes, where both remaining tests are legal); no
surviving permissive `>=`/`contains` count assertion (all exact equality); no
persistence/fork anchor left on `Flush` (all `FsyncPerEvent`); every replay/fork
anchor rejects a duplicated/extra item via full-vector equality. The runner
enforces what it advertises — `verify_contract`/`verify_inventory` SHA-pin the
artifacts before the structural jq re-derivation, `verify_diff_policy` scans only
added lines for the full prohibited set and enforces the unchanged-prefix-hash
rule, and the target guard resolves the primary repo via `git rev-parse
--git-common-dir` and fails closed (exit 2) on a non-repo or OS-temp target.

## Observations (none blocking)

1. **Compaction is a response-item view surface outside the ten (OWNER
   DISPOSITION).** `loop/compaction.rs`, `session/context_edit.rs:142-170`.
   Auto-compaction supersedes events below a cut and folds them into a
   `Compaction { summary }`, so a superseded opaque/nested item is not replayed
   verbatim in the prompt view. **Coordinator grading — OBSERVATION, not a
   gap:** compaction is a *view reroute*, not a store rewrite — `superseded`
   marks shrink the prompt view while the immutable JSONL event is never mutated
   or deleted, so the optional-shape contract's *durable-carriage* losslessness
   is structurally untouched (a compacted item still sits verbatim in the store
   and carries losslessly if the view includes it). §17 also rules exactly ten
   surfaces, so the candidate is faithful to the ruling. Surfaced only so Tom can
   choose to either name compaction explicitly out of scope in the Honest
   Boundaries, or add a compaction-supersession cell if "closure" is meant to be
   exhaustive over view transforms.

2. **274/659 headline is tool-union-dominated (CONFIRMED, disclosed rule).**
   182 of 274 occurrences come from re-enumerating the ~16-variant tool union in
   two item contexts (`tool_search_output`, `additional_tools`). Legitimate under
   the disclosed `contextual_identity = item + contextual_path` rule, but the
   number should not be read as 274 distinct schema surfaces.

3. **Cosmetic id reuse (CONFIRMED, harmless).**
   `output_item_test_fixtures.rs:229-233` reuses a sibling `call_id` string as
   the `local_shell_call_output` `id`; no identity collision in any corpus,
   tests assert only key-set/round-trip.

4. **`production_loc` heuristic is crude (CONFIRMED, not exploited).** The runner
   counts lines before the *first* `#[cfg(test)]`; a file interleaving a test
   block with later production code could hide the tail from the 500-line rule.
   Not triggered — all four changed production files keep production entirely
   before their sole trailing test module.

5. **`os_temp_used:false` is an emitted literal, not a measurement (CONFIRMED).**
   Sound today only because the earlier target guard rejects OS-temp targets; the
   field asserts rather than measures.

6. **Anchor resolution uses substring match (CONFIRMED, self-corrected).**
   `git grep -Fq "fn $test_name"` would match a prefix (`fn foo` ⊂ `fn foobar`),
   but the focused stage's `observed_passed == 1` exactly-one guard fails such a
   name, so a broken anchor cannot silently pass.

## Standing / still open

This closes the D11 evidence-method item only. Independent of this candidate,
P3/P4 acceptance still requires: the retrospective P3/P4 phase-base (Gate A)
disposition, P1 and P2 acceptance, full-range final P3 and P4 gates, and separate
P3-then-P4 acceptance reviews. The narrow M-1/F-2 response-audio correction was
confirmed separately in
[`2026-07-18-p3-p4-response-audio-correction-review.md`](2026-07-18-p3-p4-response-audio-correction-review.md).
