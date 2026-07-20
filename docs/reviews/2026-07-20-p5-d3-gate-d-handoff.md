# P5 D3 conversation-state Gate D handoff

Date: 2026-07-20
Candidate branch: `codex/p5-d3-compaction`
Requested verdict: `READY` or `NOT READY` as a D3 implementation candidate
Whole-P5 acceptance: explicitly out of scope

## Exact review targets

The functional D3 range is
`b75e64b89f7e608b896e8254f49d1bb238296f35..ef3b9c7b0fd12946d5b993457106dda0b34f0edd`,
with source tree `975d6fd66aaeab3964c50e19d416207425680012`.
It contains:

- `5358beaeb17a149b61d4864f5a658e4a96cd81de`, the Rust implementation and
  fixtures;
- `96888c55dbcc5cf4cbfacc225f232f486bc263f9`, the retained evidence runner;
- `95dedf3e6de02a626ac3bbd5d78285375bfca30d`, a runner-only correction that
  makes the Rust manifest enumerate all recursive tree entries; and
- `ef3b9c7b0fd12946d5b993457106dda0b34f0edd`, the compatibility correction
  that preserves post-cut pre-D3 anchors only before a timeline has entered the
  framed-provenance era and restores the local interrupted-result anchor cut.

The retained artifact binds a 103-path NUL inventory with SHA-256
`ad7a34b29dbad7f0b1c0a28e01086b66e5d6ce1ab2f5990b214b4d9803cac971`.
It consists of 102 Rust paths plus the runner. Review product behavior at the
exact source above; later handoff and evidence-JSON commits are documentation
only.

The candidate also requires a separate behavior-preservation verdict on the
mechanical module split
`e3549b4a39e9c7f44b686e051c91be9144d2f48d..b75e64b89f7e608b896e8254f49d1bb238296f35`,
tree `d6d628d27953f6e3ee6d5c48a0584c8ecc058d01`. Its 34-path NUL inventory has
SHA-256 `734bf0230367a53bc0f4cf3b696d737c93e2ba5380ef87d8b6a7740180b42025`.
That commit splits fork, loop-context, and the 7,024-line spawn module without
intentionally changing behavior. `spawn.rs` is now 117 lines,
`spawn/tests/mod.rs` is 386, its 21 topical test children have a 416-line
maximum, and the largest associated production file is 471 lines. Separately,
D3 implementation commit `5358bea` splits the 7,268-line runner-test root to
405 lines with 36 topical children and a final 454-line maximum.

## Outcome in practice

New D3-published provider-owned response state has one durable, auditable framed
representation. A newly successful stored response is published as a complete
group:

1. `ProviderEpochBoundary(ResponseStatePublication)`;
2. the reserved provider-state provenance row;
3. an optional response-audio artifact link; and
4. the assistant event.

Only provenance in a structurally complete local frame is hidden from prompt
projection, and only a frame accepted by full timeline validation has D3
provenance semantics. The separately validated pre-D3 assistant-anchor path is
the sole compatibility exception. An application custom event that happens to
reuse the reserved discriminator remains ordinary data when it is unframed or
sits outside the frame.

Public Responses threading uses `store:true`, a validated
`previous_response_id`, only the post-anchor input delta, current top-level
instructions, and provider server compaction. The current instructions are
resent because the API does not carry top-level instructions through
`previous_response_id`. The stateless Codex-subscription path remains
`store:false` and performs exact local replay, including nonempty encrypted
reasoning material.

Local compaction, suppression, and non-identity filtered forks cut the active
provider epoch. Durable injection does not. A filtered fork gets the distinct
first-class `FilteredFork` boundary: retained history stays audit-visible and is
replayed only when every required provider item has replay material; the source
response ID cannot become the child anchor.

Missing encrypted replay material fails typed before prompt mutation or HTTP
dispatch. A missing or expired public response anchor fails typed after that
single provider request; Norn does not silently retry with weaker reconstructed
history.

## Persistence and validation

`EventStore::append_batch` publishes each response group non-interleaved. A
registered store retains one index-and-timeline lock through the append and
counter update. In-memory visibility follows sink success.

This is an exact-prefix contract, not an atomic-filesystem claim. I/O failure or
process interruption can leave the exact durable prefix. Retrying the identical
group can finish that prefix; a different continuation is rejected. Normal
reopen of an incomplete semantic frame fails closed because there is no
automatic prefix-repair surface in this slice.

Managed create, resume, latest, open-or-resume, and fork validate the complete
provider-state provenance timeline before affinity adoption, prompt mutation,
network dispatch, or child publication. Other session-integrity checks remain
separate: response-audio association validation currently follows affinity
binding but still precedes prompt mutation, dispatch, and child publication.
Persistent fork seeding copies each complete response frame using the same batch
operation rather than independent rows.

## Compatibility

