# P3 whole-phase Gate D review — canonical ordered transcript

**Date:** 2026-07-18
**Reviewer:** Sable Nightwick (standing P3/P4 review coordinator)
**Handoff:** [`2026-07-18-p3-final-gate-d-handoff.md`](2026-07-18-p3-final-gate-d-handoff.md)
**Phase base:** `a90b730` · **Frozen source:** `7f47218` (tree `b8b042f6`)
**Exact range:** `a90b730..7f47218`

## Verdict

**P3 — READY.** The canonical ordered Responses transcript outcome is sound,
honestly described, and free of any P3-owned or newly-unowned implementation
defect. All strict lint/LOC/module rules hold with no bypasses.

This is a **P3-only** verdict. No P4 verdict is issued here; P4 remains open and
may now proceed to its own review, per the handoff.

No MAJOR or MINOR finding arose from any seat or from coordinator verification.
Every finding is an OBSERVATION — each either a carried, disclosed residual or a
newly-noted, confirmed-benign posture. They are catalogued below and none blocks
P3.

## Method — four disjoint responsibilities (as the handoff prescribes)

- **Responses-protocol seat (Opus):** canonical item model, exact JSON, order,
  phase/IDs, normalization boundary, opaque forms, actionability.
- **Session/persistence seat (Opus):** strict storage, migration/rejection,
  reload, `store:false` replay, spawn/fork ownership, audio links, severed
  provider anchor.
- **Adversarial evidence-provenance seat (Fable):** raw-fixture-to-behavior
  chain, harness integrity, distribution honesty, negative-space claims.
- **Coordinator (me):** independent hash reproduction + full source-bound
  battery + adjudication of every finding, re-verifying each seat claim I adopt
  (evidence-claims law).

Model note: the adversarial seat ran on Fable per the handoff. The two area seats
ran on Opus to conserve Fable spend; each returned SOUND with detailed file:line
traces I cross-checked.

## Scoping fact (established and relied upon)

`7f47218` assembles three foundations I independently reviewed and accepted —
corrected D2 strict session-store (`2c0350d..e9755fe`, unconditional READY),
response-audio M-1/F-2 correction (READY), and the D11 optional-shape inventory
(READY) — plus final packaging. **Between the last accepted foundation
(`56fd4dd`) and `7f47218`, zero production logic changed:** every one of the ten
touched production files has a byte-identical production prefix at both commits;
the intervening commits are (a) a behavior-preserving test-assertion rewrite that
converts `.expect()`/`panic!` into `assert!` + `let-else`, where I verified all
nine bare-`return` sites are assert-guarded (no silent-pass introduced), (b) one
`#[allow]` removal on a test module, and (c) the evidence harness. The two Opus
seats each independently re-confirmed the byte-identical-production claim for
their files. The panel therefore focused fresh energy on assembled cross-cutting
behavior and evidence provenance, not on re-litigating accepted pieces.

## Coordinator machine-evidence reproduction (all byte-exact)

| Item | Handoff value | Reproduced |
|---|---|---|
| Final gate / policy / distributions / redaction / attestation SHA-256 | 5 hashes | all 5 match |
| Attestation binding | 4 sibling hashes + source + tree, `passed:true`, `errors:[]` | all match; source/tree correct |
| 350-path NUL diff inventory | `5532d614…0a85aa4` | match |
| Policy | 298 changed / 78 test-only; zero over-500, thin-entrypoint, module-shape, and added-line (unwrap/expect/panic/allow/ignore/todo/unimplemented) | all reproduced |
| Redaction | 0 findings; current fixtures/generated artifacts path-neutral; 352 historical disclosures retained by §12 policy | reproduced; my own scan of all changed files found zero current leaks |
| Retrospective Gate A base §18 | timing exception only, waives nothing | honest |

Full source-bound battery at a clean detached `7f47218` (primary `target/`):

| Leg | Gate | My rerun |
|---|---|---|
| `fmt` / `clippy` (workspace, all-targets, all-features, `-D warnings`) | pass | pass |
| Workspace all-targets all-features tests | 5,364 | 5,364 |
| All-feature doctests | 8 | 8 |
| Distributions (3 concurrency/fork cases × 20) | 60/60 | 60/60 |
| Isolated redaction sentinels | 23/23 | 23/23 |
| Exact-range `git diff --check` | pass | pass |

## Load-bearing P3 claims verified by the coordinator directly

- **Exact provider JSON, verbatim, empty normalization allowlist.**
  `Serialize for ResponseItem` emits `self.raw()` unchanged
  (`response_item.rs:428-435`); the canonical replay path appends
  `transcript_item.item.raw().clone()` in provider order with no field-stripping
  step (`request.rs:370-378`). Stream coordinates live only in the separate
  `ResponseStreamProvenance` and are never part of `item.raw()`, so there is
  nothing provider-owned to strip. `canonical_persistence_tests.rs:135` asserts
  persisted→reloaded items match at the **byte** level (`item_bytes ==
  expected_bytes`), not merely as parsed values.
- **Migration preserves provider items.** Migration re-encodes at the
  `SessionEvent`/`Value` level (`stage.rs:64-73`); the D11 lifecycle tests assert
  full-vector equality across persist→reload→`store:false` replay. No
  byte-identity overclaim — the guarantee is exact-Value throughout, with the
  additional byte-level assertion above on the canonical path.
