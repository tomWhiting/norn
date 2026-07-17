# P3/P4 response-audio lifecycle closure handoff

**Status:** Review candidate. The six named response-audio lifecycle gaps are
implemented and source-bound evidence is retained. This handoff does not accept
the frozen response-audio implementation, P3, or P4.

**Frozen response-audio production source:**
`460c192b5160fcabfa647418a75ecf29665f6743..0512953e650c4961e790f5987896c131e82ba4f3`;
tree `1aeac724119bb525340cf7cef67dbac906131ac0`; evidence/package commit
`192756edc3c726dd99150cecc69b8645fe4f604c`.

**Accepted D2 prerequisite:** corrected source range `2c0350d..e9755fe`;
unconditional Gate D `READY` recorded at `26b4e28`.

**Lifecycle fixture base:**
`96d5f0e9346a9a6c9e3f5b859ea7ee634ed1fbe0`.

**Lifecycle fixture source:**
`f252cbb17f1dc909eb1af060c2407c6aacf49dd1`; tree
`3ec9515813ab5e95adac4e6412af83765b5aed4b`.

**Lifecycle fixture range:** `96d5f0e..f252cbb`.

**Owning records:** `docs/DECISIONS-2026-07.md` section 16 and the P3/P4
checklists in `docs/RESPONSES-API-REMEDIATION-PLAN.md`.

## 1. Review boundary

The lifecycle range changes tests only. It adds no production behavior and does
not alter the accepted D2 store, migration, codec, or publication implementation.
The reviewer should examine two separate boundaries:

1. Review the frozen production implementation and its original evidence through
   [`2026-07-17-p3-p4-response-audio-handoff.md`](2026-07-17-p3-p4-response-audio-handoff.md).
2. Review `96d5f0e..f252cbb` as the successor fixture delta that closes the six
   explicitly enumerated gaps from section 7 of that handoff.

The lifecycle delta touches six Rust paths:

- `loop/mod.rs` adds only a `#[cfg(test)]` module declaration;
- `loop/response_audio_lifecycle_loop_tests.rs` is a new 374-line test module;
- `provider/response_audio.rs` changes only its existing test module;
- `provider/openai/response_reconciler/tests/audio.rs` is test-only;
- `session/manager/tests/fork_audio.rs` is test-only; and
- `tools/agent/fork_tool.rs` changes only its existing test module.

The existing over-limit production prefix in `fork_tool.rs` gains no production
line. The new file is below 500 lines. The staged-source scan found no added
lint suppression, `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!`.

## 2. Six closed cases

The six semantic cases use seven exact test invocations because malformed-delta
symmetry is pinned independently at the direct parser and reconciler layers.

| Case | Production seam exercised | Assertion boundary | Retained runs |
|---|---|---|---:|
| Post-LLM hard cut after seal | Real `run_step`, response-audio writer, hanging `PostLlm` hook, step timeout, store resume/read | Exactly one readable sealed partial reference remains; no link or assistant event is fabricated | 20/20 |
| Absent response ID | Real completed step, sidecar seal/link/assistant publication, manager resume, linked read | `None` remains absent at sidecar, link, assistant, reload, and read boundaries | 1/1 |
| One final terminal row | Real completed step and raw private JSONL artifact | Exactly one terminal record exists, it is last, and the file ends with a newline | 1/1 |
| Multi-artifact ownership fork | Real `SessionManager::fork`, two sealed artifacts, source deletion, destination resume/read | Two distinct references and payloads are copied and resolve after the source is unavailable | 20/20 |
| Real `ForkTool` inheritance | Real `ForkTool::execute`, persistent parent, seeded link/assistant, child resume/read | The child resolves inherited audio under the accepted root-session and generation authority | 20/20 |
| Malformed delta symmetry | Direct `ResponseAudioEvent` parser and `ResponseReconciler` | Missing and non-string media/transcript deltas return their exact typed errors; invalid Base64 remains an audio-only typed error | 1/1 at each layer |

These are regression tests for existing production contracts. The implementation
work found no additional production defect in these six gaps.

## 3. Retained evidence

