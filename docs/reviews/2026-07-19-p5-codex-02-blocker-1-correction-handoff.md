# P5 `CODEX-02` BLOCKER-1 correction handoff

Date: 2026-07-19
Candidate branch: `codex/p5-codex-turn-state`
Controlling review: `b86924d`
Requested scope: narrow same-reviewer confirmation of BLOCKER-1 only
Whole-P5 acceptance: explicitly out of scope

## Exact correction target

The reviewed candidate head was `c3a7aa1`; its documentation handoff was
`fdc7328`, and the controlling `NOT READY` review is
`b86924db7c9b06507b9d36d6288839a62003d5e8`.

Correction commit `de922111855530e66996a8031c89be4e3e81b7ac` changes exactly
two source paths over the review commit:

- `crates/norn/src/provider/turn.rs`
- `crates/norn/src/provider/openai/execute/turn_state_tests.rs`

Its tree is `43374607900ba95f8c64b9b6507d102d356e11a8`. The NUL-delimited
two-path inventory for `b86924d..de92211` has SHA-256
`90770101985689ac6ff8c446cde7adfd2a31ae0ed91daf61e84114c3f0412edf`.
No other production or test source changed.

## BLOCKER-1 correction

The reviewed redactor cloned the event, looked only for an object-valued
top-level `headers` field, and returned the clone unchanged for every other
shape. An admitted `response.metadata` envelope could therefore carry
`x-codex-turn-state` under an array or nested object into the always-on
`ProviderEvent::ResponseStreamEvent` sink and the optional debug JSONL sink.

`de92211` replaces that shallow lookup with one recursive in-place walker over
the cloned JSON value:

- every object key is compared case-insensitively with
  `x-codex-turn-state`;
- a matching key's complete value subtree is replaced with `[REDACTED]`;
- non-matching object values are traversed recursively;
- every array element is traversed recursively;
- scalar values require no action.

The walker still runs at both original boundaries: before constructing the raw
provider event and before writing an SSE debug record. It does not change
metadata capture, first-wins authority, replay, validation, or request
construction.

## Regression binding

The new real HTTP/SSE fixture uses the exact reproduced sentinel
`LEAKED-SECRET-XYZ` in all relevant places:

1. A successful 2xx HTTP response header supplies the authoritative
   `x-codex-turn-state`, proving the secret is live and replayable rather than
   merely unrelated JSON.
2. The `response.metadata` body places the same sentinel under the sensitive
   key in an array-shaped `headers` value.
3. A second occurrence uses mixed-case key spelling below a nested object and
   array.
4. The fixture drives the real `SenderProvider`, `StreamExecutor`, SSE parser,
   `ResponsesMapper`, provider-event channel, and enabled `DebugDumper`.
5. It asserts that the captured context still contains the original header
   state; the emitted raw envelope, full `ProviderEvent` debug output, and
   on-disk debug JSONL contain no sentinel; and the raw envelope and debug
   JSONL contain redaction markers.

The existing canonical object-shape fixtures remain. A direct unit regression
also covers array, nested-object, mixed-case, and sensitive keys outside a
field literally named `headers`, binding key-based rather than path-based
redaction.

The new integrated fixture passes 1/1 and the complete turn-state transport
module passes 5/5. Under the reviewed shallow implementation, its first
raw-event secrecy assertion would observe the sentinel and fail.

## Verification

All Cargo commands used the repository's normal shared
`/Users/tom/Developer/ablative/norn/target` directory. No temporary Cargo target
was used.

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`:
  pass with no suppression.
- `cargo test -p norn --lib provider::openai::execute::turn_state_tests`: 5/5
  pass in the network-capable run.
- `cargo test -p norn --lib provider::turn::tests`: 2/2 pass.
- `cargo test --workspace --all-features --quiet`: 5,406/5,406 unit and
  integration harness tests pass; 8/8 doctests pass.
- `git diff --check`: pass.

The strict Clippy gate initially identified a needless pass-by-value in the new
test helper. The helper was changed to borrow its JSON input; no lint allowance
or suppression was introduced, and the rerun passed.

## Policy and size

The correction adds no `#[allow]`, `unwrap(`, `expect(`, `panic!`, `todo!`,
`unimplemented!`, `unsafe`, `TODO`, or `HACK` occurrence. It adds no timeout,
retry, cadence, queue, or other operational limit.

Both touched files remain below 500 lines. `provider/turn.rs` is 281 physical
lines with a 205-line production prefix. The integrated fixture file is test
only and 417 physical lines.

## Internal correction review

A final read-only audit returned `READY` with no finding. It verified arbitrary
object/array traversal, case-insensitive key matching, full sensitive-subtree
replacement, the exploit fixture's real transport and sink coverage, source
sizes, and the no-bypass policy. The auditor did not edit source or substitute
for the required same-reviewer confirmation.

## Honest boundaries

- The controlling external verdict remains `NOT READY` until the same reviewer
  confirms this correction.
- This handoff does not request a new full panel or a whole-P5 verdict.
- The four non-blocking observations in `b86924d` remain unchanged.
- D3, D8, credential/account affinity, resume and broader concurrency/error
  boundaries, WebSockets, and the mandatory D7/P9 authenticated real-wire gate
  remain open.

## Confirmation questions

1. Does the walker redact every case-insensitive sensitive key through arbitrary
   object and array nesting, independent of the `headers` shape?
2. Can either the emitted raw provider event or debug JSONL retain the sentinel
   from the reproduced exploit?
3. Does the integrated regression actually traverse the real transport,
   parser, mapper, provider-event, and debug-dump paths?
4. Did the correction alter any capture, replay, trust-gating, metadata, or
   turn-lifetime behavior outside BLOCKER-1?
5. Does any claim above exceed the two-file diff or reported evidence?
