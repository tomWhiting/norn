# Remediation review — transport & streaming (OpenAI Responses provider)

> **Coordinator intake note:** This report is preserved as provisional review
> input against `7d121c9`. It is not a P0 phase or Gate D verdict. Open
> findings require tracking, a final fix-round review, and complete machine-gate
> evidence.

> Reviewed against the **frozen snapshot taken 2026-07-11 while work was in
> flight** (`scratchpad/remediation-review/norn-snapshot`, snapshot HEAD
> `7d121c9`, base pin `41ea210`). No builds or tests were run per review
> constraints; all evidence is from reading the snapshot source and a
> reviewer-generated unified diff of the owned files against `41ea210`.

## Scope note — what the diff actually contains

The change since base is the **P0 security-containment phase** of
`docs/RESPONSES-API-REMEDIATION-PLAN.md` (credential redaction, error-body
non-disclosure, redirect refusal, backend confinement, debug-dump hardening).
The plan itself marks the phases that own this review's findings as **not
started**: P4 (`EVT-01`), P5 (`TRANS-01`), P6 (`TRANS-02`, `USAGE-01`), P8
(`CACHE-02`), and owner decision **D4 (single retry owner) is still open**
(plan lines 246, 679, 757, 833, 967). The code confirms the plan's
self-assessment: none of the five transport/streaming findings is remediated
in this snapshot. Verdicts and evidence below, then new findings from the P0
work that did land in the owned files.

## Verdict table

| Finding | Verdict | Evidence |
|---|---|---|
| TRANS-01 | **Not closed** | `crates/norn/src/provider/openai/provider.rs:184-188` and `crates/norn/src/provider/openai_compatible/provider.rs:146-150`: `stream()` still detaches the producer with a bare `tokio::spawn`, discards the `JoinHandle`, and returns a plain `ReceiverStream`. No abort-on-drop guard, no `tx.closed()` select anywhere in `provider/exec.rs`. Receiver drop is noticed only at the next failed `tx.send`. Leak windows: (a) header wait — `exec.rs:167` `timeout(self.timeout, builder.send())` runs to the deadline; (b) `rate_limiter.acquire().await` (`exec.rs:156`) and an imposed 429 cooldown + `sleep(wait)` (`exec.rs:265-272`) — the detached task sits through the full backoff and can re-send the request after the consumer is gone; (c) error-body drain up to `self.timeout` (`exec.rs:301`); (d) a live SSE stream emitting only frames that map to zero events (`response.in_progress`, `content_part.added`, unrecognized types — `sse.rs:441-459`) never calls `tx.send`, so the producer consumes it indefinitely with a dropped consumer. |
| TRANS-02 | **Not closed** | Single-owner retry does not exist. HTTP 429 is retried inside the executor (`exec.rs:238-274`); an in-band `response.failed` `rate_limit_exceeded` becomes `ProviderError::RateLimited` (`sse_types.rs:129-131`), is forwarded as a stream `Err`, and `loop/retry.rs` is **byte-identical to base** — `RateLimited` remains excluded from the default loop policy (`retry.rs:91-94, 273-279`), so the same condition is retried before SSE begins and terminal after. No attempt/budget metadata is carried. `emit_mapped` (`exec.rs:459-478`) still terminates only on `Done` or receiver drop, not on a mapped `Err`: after a `response.failed` the producer keeps consuming to EOF, `finish_on_clean_close` returns `None`, and `execute` returns `Err(StreamInterrupted)` which the spawn task sends as a **second, retryable** error after the terminal one (`provider.rs:185-187`). Today's only consumer (`loop/classify.rs:223-224`) returns on the first `Err` and drops the receiver, so the second send fails harmlessly — but any draining consumer would see a terminal error reclassified by a trailing retryable one. Blocked on open owner decision D4; nothing landed. |
| CACHE-02 | **Not closed** | `crates/norn/src/provider/openai/sse.rs:487`: `cache_write_tokens: 0` is still hard-coded in `extract_usage`. `provider/usage.rs` is byte-identical to base (no `Option` fields, `cost_usd` never populated by the Responses parser). |
| USAGE-01 | **Not closed** | `sse.rs:469-471`: absent `usage` collapses to `Usage::default()` — reported-zero and not-reported are indistinguishable. `response.failed` mapping (`sse.rs:372-389`) emits only the `Err`; any terminal usage in the failed payload is discarded. Loop retries still surface only the successful attempt's usage. |
| EVT-01 | **Not addressed** | `grep -rn refusal crates/norn/src/provider/` matches only a comment (`sse.rs:1239`). `response.refusal.delta`/`response.refusal.done` are not in the dispatcher's match (`sse.rs:441-459`) and fall through to the debug-level "unrecognized SSE event type" skip — a refusal still becomes an empty successful response. The plan assigns this to P4 ("Not started"), so out of scope for the landed work, but it remains open. |

