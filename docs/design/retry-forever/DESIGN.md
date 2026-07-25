# Retry-forever architecture — pinned design

Date: 2026-07-25
Branch: `feat/retry-forever` (base: main @ 44eca74)
Author seat: Sable Nightwick. Reviewer: Waffles the Terrible.
Authority: owner-ratified six-question spec + hard requirements A–E
(relayed 2026-07-25 01:32Z). Recon evidence: three seam-map reports
(retry seams, cancellation plumbing, output routing) executed against
44eca74; every claim below was recon-cited at file:line and spot-checked.

## The law being implemented

Transient provider failures retry indefinitely — backoff, full jitter,
60s ceiling — until the request succeeds or the user cancels.
Non-transient failures fail the TURN loudly with a typed error; the
WORKER survives in every driver that has one. Bounded-retry-then-die is
banned. Idle retry costs ~nothing (parked timer, flat RSS, ~0 CPU), and
every attempt/wait is visible as a format-correct event, never debug
spam. In JSON output modes, stdout carries the requested format and
nothing else.

## D1 — Policy shape (`crates/norn/src/loop/retry.rs`)

```rust
pub struct RetryPolicy {
    /// Total attempts including the first. `None` = unbounded (DEFAULT).
    /// `Some(0)` is a config validation error; `Some(1)` = no retry.
    pub max_attempts: Option<u32>,
    pub initial_backoff: Duration,        // existing default 1s (kept)
    pub backoff_multiplier: f64,          // existing default 2.0 (kept)
    /// Ratified 60s. Backoff base saturates here; never exceeded.
    pub backoff_ceiling: Duration,
    /// Ratified on. Full jitter: wait = uniform(0, base).
    pub jitter: bool,
    pub retryable_errors: Vec<RetryableError>,
}
```

- `total_duration_cap()` is **deleted**. It is a bounded-retry-then-die
  mechanism (returns the error instead of sleeping when cumulative wait
  exceeds a derived cap) and already silently truncates attempts below
  what `max_retries` promises. Bounded custom policies are bounded by
  attempts only.
- Wait formula: `base_n = min(initial * multiplier^(n-1), ceiling)`;
  `wait_n = jitter ? uniform(0, base_n) : base_n`. Full jitter is the
  canonical AWS shape; expected wait `base_n / 2`.
- `classifies_as_retryable` stays a pure projection of
  `ProviderError::class()` — `Auth`/`Terminal` can never be opted into
  retry by any policy. Unchanged, already test-pinned. This is the
  structural guarantee behind spec point 5 (no classifier ever dresses
  a non-transient error up as retryable).

## D2 — RateLimited joins the default retryable set (RECOMMENDATION, flagged)

Today the provider owns 429 with a bounded internal budget; when that
budget exhausts, `ProviderError::RateLimited` surfaces and the loop
default refuses to retry it — a death on a transient condition, which
the new law bans. The old exclusion rationale (double retry burning two
bounded budgets) inverts under unbounded gentle retry.

Pinned: `RateLimited` enters the default `retryable_errors`. The loop
wait honors the server: `wait = max(sampled_wait, retry_after)` when
`RateLimited { retry_after: Some(_) }`. A server-stated `Retry-After`
MAY exceed the 60s backoff ceiling and is honored anyway — the ceiling
caps our own guessing, not the server's explicit instruction (reviewer
ruling, 2026-07-25). The transport keeps its bounded Retry-After loop
unchanged (server-directed, correct).

Flagged to reviewer/owner as the one deviation from the previously
documented layering contract; veto reverts a single Default line.

## D3 — Jitter RNG sealed in the retry layer

A `pub(crate)` sampler owned by `retry.rs`: seeded from `OsRng` at
construction, held by the retry loop, injectable in tests with a
deterministic sampler. No `rand::rng()` calls inside the loop body, no
global RNG state that scheduler/replay paths could inherit. The
workspace currently has zero RNG in any retry/backoff path and four
crypto-only `rand` uses; this keeps it that way structurally.

## D4 — Cancellation is structural, not incidental

`retry_with_backoff` gains `cancel: Option<&CancellationToken>` and
races every inter-attempt sleep in a `biased` select, cancel arm first,
using the `cancelled_or_pending` idiom from `loop/linger.rs`. Today the
sleep is interruptible only because the whole retry future happens to
sit inside `provider_call.rs`'s select — a property that silently
breaks when `cancel` is `None` or if the loop moves. The outer select
stays (defense in depth); the inner token makes the property explicit.

Call sites currently passing `cancel: None` that would otherwise host
an uninterruptible unbounded loop get real tokens:
- Rhai script children: child token of the spawner's `AgentCancellation`,
  published on the child context so grandchildren chain under it.
