# C7 — resource-whisper evidence (hard requirement A)

Date: 2026-07-25
Branch: `retry/c7-evidence` (base: `feat/retry-forever`)
Design: `docs/design/retry-forever/DESIGN.md` § D10.

Hard requirement A, owner-ratified 2026-07-25: *an idle retry loop must be a
parked timer — zero busy-wait, flat RSS — with **measured** acceptance
evidence, not claims.*

This artifact records numbers. It states **no pass/fail thresholds**: the
ratified criteria are qualitative (flat RSS = no growth trend across the
window; CPU ≈ idle between attempts) and the owner reads the measurements.
Nothing below is inferred, extrapolated, or rounded in the favourable
direction; every figure is transcribed from the harness output files.

---

## 1. Provenance

| Fact | Value |
| --- | --- |
| norn commit (branch base) | `efe97580d7d7b1976bcc07afb2b95379095225af` |
| Production source change in this commit | **none** — the only source diff is a pure insertion inside `#[cfg(test)] mod tests` in `crates/norn/src/loop/retry.rs` (hunk `@@ -1484,0 +1485,255 @@`, module opens at line 476). Tests are not compiled into the release binary, so the measured binary is byte-identical to one built from `efe9758`. |
| Binary | `target/release/norn` (release profile) |
| Binary sha256 | `bc3b64cdf1198f2fa6c518a8ba99ef77445ad010826f1749f8689b862b4f7590` |
| `norn --version` | `norn 0.1.0` |
| `rustc --version` | `rustc 1.94.0 (4a4ef493e 2026-03-02)` |
| `cargo --version` | `cargo 1.94.0 (85eff7c80 2026-01-15)` |
| Host | `Darwin 25.3.0`, `xnu-12377.91.3~2`, `arm64` (Apple M1 Pro, T6000), macOS 26.3.1 |
| Harness | `docs/design/retry-forever/harness/resource-whisper.sh` |

---

## 2. The measured scenario

The retry brain classifies a refused TCP connection as retryable, so a real
`norn --print` run against a dead endpoint retries forever by design. That
is the scenario, end to end through the shipped binary — not a mock, not a
unit fixture:

1. `reqwest` fails to connect (`ECONNREFUSED`).
2. `provider/exec.rs` maps it to
   `ProviderError::ConnectionFailed { kind: TransientKind::ConnectionReset }`
   (observed in `stderr.log`: `is_timeout=false is_connect=true`).
3. `RetryPolicy::classifies_as_retryable` accepts it via the default
   `RetryableError::ConnectionReset`.
4. The loop announces the wait (`stream_retry`), parks a timer, retries.

The endpoint is `http://127.0.0.1:49731/v1`, chosen because nothing listens
there. The harness **probes the port before launching** and aborts (exit 69)
if anything answers, so the measurement can never silently run against a
live service.

### Credential discipline

No credential is used or present. Authentication is pinned to
`-c auth=api_key -c api_key_env=NORN_C7_DUMMY_KEY`, and the harness exports
`NORN_C7_DUMMY_KEY=dummy-key-not-a-credential` — a literal placeholder.
Pinning `auth=api_key` also keeps every OAuth path out of the run, so no
stored ChatGPT/Codex credential can be picked up and sent to the overridden
`base_url`. `NORN_HOME` is redirected into a throwaway directory inside the
run output, so the run touches no real Norn home. Nothing in this artifact
is redacted, because nothing sensitive was ever produced.

---

## 3. Harness command lines (verbatim)

Port probe (run before the measurement; exit 1 = nothing listening):

```
nc -z -G 1 127.0.0.1 49731
```
```
PROBE_EXIT=1 (nonzero = nothing listening)
```

Build:

```
cargo build --release -p norn-cli --bin norn
```
```
BUILD_EXIT=0
```

Measurement (`BINARY PORT DURATION_S INTERVAL_S GRACE_S OUT_DIR` — the
harness has no defaults; every parameter is supplied explicitly so the
window, sampling period and grace period are on the record):

```
./docs/design/retry-forever/harness/resource-whisper.sh \
    ./target/release/norn 49731 900 15 30 ./c7-runs/measured
```
```
HARNESS_EXIT=0
```

The run the harness launched, as recorded in `meta.txt`:

```
./target/release/norn \
    -p <PROMPT> \
    -m sol \
    -f stream-json \
    -C <OUT_DIR>/workdir \
    -c base_url=http://127.0.0.1:49731/v1 \
    -c auth=api_key \
    -c api_key_env=NORN_C7_DUMMY_KEY
```

with `NORN_HOME=<OUT_DIR>/norn-home` and
`NORN_C7_DUMMY_KEY=dummy-key-not-a-credential`.

Sampling command, once per interval:

```
ps -o rss=,pcpu=,utime=,stime= -p <RUN_PID>
```