None of the five regressed: the P0 redaction changes altered error *text* on
these paths, not classification (`transient` kinds, `RateLimited`
`retry_after` parsing, and stop-reason mapping are semantically unchanged).

## Known-diagnostics check: `http_client.rs` dead_code

The reported `dead_code` diagnostics on `build_streaming_client`,
`build_bounded_client`, `build_blocking_bounded_client` **do not reproduce
against the frozen snapshot**. All three have production callers:

- `build_streaming_client` — `provider/openai/provider.rs:142`, `provider/openai_compatible/provider.rs:168`
- `build_bounded_client` — `provider/openai_oauth/manager.rs:76`, `refresh.rs:177`, `revoke.rs:62`
- `build_blocking_bounded_client` — `provider/openai_oauth/login_server.rs:317`

No direct `reqwest::Client::builder()` construction remains in the provider
layer (remaining direct builders are in `tools/web/{fetch,search}.rs`,
`integration/{extensions,mcp_client}.rs`, `norn-cli doctor` — outside this
scope; `provider/auth.rs` hits are `#[cfg(test)]`). So there are **not** two
client-construction paths alive in the snapshot. Implication: the diagnostics
were captured while the implementer's working tree was mid-wiring (module
created before call sites landed), or wiring has regressed **after** the
snapshot. This must be re-checked against the live tree at land time — if
`dead_code` still fires there, the redirect-refusing client is not actually
protecting the streaming path in the tree being landed.

## New findings (ranked)

### NF-1 — Medium, CONFIRMED: unknown `response.failed` codes and unknown `incomplete` reasons now carry zero diagnostic signal anywhere

`provider/openai/sse_types.rs:151-155`: an unrecognized error code maps to the
fixed string "provider returned an unrecognized response.failed error" — the
code itself is dropped. `sse_types.rs:183-186`: an unrecognized
`incomplete_details.reason` likewise drops the reason. Neither path emits any
`tracing` event (the `response.failed` arm in `sse.rs:372-389` logs nothing;
`classify_failed_error` and `incomplete_stop_reason` are pure). Before this
change the reason text was carried in the error; now the operator record —
logs, session events, error surfaces — contains no way to distinguish one
unknown code from another. Failure scenario: OpenAI ships a new terminal code
(e.g. a billing hard-stop); every affected turn reports the same generic
string, and the operator cannot tell whether one novel condition or five are
in play, nor file an accurate upstream report. This is silent loss of the
only discriminator on a terminal failure path (house rule: no silent
failures). The D1 redaction ruling covers provider *message* text; an error
`code` is a short enum-like identifier, and a bounded/validated structural log
(e.g. code logged only if it matches `[a-z0-9_]{1,64}`, else its length and
hash) would preserve the redaction posture. Needs an owner ruling either way
— but shipping *nothing*, not even a warn-level counter, is a defect.

### NF-2 — Medium, CONFIRMED (behavior) / PLAUSIBLE (user impact): 3xx responses are now terminal errors with a misleading reason and no redirect hint

`provider/http_client.rs:14` sets `redirect::Policy::none()` on the streaming
client (owner-ruled, D1 / DECISIONS §6 — correct for credential safety). A
redirect status now falls into `!status.is_success()` (`exec.rs:276-278`) and
is classified by `error_response_to_provider_error` (`exec.rs:322-332`) as
`StreamError { transient: None }` — terminal — with reason
`"HTTP 307 Temporary Redirect from chat completions; response body omitted"`.
Two problems: (a) the base behavior was reqwest's default follow-up-to-10, so
`openai_compatible` deployments behind gateways that issue same-origin
redirects (trailing-slash normalization, LiteLLM/proxy http→https upgrades)
silently flip from working to hard-failing with a message that never mentions
redirects are refused — the one fact needed to fix the config; (b) the
taxonomy is wrong: a 3xx is not a stream error with an omitted body, it is a
policy refusal at this client. The reason string should state that the
endpoint attempted a redirect and that redirects are not followed for
credential-bearing requests (the target host — not the full Location value —
is arguably safe and decisively diagnostic, but even without it the hint
must be present).

