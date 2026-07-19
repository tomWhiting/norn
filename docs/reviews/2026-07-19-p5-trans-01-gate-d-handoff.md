# P5 `TRANS-01` Gate D handoff

**Status:** READY FOR INDEPENDENT REVIEW. This is candidate evidence, not
`TRANS-01` acceptance, P5 Gate B completion, or whole-P5 acceptance.

## Frozen source

- Exact range: `d46bbe2aa7f9556d010b0662d87e002e45304134..e448133d285d9cbabc464ca89d1497a55757f4e1`
- Source tree: `065a1a73ebbe9e2fcfaf38f0dbd85e6cd6c4440f`
- Exact eight-path NUL inventory SHA-256:
  `c411a068951adf8e5f7ef28f34a8f639d69466aef80a37407223794563bafe8a`
- Product diff: 720 insertions, 13 deletions across seven Rust files and the
  remediation plan.
- Packaging HEAD during evidence execution: `1869828`; changes after the
  frozen source are documentation/evidence only. The sole later `.rs` path is
  the P2 evidence fixture `docs/reviews/evidence/p2/p2_live_refresh_probe.rs`;
  no later product Rust path exists, and all seven frozen Rust blobs match at
  packaging HEAD.

The exact source inventory is embedded in the retained evidence artifact. The
review should use the frozen range above rather than treating later P2 or NS0
documentation/evidence as part of this product slice.

## Outcome under review

The slice adds one crate-private `TaskOwnedProviderStream`. It owns the existing
bounded receiver and its producer `JoinHandle`; `Drop` aborts the producer. Both
the Responses and OpenAI-compatible provider paths return this wrapper.

The intended observable result is that abandoning a provider stream also ends
the request task and releases its HTTP socket. That covers rate-limiter waits,
response-header waits, 429 backoff, error-body drains, silent SSE reads, and a
blocked channel send. Existing user-cancellation and step-timeout outcomes stay
`Cancelled` and `TimedOut`; direct stream drop does not synthesize a provider
error.

This slice does not change public stream or executor types, request formation,
retry policy, channel capacity, timeout policy, OAuth refresh-worker ownership,
or any operational limit.

## Retained evidence

| Artifact | SHA-256 |
| --- | --- |
| `docs/reviews/evidence/p5-trans-01/2026-07-19-p5-trans-01-evidence-e448133.json` | `8dee5be431a373f0bfd856d284f42bc34fb91df011cf4eff3765297a0e4bd635` |
| `docs/reviews/evidence/p5-trans-01/2026-07-19-p5-trans-01-policy-e448133.json` | `a9757b0983bf880e38a006e1306e24af7f93b44942d57c437fb29a9441e2e819` |
| `docs/reviews/evidence/p5-trans-01/run_p5_trans_01_evidence.py` | `a2b011c828af5f780fbc62c71e19be560bbbfb3804f47cc9fc92cc381f762fff` |

The runner bound itself to the exact base, source, tree, eight-path inventory,
and seven Rust blob identities before running any gate. It used only the shared
repository target at `/Users/tom/Developer/ablative/norn/target`.

Strict candidate gates all passed:

- `cargo +1.94.0 --locked fmt --all -- --check`
- `cargo +1.94.0 --locked clippy --workspace --all-targets --all-features -- -D warnings`
- `git diff --check d46bbe2..e448133`
- the established syntax-aware source policy over the exact range

The policy report records seven changed Rust files, three test-only files, zero
over-500 production files, zero module/entrypoint violations, and zero matches
in every added-line bypass category. Production-code counts are 155, 139, 62,
and 32 for the four product-bearing Rust files.

The runner executed each of these exact tests in 20 separate Cargo processes,
required exactly one observed test per invocation, and retained every result:

1. `dropping_stream_aborts_a_rate_limiter_wait`
2. `dropping_stream_aborts_its_producer_task`
3. `dropping_stream_releases_a_blocked_channel_send`
4. `dropping_stream_aborts_real_429_backoff`
5. `compatible_provider_receiver_drop_closes_socket`
6. `receiver_drop_cancels_error_body_drain`
7. `receiver_drop_cancels_response_header_wait`
8. `receiver_drop_cancels_silent_sse_read`
9. `real_loop_cancellation_closes_provider_socket`
10. `real_step_timeout_closes_provider_socket`

Result: **200/200** exact process-isolated observations, 20/20 for every case,
with every requested selector observed exactly once per process: none was
missing or vacuous. Each test also has its own 20-case internal repetition, but
the retained 200/200 claim counts only the process-level observations and does
not multiply those internal loops.

## Independent review request

Review the following seams rather than rerunning a broad phase audit:

1. Confirm the wrapper owns both receiver and producer, forwards stream polling
   unchanged, and cannot leave the task detached when the consumer is dropped.
2. Confirm both provider call sites retain the same bounded channel, error-send,
   request, retry, and public-return semantics.
3. Attack the raw-socket fixtures for synthetic success, server-side task abort,
   sleep-based timing, filtered-out tests, or failure to prove peer-observed
   closure.
4. Confirm real loop cancellation and step timeout reach the same provider-drop
   boundary and preserve their existing typed outcomes.
5. Reproduce the source/tree/inventory binding, runner hash, strict gates, policy
   totals, and 10-by-20 distribution before returning a verdict.

## Honest boundaries

- `TRANS-01` remains open until an independent reviewer returns `READY` and the
  owner records acceptance.
- This package is not P5 Gate B, Gate C for all of P5, or whole-P5 acceptance.
- D3 and D8 remain undecided, and `STATE-02`, `STATE-03`, `ROLE-01`, `CODEX-01`,
  and `CODEX-02` remain open.
- The retained run is deterministic local-loopback evidence. It uses no live
  OpenAI credentials or external network and makes no authenticated-wire claim.
- The package runs focused distributions plus strict fmt, Clippy, diff, and
  policy gates. It does not claim a fresh full-workspace test or doctest run.