Run start `2026-07-25T08:36:52Z`, finish `2026-07-25T08:51:55Z`. No other
`cargo` build, test, or clippy job ran in this worktree during the window.

---

## 4. Raw sample table

61 samples, one per 15 s, from process launch to the moment of SIGTERM.
`rss_kb` is resident set size in kilobytes; `pcpu` is what `ps` reported;
`utime`/`stime` are **cumulative** user and system CPU time (`M:SS.CC`) —
cumulative time is the decisive column, because a busy-wait cannot consume
zero of it.

| elapsed_s | rss_kb | pcpu | utime | stime |
| ---: | ---: | ---: | --- | --- |
| 0 | 6864 | 0.0 | 0:00.00 | 0:00.00 |
| 15 | 25200 | 0.0 | 0:00.01 | 0:00.03 |
| 30 | 25280 | 0.0 | 0:00.01 | 0:00.03 |
| 45 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 60 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 75 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 90 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 105 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 120 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 135 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 150 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 165 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 180 | 25616 | 0.0 | 0:00.02 | 0:00.03 |
| 195 | 25648 | 0.0 | 0:00.02 | 0:00.03 |
| 210 | 25680 | 0.0 | 0:00.02 | 0:00.03 |
| 225 | 25680 | 0.0 | 0:00.02 | 0:00.03 |
| 240 | 25680 | 0.0 | 0:00.02 | 0:00.03 |
| 255 | 25680 | 0.0 | 0:00.02 | 0:00.03 |
| 271 | 25680 | 0.0 | 0:00.02 | 0:00.03 |
| 286 | 25680 | 0.0 | 0:00.03 | 0:00.03 |
| 301 | 25680 | 0.0 | 0:00.03 | 0:00.03 |
| 316 | 25680 | 0.0 | 0:00.03 | 0:00.03 |
| 331 | 25680 | 0.0 | 0:00.03 | 0:00.03 |
| 346 | 25680 | 0.0 | 0:00.03 | 0:00.03 |
| 361 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 376 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 391 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 406 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 421 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 436 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 451 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 466 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 481 | 25680 | 0.0 | 0:00.03 | 0:00.04 |
| 496 | 25680 | 0.0 | 0:00.04 | 0:00.04 |
| 511 | 25680 | 0.0 | 0:00.04 | 0:00.04 |
| 526 | 25680 | 0.0 | 0:00.04 | 0:00.04 |
| 541 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 556 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 571 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 586 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 601 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 616 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 631 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 646 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 661 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 676 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 691 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 706 | 25696 | 0.0 | 0:00.04 | 0:00.04 |
| 721 | 20304 | 0.0 | 0:00.04 | 0:00.04 |
| 736 | 20304 | 0.0 | 0:00.04 | 0:00.04 |
| 751 | 20592 | 0.0 | 0:00.04 | 0:00.04 |
| 766 | 20592 | 0.0 | 0:00.04 | 0:00.04 |
| 781 | 20592 | 0.0 | 0:00.04 | 0:00.04 |
| 797 | 20880 | 0.0 | 0:00.04 | 0:00.04 |
| 812 | 21024 | 0.0 | 0:00.05 | 0:00.04 |
| 827 | 21024 | 0.0 | 0:00.05 | 0:00.04 |
| 842 | 21024 | 0.0 | 0:00.05 | 0:00.04 |
| 857 | 21024 | 0.0 | 0:00.05 | 0:00.04 |
| 872 | 21024 | 0.0 | 0:00.05 | 0:00.04 |
| 887 | 20352 | 0.0 | 0:00.05 | 0:00.04 |

---

## 5. Observations — memory

- **First sample (t=0, 6,864 KB)** is the process mid-startup, before the
  agent assembly has run. It is not a steady-state figure and is reported
  only for completeness.
- **Steady state begins at t=15 s: 25,200 KB.**
- **Peak across the whole window: 25,696 KB**, first reached at t=541 s and
  held to t=706 s.
- **Final sample (t=887 s): 20,352 KB.**
- **Net change from the first steady-state sample to the last:
  −4,848 KB (−19.2 %).** RSS *fell*; it did not grow.
- Total spread between the steady-state minimum (20,304 KB) and the maximum
  (25,696 KB) is 5,392 KB, and the entire excursion is downward: the series
  is a flat plateau at ~25.6 MB for the first 706 s, then a single step down
  to ~20.3–21.0 MB from t=721 s onwards, consistent with the kernel
  reclaiming clean pages. Between t=210 s and t=706 s — 33 consecutive
  samples, 8 minutes — RSS moved by 16 KB in total (25,680 → 25,696).
- **No growth trend is present in the series.** Across 43 retry cycles
  there is no per-attempt accumulation: monotonic growth of any per-attempt
  buffer would have shown as a rising staircase, and the observed series
  contains no rising step at all.