- ~~Schedule executor turns~~ — STRUCK (C3 implementation finding,
  2026-07-25): the recon cite pointed into `schedule/executor.rs`'s test
  module; the production executor delivers channel messages only and
  never runs agent steps, so there is no in-flight scheduled turn to
  orphan and no consumer for a token. Its shutdown remains
  `ScheduleExecutorGuard::drop → abort()`, which is safe at its only
  await points (sleep/notify).

`step_timeout` remains a valid terminator: it is user-configured, so it
counts as user intent. The rustdoc contract states this explicitly.

## D5 — Headless signal handling (print + driven)

`norn --print` today has zero signal handling: SIGINT is instant
process death — no Cancelled result, no checkpoint, orphaned tool
children. Pinned (ratified: "Ctrl-C/SIGTERM IS the user cancel"):

- A signal task in print orchestration: first SIGINT or SIGTERM trips
  the run's cancel token → the existing graceful path (Cancelled
  envelope in the requested format, tool-result repair, checkpoint).
- Second signal: immediate `exit(130)` (128+SIGINT convention).
- Unix: SIGINT + SIGTERM; non-unix: ctrl_c only (`cfg` split).

An orphaned headless retry loop that nobody signals keeps retrying
gently forever — intended behavior per ratified spec point 2.

## D6 — Worker survival (persistent spawned agents)

Recon G4: a persistent spawned worker DIES today on exactly the class
that must not kill it — hard `NornError` maps to
`status: Failed, stop: None`, and `Failed && stop.is_none()` is the
spawn controller's terminate predicate. Pinned fix: turn failure gets a
typed stop shape (error class carried), so the persistent-worker arm
falls through to `mark_idle` + `IdlePark` with mailbox and route
preserved; the failure is surfaced loudly on the result/lifecycle
events AND visible in the session record — a silent park is banned
(reviewer ruling, 2026-07-25). Panic-in-turn keeps its current
worker-fatal semantics (a poisoned worker must not idle-park).
Evidence requirement: the D8 terminal-recovery invariant tests are
named individually in the gate evidence and green. One-shot drivers (print, driven,
fork) keep exiting after reporting the typed failure — that is the
driver's lifecycle, not a death-by-retry-policy.

This touches the D8-reviewed controller seams; the change is confined
to the outcome→terminate predicate and the surviving-worker transition,
red-first, without altering pending/terminal-recovery invariants.

## D7 — TUI cancel cascade

`run_turn` mints a fresh per-turn token and the builder root token is
never used by the TUI, so Ctrl-C cannot reach spawned descendants and
TUI exit leaks child retry loops. Pinned: per-turn token becomes a
`child_token()` of the root `AgentCancellation` (turn cancel stays
turn-local; children are closed via `close_agent` as today), and the
TUI exit path cancels the root token so every descendant retry loop
dies with the app.

## D8 — Retry visibility (requirement B)

`AgentStreamRetry` already exists (`{ attempt }`) and is emitted after
the sleep from `classify.rs`. It grows to:

```rust
pub struct AgentStreamRetry {
    pub attempt: u32,              // attempt about to be made
    pub max_attempts: Option<u32>, // None = unbounded
    pub delay_ms: u64,             // actual sampled wait before it
    pub error_class: String,       // house-sanitized class label, never provider free text
}
```

Emitted BEFORE the sleep via an observer threaded into
`retry_with_backoff` (the sender cannot reach the wait today). Surfaces:
- stream-json: `{"type":"stream_retry",...}` (existing mapper row,
  enriched). Unbounded `max_attempts` serializes as `null` — never a
  sentinel number (reviewer ruling, 2026-07-25).
- driven: `event/progress` (existing routing).
- TUI: agent status line activity ("retrying in Ns (attempt N)") at the
  currently-no-op `StreamRetry` dispatch arm.
- `-f text`: a stderr status line (stdout stays final-output-only),
  suppressed by `--quiet`.
- `-f json`: rollup via the diagnostics array (zero envelope schema
  change in v1).

Error text discipline: the event carries the taxonomy class label only;
reasons stay in the loud terminal error, which is already
house-sanitized (no provider free text).

## D9 — stdout purity (requirement C)

Mechanisms found and fixed:
- **M1**: the binary's stderr tracing install is best-effort
  (`let _ = try_init()`), and library embedders of `print::run_async`
  inherit whatever subscriber the host installed — `tracing_subscriber`
  defaults to STDOUT. Fix: surface install failure loudly on stderr;
  provide and use a guaranteed stderr-tracing installer on the print
  path; document the embedder contract.