Current readers preserve old sessions and the separately validated legacy
assistant-anchor path. After an ordinary historical migration, adoption,
compaction, or suppression cut, a later unframed pre-D3 response remains
eligible only when that timeline has never contained a valid framed D3
publication. Pre-cut candidates stay excluded; `FilteredFork` always closes
legacy eligibility. Unframed discriminator collisions are never upgraded to
provider state. Older binaries reject the two new enum values during
deserialization rather than accepting new timelines with weaker semantics.
Unframed D3 rows written only by unpublished iterations of this branch have no
migration promise.

## Source-bound evidence

All Cargo work used the repository `target/`; no temporary Cargo target was
used.

| Artifact | SHA-256 | Result |
| --- | --- | --- |
| [`D3 runner`](evidence/p5-d3/run_p5_d3_evidence.py) | `0b7bd6fb4d866b05074f8d47b4d66b268f93df4732e8fb2a1133c5cc0879627f` | Source/tree/branch bound; rejects Rust drift; repository target only. |
| [`42-observation record`](evidence/p5-d3/2026-07-20-p5-d3-evidence.json) | `0b9a229b04776bfc72bb262f5f1af15cf6ed5122a779ea2bdcd958bbb262be44` | `42/42`, zero failures. |

The runner records:

- independent in-process handles publishing complete non-interleaved groups:
  `20/20`;
- four true independent subprocesses released together before resume and then
  publishing the same registered session: `20/20`;
- persistent fork seeding preserving one complete frame: `1/1`;
- a response-event-hook timeout never duplicating durable output as a partial
  response: `1/1`.

The first distribution directly contends at append through independent handles.
The subprocess barrier proves synchronized process-level convergence, but it is
before each process resumes the session and therefore does not claim that all
four have entered `append_batch` simultaneously. Every invocation uses
`--exact --test-threads=1`, must report exactly one
executed test, and retains its exit status, duration, observed counts, and
output hash. The corrected source manifest contains 991 Rust tree records with
SHA-256 `7d1531524ac14205150234d49ed6b7f13215646b76e140650c9e952486f010f4`.

Coordinator gates at post-source packaging commit
`2ef1427c2d4bdfffdbb80e943e1b099a42d7e90b` are green. Its only change after
the headless correction is a test-only OAuth response reader; no D3 production
path differs from the exact source above:

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- `cargo test --workspace --all-targets --all-features --no-fail-fast --quiet`,
  including the Norn `4,192`, CLI `518`, and TUI `683` primary harnesses;
- `cargo test --workspace --doc --quiet`: `8/8` doctests.

Two pre-correction workspace samples each exposed the same OAuth fixture failure:
the canceled partial-request test read until TCP EOF and received macOS
`ConnectionReset` after the complete framed response. The exact test was 20/20
in isolation, so neither failed workspace sample was relabeled as a pass.
Test-only commit `2ef1427` makes the fixture stop at its declared
`Content-Length`; the file remains 485 lines. After that correction, the exact
test was 20/20 and the complete workspace command above passed.

No lint suppression or Clippy bypass was added. No new file exceeds 500 lines
and no touched file newly crossed 500. Six already-oversized files in the D3
range grew slightly and remain disclosed rather than hidden: TUI schema render
`879 -> 884`, inflight compaction `665 -> 671`, OpenAI provider `1006 -> 1013`,
OpenAI request `1455 -> 1459`, session events `995 -> 1003`, and legacy
integration tests `1754 -> 1769`.

## Honest boundaries

- This is a D3 candidate, not D3 acceptance, Gate B/C for all of P5, or
  whole-P5 acceptance.
- D8 role authority, the broad volatile-context matrix, the concurrent-agent
  lifecycle matrix, and the authenticated D7/P9 public/Codex real-wire gate
  remain open.
- Registered append still holds the global index lock across timeline scans and
  fsync. Existing scale and quadratic-rescan observations remain open for later
  persistence work.
- Exact-prefix interruption is fail-closed availability behavior. This slice
  does not claim automatic crash recovery for an incomplete semantic frame.
- Resume repair clears a stale provider anchor before sending its synthetic
  local tool result. Replayable repaired history resumes by full replay; stored
  reasoning without replay material fails typed before prompt mutation or wire
  dispatch rather than claiming universal seamless recovery.
- The evidence is deterministic local/process evidence and uses no live OpenAI
  credential or external network.

## Reviewer questions

1. Can malformed, duplicated, conflicting, orphaned, or interleaved provenance
   become an active anchor or be hidden from prompt projection?
2. Can any managed open/resume/fork path mutate affinity, prompt state, or a
   child before complete provider-state provenance validation, and is the later
   response-audio validation ordering described accurately?
3. Can concurrent handles or processes interleave response groups, or can an
   interrupted prefix accept a different continuation?
4. Can compaction, suppression, or a non-identity fork retain or resurrect a
   stale provider anchor after restart?
5. Do public threaded and stateless Codex request shapes preserve their distinct
   instruction, replay, storage, and compaction contracts?
6. Is the compatibility claim accurate in both directions, including reserved
   discriminator collisions and older-binary enum rejection?
7. Do both structural splits preserve behavior while actually correcting the
   7,024-line spawn module and 7,268-line runner-test root?