## 6. Observations — CPU

- **`ps` reported `pcpu` = 0.0 on every one of the 61 samples.**
- **Total CPU consumed over the 887 s of sampled life: 0.09 s**
  (`utime` 0.05 s + `stime` 0.04 s). That is an average of
  **0.010 % of one core** across the whole run.
- **53 of the 59 sample-to-sample intervals show a cumulative CPU delta of
  exactly zero** — i.e. in 53 quarter-minute windows the process consumed
  no measurable CPU at all. Every non-zero interval is listed in full,
  nothing omitted:

  | interval ending (s) | Δ CPU (s) | interval CPU % |
  | ---: | ---: | ---: |
  | 15 | 0.04 | 0.267 |
  | 45 | 0.01 | 0.067 |
  | 286 | 0.01 | 0.067 |
  | 361 | 0.01 | 0.067 |
  | 496 | 0.01 | 0.067 |
  | 812 | 0.01 | 0.067 |

  The largest interval is the first, which contains process startup,
  configuration assembly, prompt build and the first connect attempts. The
  maximum CPU share observed in **any** 15 s window is **0.267 %**, and the
  maximum after startup is **0.067 %** — the resolution floor of `ps`
  (one 10 ms tick per 15,000 ms window).
- From t=15 s to t=887 s the process consumed **0.05 s of CPU while making
  43 retry attempts** — on the order of a millisecond of CPU per attempt,
  with the rest of the 872 s spent consuming none.

## 7. Observations — the retry schedule (visibility)

43 `stream_retry` events were emitted on stdout, one per wait, for
attempts 2 through 44 — **contiguous, no gaps, no duplicates**. Every event
carried `"max_attempts": null` (unbounded, serialized as JSON `null`, never
a sentinel) and `"error_class": "connection_reset"` (taxonomy label only,
no provider free text).

Announced delays in emission order, ms:

```
290, 1173, 800, 5577, 2973, 3491, 6277, 11977, 59199, 1908, 24456, 33041,
2022, 32524, 11279, 16329, 23548, 34044, 37004, 6108, 29523, 9592, 38924,
47473, 32945, 10480, 23104, 24075, 16370, 11371, 31033, 28617, 10112,
59988, 11217, 51726, 43363, 10703, 28599, 42787, 13074, 2392, 18397
```

Facts read off that series:

- **Minimum 290 ms, maximum 59,988 ms. No delay exceeded 60,000 ms**, the
  ratified `backoff_ceiling`, at any point in the run.
- The backoff *base* saturates at the 60 s ceiling from the seventh failure
  onward, so the 37 events for attempts 8–44 are all draws from
  `uniform(0, 60000] ms`. Their observed mean is **24,205 ms**, minimum
  1,908 ms, maximum 59,988 ms — the expected full-jitter shape (expected
  value `base / 2` = 30,000 ms), which is exactly why the run fits 43 waits
  into 15 minutes rather than the 9 a fixed 60 s schedule would allow.
- Announced delays sum to 909,885 ms against a 902 s measured window. The
  excess is the 44th event's 18,397 ms wait, which SIGTERM cut short —
  itself an observation that the wait is interruptible rather than a
  sunk timer.
- Sample of the raw stdout lines:

  ```
  {"type":"stream_retry","attempt":2,"max_attempts":null,"delay_ms":290,"error_class":"connection_reset"}
  {"type":"stream_retry","attempt":10,"max_attempts":null,"delay_ms":59199,"error_class":"connection_reset"}
  {"type":"stream_retry","attempt":44,"max_attempts":null,"delay_ms":18397,"error_class":"connection_reset"}
  ```

- Corroborating stderr from this run's `stderr.log` (human log, ANSI
  colour codes stripped), first cycle:

  ```
  2026-07-25T08:36:52.725479Z  WARN norn::provider::exec: provider request failed elapsed_s=0.000575667 is_timeout=false is_connect=true error=error sending request backend="responses"
  2026-07-25T08:36:52.725758Z  WARN norn::r#loop::retry: provider call failed; retrying after backoff attempt=2 max_attempts=None error_class="connection_reset" error=connection failed: request failed: error sending request delay_ms=290
  ```

  The `elapsed_s=0.000575667` on the failed request is the connect attempt
  itself: refusal is immediate, so essentially the entire 902 s window is
  inter-attempt wait rather than in-flight work.

## 8. Observations — cancellation and exit

- SIGTERM was sent at elapsed **902 s**; `kill` returned **0**.
- The binary acknowledged it on stderr — the last line of `stderr.log`:

  ```
  norn: SIGTERM received; cancelling the run — signal again to exit immediately
  ```

- The process was gone at the harness's next 1 s poll:
  `seconds_waited_for_graceful_exit=1`, `escalated_to_sigkill=0`. The
  30 s grace budget was never approached, and SIGKILL was never needed —
  the run woke out of a mid-flight 18.4 s backoff wait rather than serving
  it out.