The new runner is separate from the immutable 2026-07-17 response-audio gate
and distribution runners. It refuses dirty Rust input, binds the source commit,
tree, complete Rust manifest, runner, exact command, source file, observation,
and output hashes, and requires at least 20 runs for each timing-, concurrency-,
or publication-sensitive case. It honors `CARGO_TARGET_DIR`; this run used the
normal main-repository target rather than a temporary or successor-worktree
target.

| Artifact | SHA-256 |
|---|---|
| [`run_response_audio_lifecycle_distributions.sh`](evidence/p3-p4-audio/run_response_audio_lifecycle_distributions.sh) | `398b6098930ceb0c6ac3c0402b58cdb314a02303b7b56fd1a200d2c30269927e` |
| [`2026-07-17-response-audio-lifecycle-distributions-f252cbb.json`](evidence/p3-p4-audio/2026-07-17-response-audio-lifecycle-distributions-f252cbb.json) | `4f1d5f81ecceb608972b8a803e193c2f3f3d23b1cb3ddf9462dba76dcab41798` |

The evidence filename uses the runner's UTC date; this handoff uses the local
Australia/Melbourne date.

The retained result is 64/64: four deterministic invocations at 1/1 and three
sensitive invocations at 20/20. It reports six semantic cases, seven exact test
invocations, source tree `3ec9515813ab5e95adac4e6412af83765b5aed4b`, and
Rust-manifest hash
`95be803c9122593dfafccc8c03f92abd0de2951b640fad9538419e0f2ffd2bb6`.

Before packaging, the candidate also produced these local observations in the
normal repository target. They are disclosed for reviewer orientation, not
promoted into final P3/P4 Gate C evidence:

- strict workspace/all-target Clippy passed with `-D warnings`;
- `cargo fmt --all -- --check` and `git diff --check` passed;
- the focused `response_audio` library filter passed 39/39; and
- the complete `norn` library suite passed 3,988/3,988 outside the managed
  network sandbox.

The first complete-library attempt inside the managed network sandbox is not
evidence: 3,877 tests passed and 111 loopback-binding tests failed with `EPERM`.
The identical command then passed 3,988/3,988 outside that sandbox. No network
result or discarded output hash is presented as retained Gate C evidence.

## 4. Deliberately open work

This package closes only the six named lifecycle gaps. It deliberately leaves
all of the following open:

- focused independent review of the frozen response-audio implementation and
  this successor fixture range;
- a finite, mechanically enumerable inventory of optional nested shapes and
  lifecycle surfaces; the six cases do not prove an undefined exhaustive set;
- the owner disposition for the retrospectively missing P3/P4 phase bases;
- P1 and P2 acceptance, which P3 depends on, and P3 acceptance, which P4
  depends on;
- retained full-range final P3 evidence and its protocol/persistence review;
- retained full-range final P4 evidence and its separate streaming review; and
- P6-owned absent-versus-zero usage projection and retry-attempt accounting,
  which are not P4 acceptance requirements.

The accepted D2 range and its corrected Gate D verdict remain unchanged. This
handoff makes no claim about request-side audio, playback, export, TUI rendering,
WebSocket transport, or the later scale work for global-index lock occupancy and
quadratic full-timeline rescans.

## 5. Independent review request

The reviewer should:

1. Reproduce both retained artifact hashes and the 64/64 distribution totals.
2. Confirm that `96d5f0e..f252cbb` is test-only and adds no production line or
   policy bypass.
3. Trace each fixture through the real seam named in section 2 and reject any
   assertion that can pass without exercising that seam.
4. Review the frozen production source `460c192..0512953` against its original
   handoff, using these fixtures as additional adversarial evidence rather than
   treating tests as proof by themselves.
5. Attack hard-cut ordering, absent-ID binding, terminal-row uniqueness,
   multi-artifact ownership transfer, real `ForkTool` inheritance, and typed
   malformed-delta symmetry.
6. Return `READY` or `NOT READY` for the focused response-audio implementation
   candidate, and state separately whether the six successor fixtures are
   sufficient for their named claims.

A `READY` verdict may close the focused response-audio slice and its six named
fixture gaps. It must not accept P3 or P4, fill the blank phase-base cells, or
claim the still-undefined optional-shape matrix is exhaustive.