### NF-3 — Low, CONFIRMED: in-band `retry_after` is not clamped by `retry_after_ceiling`

`sse_types.rs:129-131` parses `retry_after` out of the provider-controlled
failure message (`parse_retry_after`, `sse_types.rs:200-212`) with no ceiling;
`retry_after_ceiling` clamps only the HTTP-429 header path
(`exec.rs:253-256`). Today nothing sleeps on it — the default loop policy
treats `RateLimited` as terminal and `retry_with_backoff` uses its own
exponential backoff — so this is latent. But any consumer that honors
`ProviderError::RateLimited::retry_after` (UI, custom policy per
`retry.rs:481-486`) inherits an unclamped, authority-controlled duration
(e.g. "try again in 99999999s"). Fold the clamp into the D4/P6 single-owner
work so the two rate-limit paths cannot diverge again.

### NF-4 — Low, CONFIRMED: blanket header redaction in debug dumps erases the one value OpenAI support asks for

`provider/debug.rs:112-124` redacts **every** response-header value, including
`x-request-id` and `content-type`. The redaction posture is owner-ruled
(DECISIONS §6: "every response-header value is redacted"), so this is
compliant — recorded here because the request id is the canonical correlation
key for upstream incident reports, and dumps that exist specifically for
debugging now cannot correlate a failing response with OpenAI's own logs. If
the owner wants an exception, it should be an explicit allowlist entry
(`x-request-id` only), not a relaxation of the default. Related hygiene:
`exec.rs:133-143` still materializes the real header values into a
`Vec<(String, String)>` and hands them to `write_response_meta`, which then
discards them — redacting at the collection site would remove a standing
invitation for a future sink to leak them.

### NF-5 — Info: post-first-error producer behavior (double terminal signal)

Recorded as evidence under TRANS-02 rather than a separate defect: after any
mapped `Err`, the detached producer continues to EOF and `execute()`'s
`Err(StreamInterrupted)` is pushed as a second error into the channel
(`provider.rs:184-188`). Safe with the current single consumer
(`classify.rs:224` returns on first `Err`), but it is exactly the
"treat a mapped error as terminal immediately after delivery" fix the
original review specified, still absent.

## House-rules check on the landed (P0) changes in owned files

- No `unwrap`/`expect` introduced in production code (`value.to_str().unwrap_or("<binary>")` at `exec.rs:140` is `unwrap_or`, pre-existing).
- `#[allow(...)]` appears only inside `#[cfg(test)]` items in the owned diffs.
- No invented constants: `0o600`, `O_NOFOLLOW`/`O_NONBLOCK`, `Policy::none()`, and full-value header redaction are owner-ruled (D1, DECISIONS-2026-07 §6); keepalive/pool numbers in `http_client.rs` are carried over verbatim from the base inline builders; `channel(64)` and `DEFAULT_RETRY_BACKOFF` predate this work.
- Error-body drain (`exec.rs:292-333`) is time-bounded (total `self.timeout` over the whole drain — strictly tighter than the base `response.text()` behavior) and drops chunks without buffering; drain failures are logged, not swallowed. No byte-cap was added, which is consistent with the no-invented-limits rule.
- `with_auth_provider` narrowed to `pub(crate)` + `#[cfg(test)]` on both providers: every remaining caller in the snapshot is inside `#[cfg(test)]` (including `loop/conversation_state.rs:282`), consistent with the no-backwards-compat rule. Note this deletes a formerly `pub` API — external embedders (meridian) must construct via `new()`.
- File sizes: `http_client.rs` 171, `exec.rs` 478 (incl. docs), `debug.rs` 445 (incl. tests) — within budget; `sse.rs`/`execute.rs` totals are test-dominated and their production splits predate this work.
- `debug.rs` dump-file hardening (symlink rejection, FIFO rejection via `O_NONBLOCK` + `is_file` re-check on the opened handle, 0600 enforcement on every open) is correctly ordered: the post-open `metadata` check closes the classic pre-open TOCTOU, and `O_NOFOLLOW` covers the final component. All failure paths log.

## Bottom line

The snapshot contains a competently executed P0 security pass, with the four
diagnosability/latent issues above. It contains **no remediation** for
TRANS-01, TRANS-02, CACHE-02, USAGE-01, or EVT-01 — all five remain open
exactly as described in the 2026-07-10 review, consistent with the plan's own
phase gating (P4/P5/P6/P8 not started, D4 undecided). Any claim that the
transport/streaming findings are fixed in this snapshot would be false.