- The final stdout line is the graceful envelope, not a truncation:

  ```
  {"type":"completed","envelope_version":1,"stop":{"reason":"cancelled"},"output":null,"usage":{"input_tokens":0,"output_tokens":0}}
  ```

- **Observed exit code: 1.** Per `crates/norn-cli/src/cli/exit.rs` that is
  `ExitCode::AgentError`; the enum has no `Cancelled` variant, so a
  signal-cancelled run is currently indistinguishable by exit code from a
  provider or tool failure. Recorded as a **factual observation for the
  owner**, not a defect claim and not in scope for C7 (no production code
  was changed by this commit).

## 9. Side observation — stdout purity

Not the subject of this leg (that is C1 / design D9), but the run produced
the evidence for free and it is recorded rather than discarded: all **48**
stdout lines of the `-f stream-json` run parsed as JSON, **0 unparseable**.
This run did **not** set `RUST_LOG=trace`, so it is corroboration of the
default-tracing case only — it is not the D9 acceptance test.

---

## 10. Structural evidence (paused-clock tests)

The measured half above shows the loop *was* idle. The structural half
shows it *must* be, and pins the property against regression. Three tests
in `crates/norn/src/loop/retry.rs` (module `tests`):

| Test | What it establishes |
| --- | --- |
| `unbounded_retry_spends_virtual_hours_and_no_real_time` | Under the shipped default policy (unbounded, live OS jitter, 60 s ceiling), a simulated ~10-hour outage — 600 consecutive transient failures then success — completes while real time advances by less than `DEFAULT_INITIAL_BACKOFF`. Also asserts exactly one announced wait per failure. |
| `virtual_time_advanced_equals_the_sum_of_the_announced_delays` | With jitter off, total virtual time equals the sum of the announced delays *exactly*, and every attempt begins exactly one whole announced wait after the previous one — so no wakeup shortens a wait, pads it, or splits it. All expected values are computed from `RetryPolicy::backoff_base`, not transcribed. |
| `cancel_inside_a_ceiling_saturated_wait_wakes_without_finishing_it` | Cancellation deep in a ceiling-saturated streak (attempt 9, mid-way through a 60 s wait) returns `Cancelled` at the token's instant, not at the end of the backoff, at no real-time cost. |

The mechanism that makes these load-bearing is tokio's paused-clock
auto-advance: under `start_paused = true` the virtual clock only jumps
forward when the runtime has **no** task ready to poll. A loop that spun —
on `yield_now`, on a real-time deadline check, on `std::thread::sleep`, or
on any non-timer wait — would keep the runtime busy or block its only
thread, auto-advance would never fire, and the tests would hang rather
than complete. Completion is itself the proof; the assertions make the
failure loud instead of a silent hang.

These three are **evidentiary, not red-first**, and are labelled as such in
their own doc comments: `wait_or_cancel` already awaits `tokio::time::sleep`,
so there was no failing state to observe first. Their bite was instead
established by mutation, each mutation applied to production code
**temporarily and then reverted** (this commit changes no production code):

| Mutation | Result |
| --- | --- |
| `tokio::time::sleep(delay)` → `sleep(delay / 2)` | `virtual_time_advanced_equals_the_sum_of_the_announced_delays` FAILED; `cancel_inside_a_ceiling_saturated_wait_wakes_without_finishing_it` FAILED |
| `std::thread::sleep(Duration::from_millis(3))` added at the head of `wait_or_cancel` | `unbounded_retry_spends_virtual_hours_and_no_real_time` FAILED: *"a 18196.037s simulated outage burned 2.343651875s of real time — more than the shortest single wait in its own schedule (1s), so the wait is not parked"* |

Production source was restored bit-for-bit after each mutation and the
release binary was rebuilt from the restored source before the measurement
(sha256 recorded in §1).

---

## 11. Gates

| Gate | Exit code |
| --- | --- |
| `cargo fmt --check` | 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 |
| `cargo test -p norn --lib` | 0 — **4,465 passed, 0 failed, 0 ignored** |

## 12. Reproducing this

```
cargo build --release -p norn-cli --bin norn
./docs/design/retry-forever/harness/resource-whisper.sh \
    ./target/release/norn <FREE_PORT> <DURATION_S> <INTERVAL_S> <GRACE_S> <OUT_DIR>
```

The harness writes `meta.txt`, `samples.tsv`, `stdout.jsonl`, `stderr.log`
and `exit.txt` into `OUT_DIR`, and refuses to run (exit 69) if anything is
listening on the chosen port. It has no built-in duration, interval, grace
period, port or threshold — every value is supplied by the operator and
recorded in `meta.txt`, so a later run's parameters are never in doubt.
