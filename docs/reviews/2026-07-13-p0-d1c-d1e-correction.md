# P0 D1C/D1E review correction

## Status

**Implemented and verified; independent acceptance pending.** This record closes
the actionable findings in the external review of `dc6c2e3..09b9d49`. The
weighted D1E candidate was subsequently packaged at `aa6a653`, so that later
candidate still requires review over its own range. This is not a whole-phase
Gate C or Gate D claim.

Code range: `aa6a653..d488c1a`.

## Findings

### F-1: configured index-lock deadline reporting

`lock_index` now passes two distinct values into the cross-process polling
phase: the residual poll budget and the caller's original configured wait. The
poll remains bounded by the total deadline, while every typed
`IndexLockTimeout.waited` arm reports the same configured duration.

The previously deterministic failing integration test passes 20/20 in the
retained correction runner.

### F-2: dead JSONL newline state

`JsonlSink` no longer stores `needs_newline`. Its reopen path already verifies
and heals a torn tail before returning the descriptor, so sink persistence can
write the serialized line directly. The separate single-descriptor
`write_event_line` helper remains test-only because its tear-state contract is
still independently pinned.

### F-3: first resume after a hard kill

The review's server-thread hypothesis was reproduced at request-shaping level.
Open-time repair appends a synthetic tool result locally after the assistant
response carrying `response_id`, but the old anchor discovery treated that
result as a delta after `previous_response_id`. The provider-side thread cannot
contain a result synthesized after the process died.

The synthetic interruption result is now a durable response-thread boundary.
When anchor discovery encounters it after an assistant response, it discards
that stale anchor. The first resumed request therefore has no
`previous_response_id` and fully replays the healed transcript; the next
successful response establishes a fresh anchor through the existing normal
path.

The regression constructs the exact persisted killed-mid-tool shape, runs the
real repair, and proves that the first request includes the original user
message, function call, synthetic function output, and resume prompt with no
provider anchor. It passes 20/20. This is deterministic request-construction
proof, not a claim that a live external provider or literal SIGKILL subprocess
was exercised.

### F-4: reopen cost

No change. The per-event reopen is the accepted trade for zero idle descriptor
retention; a future performance change belongs behind the same D1E lifetime
and identity invariants.

## Evidence

Retained distribution runner:

```sh
sh docs/reviews/evidence/run_p0_review_corrections.sh 20
```

Raw result:
[`2026-07-13-p0-review-corrections.json`](evidence/2026-07-13-p0-review-corrections.json)
records 20 passed, zero failed for both the deadline and repaired-anchor cases.

Verification at `d488c1a`:

- `cargo fmt --all --check`: pass.
- `cargo clippy -p norn -p norn-cli --all-targets --all-features -- -D warnings`:
  pass.
- `cargo test -p norn --lib`: 3,177 passed, zero failed, zero ignored. The first
  sandboxed run was invalid because loopback binds returned `EPERM`; the exact
  suite was rerun unrestricted and passed.
- `cargo test -p norn-cli --lib`: 444 passed, zero failed, zero ignored.
- `cargo test -p norn --lib torn_line_is_terminated_not_continued`: one passed.

The syntax-aware policy output
[`2026-07-13-p0-review-corrections-policy.json`](evidence/2026-07-13-p0-review-corrections-policy.json)
covers the five changed Rust files in `aa6a653..d488c1a`: zero production files
over 500 lines, zero thin-entrypoint violations, and zero added matches for
unwrap, expect, panic, TODO/unimplemented, lint suppression, ignored tests,
empty cfg, or unresolved markers.

## Remaining boundary

The external report accepted D1C and the idle-retention slice at `09b9d49`; it
did not review the later weighted-admission and Chiron changes ending at
`aa6a653`, nor these corrections. D1E therefore remains open until an
independent reviewer accepts `ca43c1b..d488c1a` and rules on the candidate's
explicit one-shot filesystem exclusion. D1D and the owner Gate A/B
dispositions also remain open before final Gate C.