- **Severed provider anchor.** `latest_response_anchor` resets to `None` at a
  `ProviderEpochBoundary` (`conversation_state.rs:51`); migrated sessions keep
  genuine historical IDs as recorded input but derive `previous_response_id =
  None`, manufacturing no continuity.
- **Attestation re-reads artifact bytes from disk** (`read_bytes`,
  `run_p3_p4_final_evidence.py:320-321`) rather than trusting prior in-memory
  output; the correct trust anchor for its validate-not-execute nature is the
  coordinator battery, which was exercised.

## Seat results

- **Responses-protocol (Opus): SOUND ×5.** 28-item union pinned with an explicit
  authoritative validator each; four independent discriminator lists (parse
  dispatch, contract manifest, validator table, minimal fixtures) agree on the
  same 28, so a 29th cannot slip through as success. Unknown/unsupported items
  retain raw as Opaque and fail loudly (`Err`) before any assistant/tool turn is
  persisted (`execute.rs:117-131`, `provider_call.rs:69`, verified end-to-end by
  `unsupported_response_loop_tests.rs`). Phase/refusal/annotations/hosted-search/
  compaction/encrypted-reasoning/unknown-parts all preserved with
  Absent/Null/Value distinction. Actionability failure-only enforcement holds for
  the 6 executable forms + 12 nested variants + null-shell carrier.
- **Session/persistence (Opus): SOUND ×5, READY.** Migration never mutates the
  source and publishes with `RENAME_NOREPLACE` + dev/ino recheck after
  classify→backup→stage→re-digest→validate; fail-closed to `InspectOnly`/
  `UnrepresentableSource` on malformed input. Top-level fork eagerly copies every
  referenced sidecar with double byte-verification; persistent spawn shares root
  artifacts; all under `lock_recovered_index` with generation/ABA revalidation.
  Audio links are sealed-before-linked, parent-ordered, and fail typed on missing
  sidecar via `InvalidResponseAudioReference(#[from])`.
- **Adversarial evidence-provenance (Fable): READY.** Three strongest attacks
  failed: (1) typed-variant field-drop refuted structurally — unknown fields
  inside known variants survive because serialization is verbatim `raw`, with
  full-array + byte-equality assertions across assembly/persist/reload/replay/
  spawn/both forks; (2) distribution vacuous-pass refuted — `--exact`,
  `observed == 1` required, all 60 records re-validated, tests assert genuine
  convergence/ownership; (3) gamed-attestation refuted for the committed tooling
  — single `final` action, clean-worktree bootstrap, byte-pinned scripts,
  `attest` re-derives every document from on-disk bytes.

## Observation ledger (all non-blocking)

Carried from the accepted foundations and re-confirmed benign: D2 index-lock
O(n²) appends; `TornTail` no-repair-verb; lookalike-posture asymmetry;
logged-not-silent counter repair; retained lock files; cutover guard is
embedder-authority not type-enforced (`SessionManager::new` custom-store door vs
guarded `standard()`); migrated IDs retained but anchor severed. Audio: orphan
unlinked sidecars until session deletion; semantically-identical terminal-row
rewrites survive the corruption digest (benign by definition — the integrity
hash chain binds all protected fields, so any escape preserves every data field);
current-thread small sidecar writes when `block_in_place` unavailable;
best-effort-but-loud abnormal-stop `partial_output`; transcript-delta
plaintext/Base64 asymmetry proven end-to-end. D11: 274 count is tool-union
dominated; cosmetic fixture ID reuse.

Newly noted this review, all CONFIRMED benign:

1. Attestation validates but does not re-execute (inherent self-attestation
   TOCTOU); mitigation is coordinator independent reproduction, which was done.
2. Redaction fixture inventory is heuristic (scans only test-tagged changed
   files); honestly scoped in the handoff, and both the adversarial seat and I
   independently swept all changed crate files for path/secret leaks with zero
   hits.
3. `canonical_payload_items` replay comparison is a public-item projection, but
   the D11 whole-array input-equality tests close the gap a synthesized extra
   item would open.
4. Known-discriminator enum pinning turns a novel enum value on a known
   discriminator into a typed `ResponseParseError` (fail-closed, never silent);
   an availability trade under the documented pinned-contract/empty-allowlist
   boundary, symmetric over `raw`.
5. Protocol lenience notes: `finish()` tolerates a wrapperless terminal frame but
   still requires output (fails closed); `response.failed` skips actionability
   enforcement but the turn fails regardless (cosmetic error-classification only);
   retained items reach the live event stream before the terminal `Err` but are
   never persisted.

## Standing / still open (not part of this P3 verdict)

P3 establishes the canonical-transcript foundation; it does not close `STATE-01`
or `EVT-01..EVT-07` (P4 owns those). Remaining before P3/P4 acceptance: the P4
whole-phase review, P1/P2 acceptance, proposed D15 (credentialed real-wire
fixture, an open P4 item), and the final phase-acceptance verdicts. The
credentialed real-wire fixture was not run; deterministic public/Codex fixtures
cover P3, as disclosed.