- **M3**: `-f stream-json -o PATH` splits the stream (events to
  terminal stdout, envelope to the file). Fix: parameterize the stream
  renderer's writer; `-o` redirects the whole stream.
- **Silent truncation** (`stream_renderer.rs`): any stdout write error
  reads as clean completion, exit 0. Fix: `BrokenPipe` = conventional
  quiet stop; every other write error = typed stream-torn failure,
  nonzero exit, no misleading terminal envelope.
- **Acceptance test** (the criterion itself): end-to-end run of the
  built binary with `RUST_LOG=trace` against a local mock provider in
  `-f json` and `-f stream-json`; every stdout line must parse as the
  requested format. This catches any residual mechanism regardless of
  origin.

## D10 — Resource whisper (requirement A)

The inter-attempt wait is a single parked tokio timer — zero busy-wait
by construction; nothing is buffered per attempt (the failed attempt's
partial capture is reset, and failed-attempt response-audio artifacts
are discarded before the next attempt, preventing unbounded unsealed
accumulation across unbounded attempts).

Evidence, not assertion:
- Paused-clock structural test: across a simulated multi-hour outage,
  attempt count and timer count are exactly N (no spin, no extra polls).
- Measured harness (`scripts/` + evidence artifact): the real binary
  against a mock provider that fails every request for 10+ minutes;
  RSS sampled flat, CPU ~0 between attempts. The artifact records the
  norn binary version/commit and toolchain (version-skew discipline).

## D11 — Summarization/compaction joins the brain

`summarization.rs` calls the provider directly with zero retries and
runs before the retry brain gets control — a transient 5xx during
auto-compaction kills the step. Pinned: the summarization provider call
is wrapped in the same `retry_with_backoff` with the loop's policy and
cancel token.

## Deliberately NOT in v1 (recorded, not silently dropped)

- **Sticky-routing staleness**: retries replay the cloned request with
  the same turn context, so Codex sticky routing re-pins the same
  backend node across attempts. Kept for v1 (matches today's replay
  semantics; a gentle unbounded loop outlives node sickness). Escalation
  (fresh turn context after K failures) needs an owner-ruled K — parked.
- **Rebuild-from-session-state leg**: `build_request` is not idempotent
  (drains pending rule injections, re-runs paid auto-compaction,
  replaces tool-execution lease). The brain replays the frozen request
  clone — provably identical to a rebuild for every observed strike
  class. A true rebuild arm is a separate design if ever needed.
- **Durable failure rows / usage from failed attempts**: deferral 1e
  remains the home; unbounded retry sharpens the case but does not gate.
- **Mid-retry state across restarts**: ratified out (spec point 6).
- **Linger gate semantics for newly-tokened script children** (C3
  finding, needs an owner/reviewer ruling): `linger.rs`'s short-circuit
  skips a granted linger only when a child has no cancel, no child_rx,
  and no inbound. Rhai children previously had none of the three and
  short-circuited; now that they carry a cancel token they serve a
  granted `linger_secs` in full (interruptibly). Narrowing the gate to
  work-sources only (`child_rx`/`inbound` — a cancel ends a linger, it
  never fills one) would restore the short-circuit but changes linger
  behavior for every driver. Blast radius today: only hosts that
  explicitly grant `linger_secs` to script children. Parked for the
  working session.

## Commit plan (each red-first)

1. **C1 stdout purity** (norn-cli): M1 + M3 + silent truncation +
   RUST_LOG=trace purity acceptance test.
2. **C2 retry core** (norn engine): policy reshape, cap deletion,
   sealed jitter, cancel token into the sleep, enriched pre-sleep
   `AgentStreamRetry`, RateLimited default + Retry-After floor,
   failed-attempt audio-artifact discard, config/CLI threading
   (`retry.max_attempts`, `retry.backoff_ceiling`, `retry.jitter`;
   `-c retry_max`, `-c retry_backoff_ceiling`, `-c retry_jitter`).
3. **C3 cancel plumbing**: print/driven signal task; TUI child-token +
   root-cancel-on-exit; rhai/schedule token wiring.
4. **C4 worker survival**: typed turn-failure stop; persistent worker
   idle-parks with route preserved; panics stay worker-fatal.
5. **C5 summarization wrap.**
6. **C6 visibility surfaces**: TUI status line, text-mode stderr line,
   json diagnostics rollup.
7. **C7 evidence**: paused-clock structural tests + measured
   resource-whisper harness and artifact (versions pinned).

Gates per commit: `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --check`, full test battery. Reviewer byte-review before landing.
