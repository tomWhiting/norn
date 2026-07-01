# Option B — Worker-Kill (kill -9 mid-Norn-step) Durability: Engineering Scoping

**Date:** 2026-06-29
**Author:** scoping pass (code-verified, not review-trusted)
**Scope:** Harden the Aion↔Norn boundary so an Aion worker killed with `kill -9`
**mid-Norn-agent-step**, on restart/redial, resumes the same Norn session and the
durable workflow completes **exactly-once with the correct AI result**.

This is the *deeper* durability story beyond node-kill failover (which already
works at the Aion engine level). Here the unit that dies is the **worker process
that owns the running Norn subprocess** — so the in-flight LLM step, its
partially-written session log, and the activity's success/failure classification
are all at risk in ways node-failover does not exercise.

---

## 0. Method and headline result

I read `PLAN.md`, `REVIEW.md`, `CLAUDE.md`, and `MERIDIAN-HANDOFF.md`, then
**verified every claim in the brief against the current source** (Norn `main`,
Aion `examples/stacked-dev/norn-worker`). I did **not** trust the review.

**Headline: most of the forensic findings the brief asked me to scope are
already fixed on Norn `main`.** The Phase-0/1 hardening and the Phase-2 typed API
described in `MERIDIAN-HANDOFF.md` landed. Specifically:

| Brief item | Review claim | Verified state today | Evidence |
|---|---|---|---|
| **H19** torn-file / strict reader | Open | **FIXED** | `session/persistence/io.rs:94-125` (torn-line heal on reopen), `:153-229` (tolerant reader, skip+count+warn, dup-EventId skip); `session/store.rs:288-302` (in-lifetime tear flag) |
| **H18** index race | Open | **FIXED** | `session/persistence/lock.rs` (flock on `index.lock`); `session/persistence/index.rs:137-244` (all mutations hold the lock; atomic tmp+fsync+rename) |
| **H4** provider timeout no-op | Open | **FIXED** | `provider/openai/provider.rs:84,124-126` (`connect_timeout`); `provider/openai/execute.rs:94,234` (per-chunk SSE stall deadline from `ProviderConfig.timeout`) |
| **H6** OAuth single-flight + atomic auth.json | Open | **FIXED** | `provider/openai_oauth/manager.rs:38,157` (`refresh_gate` mutex); `storage.rs:59-75` (tmp + fsync + atomic rename) |
| **Typed stop-reason** | Open | **PARTIALLY fixed in Norn; UNCONSUMED by Aion** | `agent_loop/config::AgentStepResult` is fully typed; CLI maps non-completion → non-zero exit (`print/orchestrator.rs:444-456`); **but the JSON envelope carries only a coarse `result` string label and Aion's worker ignores both** (see Gap A) |

So Option B is **not** "re-fix H19/H18/H4/H6." Those are done. Option B is now a
**narrower, mostly Aion-side integration problem** with three real remaining
gaps plus a power-loss caveat. That is the honest finding and it makes Option B
substantially cheaper than the brief assumed.

The DoD-blocking work that genuinely remains:

- **Gap A (Aion side, the real blocker):** the worker shells Norn as a CLI and
  classifies purely on `exit_status == 0` + a "does stdout parse as my report
  shape" check. A non-completion (MaxIterations/Timeout/SchemaUnreachable) is
  turned into a **terminal** activity failure with the typed reason discarded;
  there is no retryable-vs-terminal distinction and no honoring of Norn's
  partial output. Worse, after a worker `kill -9` the activity **re-runs from
  scratch** and (because the dev/scout/review handlers do *not* pass
  `--resume-if-exists` consistently, and `dev_resume` uses `--resume` which hard-
  errors if the prior attempt never created the session) idempotent resume is
  not guaranteed.
- **Gap B (Aion side):** non-idempotent / inconsistent session-id strategy
  across attempts and a `--resume` that fails when the first attempt died before
  the session file existed.
- **Gap C (Norn side, small):** the `--print` JSON envelope exposes the
  stop-reason only as a lossy `&'static str` (`result`), with `output: None` for
  non-completions — there is no machine-stable typed envelope (reason + partial +
  usage + a schema version) for a subprocess consumer to branch on. Aion can
  *almost* get by on exit code + `result`, but "exactly-once correct result"
  needs the typed reason and the partial.
- **Caveat D (durability ceiling):** print-mode hard-codes
  `DurabilityPolicy::Flush` (`print/session.rs:97,99,108`). That survives
  process `kill -9` (the bytes are in the OS page cache, handed off via
  `write(2)`) — which is exactly the Option-B scenario — but does **not** survive
  OS crash / power loss. If "worker kill" ever means "the box lost power," the
  last events of the step can be lost. This needs a conscious decision, not a
  silent default.

---

## 1. The scenario, precisely

```
Aion engine ──schedules activity──▶ norn-worker process (Rust)
                                       │  handlers.rs::dev() etc.
                                       │  shell.run("norn", [...])  ← blocking
                                       ▼
                                    norn --print --session-id <branch>
                                       --resume-if-exists --output-format json
                                       │  run_agent_step (one LLM turn, many tool calls)
                                       │  JsonlSink write-through to ~/.norn/<id>.jsonl
                                       ▼
                                    SSE stream from OpenAI ...
```

`kill -9` can land on **either** process. Two distinct sub-cases:

1. **Norn subprocess killed, worker survives.** `Command::output()` returns a
   `CliRun` with `exit_status = 128 + 9 = 137` (`shell.rs:169-176`). The worker
   sees non-zero → `require_run` → **terminal `ActivityFailure`**
   (`handlers.rs:681-700`). The half-written session file is healed/tolerated by
   Norn on the next open. **Recovery depends on Aion retrying with a resuming
   session id — which today it does not reliably do.**

2. **Worker process killed (with the Norn child).** On a default process group
   the SIGKILL to the worker does *not* propagate to the child, but when Aion
   restarts the activity on the same or another node, a fresh worker re-invokes
   Norn. The orphaned first Norn may still be writing. **This is the case that
   needs the inter-process index lock (FIXED, H18) and the
   duplicate-EventId-skip tolerant reader (FIXED, H19) — and an idempotent,
   resuming session id (Gap B).**

Both sub-cases reduce to the same three requirements:
**(R1)** the session log is never bricked by a torn write — *met by Norn today*;
**(R2)** re-invocation resumes the *same* session deterministically and
exactly-once — *Gap B*;
**(R3)** the activity result classification distinguishes "the AI finished and
produced the correct schema-valid result" from "the step stopped early / was
truncated / hit the iteration cap," and treats the latter as **retryable** where
appropriate rather than a terminal failure or, worse, a false success — *Gap A +
Gap C*.

---

## 2. Verification of the brief's enumerated items

### 2.1 H19 — torn-file session corruption — **FIXED**

**(a) Problem as stated:** `kill -9` mid-append leaves a partial JSONL line; the
strict reader hard-fails the whole session.

**(a) Current evidence — already resolved:**
- `open_session_append` (`session/persistence/io.rs:94-125`) detects a non-`\n`
  final byte on reopen and writes a lone `\n` to terminate the torn line, logging
  a `tracing::warn!`. Subsequent appends therefore never concatenate onto the
  torn bytes.
- `read_session_events` (`io.rs:153-229`) is fully tolerant: empty lines skipped;
  an optional `SessionFileHeader` first line populates `format_version`; a line
  that is not valid JSON **or** is valid JSON not matching `SessionEvent` (e.g.
  unknown variant from a newer writer) is **skipped + counted + warned**, not
  fatal; a duplicate `EventId` (crash-retry artifact) keeps the first and skips
  the rest. The function fails only on a whole-file open/stream I/O error.
- The write side mirrors this within a sink lifetime via a `needs_newline` tear
  flag (`session/store.rs:288-302`), and `EventStore::append` does not add the
  event to memory if the sink persist failed, so memory never over-claims
  durability (`store.rs:427-456`). There are regression tests:
  `torn_line_is_terminated_not_continued`, `sink_failure_surfaces_typed_error_and_retry_is_safe`.

**(b) Fix shape:** none required. The brief's three suggested shapes
(version-tolerant reader; skip-with-warning; atomic write+fsync+rename) are all
present (the index uses tmp+fsync+rename; the session file uses append + tear
heal + tolerant read, which is the correct shape for an append-only log).

**(c) Effort:** 0 (done). Add a **crash-injection integration test** that
actually truncates a real session file mid-line and asserts resume recovers all
prior events — see §6. ~0.5 day.

**(d) Risk/subtlety:** the only residual is the durability *level* (Caveat D):
`Flush` does not fsync the session file per event, so an OS crash can still lose
the tail. The tolerant reader cannot recover bytes the OS never wrote.

**(e) Dependencies:** none.

### 2.2 H18 — session index race — **FIXED**

**(a) Problem:** concurrent Norn processes do an unlocked read-modify-rewrite of
`index.jsonl` → dropped entries → resume `NotFound`.

**(a) Evidence — resolved:** `session/persistence/lock.rs` implements an advisory
`flock` on a *separate* `index.lock` file (separate so the atomic rename of
`index.jsonl` never swaps the locked inode). Every mutating entry point holds it:
`append_index_entry`, `insert_index_entry_if_absent`,
`insert_index_entry_for_new_session`, `update_index_entry`, `remove_index_entry`
(`index.rs:137-244,445-456`). Rewrites are tmp+fsync+rename (`write_index_atomic`,
`index.rs:76-93`). The idempotent create primitives
(`insert_index_entry_if_absent`, `insert_index_entry_for_new_session`) do the
existence check **and** the append under the same lock, so two racing creates
with the same id cannot both win.

**(b) Fix shape:** none required (the brief's "flock" option is exactly what
landed).

**(c) Effort:** 0 (done) + a multi-process race test in §6 (~0.5 day).

**(d) Subtlety:** `flock` is advisory and per-open-file-description; it correctly
excludes both other processes and other threads here. On network filesystems
`flock` semantics are weaker — `~/.norn` is assumed local (true for the
stacked-dev worker). Worth a one-line doc assertion.

**(e) Dependencies:** none.

### 2.3 H4 — provider timeout ignored — **FIXED**

**(a) Problem:** `ProviderConfig.timeout` parsed but never enforced → a stalled
SSE stream hangs the step forever.

**(a) Evidence — resolved:** `build_http_client(config.timeout)` sets
`connect_timeout` (`provider/openai/provider.rs:84,124-126`). The streaming
executor wraps **both** the initial `send()` and **each** `stream.next()` in
`tokio::time::timeout(self.timeout, …)` (`provider/openai/execute.rs:94,234`) —
an inactivity/stall deadline per chunk, classified as a retryable network
timeout (tests at `execute.rs:1097-1142`). Same shape in
`openai_compatible/provider.rs:72,143-145`.

**(b) Fix shape:** none required. One **design note**: this is a per-chunk stall
timeout, not a whole-request wall-clock cap (deliberate — streamed responses are
legitimately long). For Option B the relevant guarantee is "a *stalled* stream
cannot hang the worker indefinitely," which holds. A whole-step wall-clock bound
is `AgentLoopConfig.step_timeout` (surfaced as `AgentStepResult::TimedOut`), and
**that** is the one the Aion side currently does not set or honor (Gap A).

**(c) Effort:** 0 for H4 itself.

**(d) Subtlety:** the worker invokes Norn without `step_timeout`, so a *very*
long-but-not-stalled step has no upper bound from Norn. Aion's own activity
`start_to_close` timeout is the backstop, but on timeout Aion kills the worker
without Norn checkpointing a clean stop reason → falls back into Gap A.

**(e) Dependencies:** ties into Gap A (timeout classification).

### 2.4 Typed stop-reason gap — **PARTIAL (Norn) + UNCONSUMED (Aion)** → Gap A & C

**(a) Problem:** agent runs that hit MaxIterations/Cancelled/Timeout/
SchemaUnreachable can be reported as success → Aion durably records a
failure-that-looks-like-success.

**(a) Evidence — nuanced:**
- **Inside Norn the type is honest.** `AgentStepResult` has the full variant set:
  `Completed | SchemaUnreachable | MaxIterationsReached | TimedOut | Cancelled |
  Truncated` (used throughout `print/orchestrator.rs:444-456`,
  `print/output.rs:39-75`). `MERIDIAN-HANDOFF.md §7.1` documents the embedded
  `RunOutcome::{Completed|Stopped{reason,partial}}` typed API.
- **The CLI exit code is honest.** `print/orchestrator.rs:444-456` maps
  `Completed → ExitCode::Success` and **every** non-completion (including
  `Cancelled` and `Truncated`) → `ExitCode::AgentError` (1). So a stopped run
  cannot exit 0. Good.
- **The CLI JSON envelope is lossy.** `JsonEnvelope` (`print/output.rs:175-191`)
  carries `result: &'static str` — one of `completed | schema_unreachable |
  max_iterations | timed_out | cancelled | truncated` (`result_label`,
  `output.rs:39-48`) — plus `output: Option<Value>` (the partial, via
  `extract_output_and_usage`, `output.rs:54-75`) and `usage`. There is **no
  structured stop-reason object, no schema/envelope version field, and no
  retryable hint.** It is *almost* enough (the label distinguishes the variants),
  but it is a stringly-typed contract with no versioning.
- **Aion does not consume any of it.** `handlers.rs` runs Norn through
  `require_run` (`handlers.rs:177-198,681-700`), which treats **any** non-zero
  exit as a **terminal** `ActivityFailure`. The envelope is then parsed by
  `parse_report` (`handlers.rs:712-744`), which only ever looks for the report
  under the bare shape or `{"output": …}` and **never reads `result`**. So:
  - A `MaxIterationsReached` run (genuinely retryable — give it another turn
    budget) becomes a **terminal** failure: the workflow dies instead of
    retrying.
  - A `Truncated`/`TimedOut` run with a usable partial is discarded.
  - Conversely there is no path by which a non-completion is recorded as
    success — so the brief's worst case ("failure that looks like success") is
    **not currently reachable through the CLI path** (exit code guards it). The
    real bug is the *inverse*: retryable stops are misclassified as terminal, and
    partials are thrown away.

**(b) Fix shape — two coordinated pieces:**

- **Gap C (Norn):** add a typed, versioned stop-reason to the `--print` JSON
  envelope. Extend `JsonEnvelope` with a structured field, e.g.
  ```json
  {
    "envelope_version": 1,
    "result": "max_iterations",
    "stop": { "reason": "max_iterations", "retryable": true },
    "output": null,
    "partial": { ... },          // partial text/output if any
    "usage": { ... },
    "session_id": "...",
    "model": "..."
  }
  ```
  `result` stays for back-compat; `stop` + `envelope_version` are the
  machine-stable contract. The `retryable` bit is derived from the variant
  (`MaxIterationsReached`/`TimedOut` → retryable; `SchemaUnreachable` →
  terminal-ish but configurable; `Cancelled` → neither, it was deliberate;
  `Truncated{MaxTokens}` → retryable with more budget, `Truncated{ContentFilter}`
  → terminal). This is a small, additive change in `print/output.rs`
  (`JsonEnvelope` + the `StepOutput`→envelope mapping in `orchestrator.rs:637-655`)
  plus a serde-stable enum mirroring `AgentStopReason`. Honors CLAUDE.md "no
  silent failures": the consumer can no longer accidentally ignore a stop.

- **Gap A (Aion):** stop using `require_run` for Norn. Add a Norn-specific
  classifier that:
  1. parses the envelope **first** (even on non-zero exit, since Norn prints a
     valid envelope before exiting non-zero on a stop);
  2. branches on `stop.reason` / `stop.retryable`:
     `Completed` → success; `MaxIterations`/`TimedOut`/`Truncated{MaxTokens}` →
     **retryable** `ActivityFailure` (Aion retries, and because the session
     resumes, the next attempt continues where it stopped);
     `SchemaUnreachable`/`ContentFilter` → terminal (or bounded-retry by
     policy); `Cancelled` → propagate cancellation, not a failure;
  3. only falls back to "unparseable terminal failure" when there is truly no
     envelope (the existing exit≥1 + no-JSON case — a real broken environment).

**(c) Effort:**
- Gap C (Norn envelope): **0.5–1 day** (additive serde type + mapping + envelope
  test + a `MERIDIAN-HANDOFF` note). Low risk.
- Gap A (Aion classifier): **1.5–2.5 days** including rewriting `dev`/`scout`/
  `dev_review`/`dev_resume` to use the classifier, threading the retryable
  distinction into `ActivityFailure`, and unit tests with fake-CLI shims that
  emit each envelope shape (the existing `tests/wire_compat.rs` harness already
  shells fake CLIs).

**(d) Risk/subtlety:** the retryability policy is a *product* decision, not a
mechanical one — e.g. should `MaxIterations` retry with the **same** budget
(pointless — it will stop again at the same place unless the session resumes and
makes progress) or a **larger** one? Because the session resumes, a retry that
keeps the same budget *does* make progress (it gets N more iterations on top of
the persisted history). That must be deliberate. Also: Aion's own
`start_to_close` activity timeout vs Norn's `step_timeout` must be ordered so
Norn times out and prints a clean `timed_out` envelope *before* Aion force-kills
the worker; otherwise we lose the typed reason. Recommend setting Norn
`--step-timeout` to ~80% of the Aion activity timeout (needs a `--step-timeout`
CLI flag if one does not exist — verify; `AgentLoopConfig.step_timeout` exists in
the lib).

**(e) Dependencies:** Gap A depends on Gap C (the typed envelope) for the clean
version, though a stop-gap Gap-A-only fix can read the existing `result` string.
Recommend doing C then A.

### 2.5 Other kill-9-mid-step hazards found

- **Cache / partial tool-call state:** *no torn-write hazard found.* Tool results
  and assistant messages are persisted as discrete `SessionEvent`s through the
  same write-through `JsonlSink`; a kill between events loses at most the
  in-flight event (tolerated on resume), and a kill mid-event leaves a torn line
  (healed). The OpenAI `previous_response_id` / `cache_key` threading
  (`orchestrator.rs:239`) is keyed to the session id, so resume re-threads
  correctly. **No action.**
- **H6 auth.json refresh race:** **FIXED** (single-flight `refresh_gate` mutex,
  atomic auth.json write — §0 table). The shared-with-Codex-CLI file is written
  tmp+fsync+rename. **No action.**
- **Signal handling in Norn:** Norn installs no SIGTERM/SIGKILL handler for
  graceful checkpoint (SIGKILL can't be caught anyway). Because durability is
  write-through per event, this is acceptable — there is no "flush on shutdown"
  obligation. A SIGTERM handler that calls `checkpoint()` would only tidy the
  *index* (the events are already durable), and the index self-heals on resume
  anyway. **Optional, low value (~0.5 day if wanted).**
- **Orphaned Norn child after worker kill (sub-case 2):** if the worker dies but
  its Norn child keeps running and writing while a new worker starts a *second*
  Norn on the same session id, two writers append to one file. The flock protects
  the **index**; the **session file** is append-only and the tolerant reader
  skips duplicate EventIds, so the file does not brick — but interleaved events
  from two live writers on one session is semantically messy. Mitigations:
  (i) Aion should kill the Norn child group when it kills/retries a worker
  (spawn Norn in its own process group and `killpg`), or (ii) accept that the
  orphan will finish or error and the resume reconciles. **Recommend (i): a
  process-group spawn + kill in the worker's shell layer** (~1 day) — this is the
  one genuinely new Aion-side robustness item beyond classification.
- **`dev_resume` uses `--resume` (not `--resume-if-exists`):**
  `handlers.rs:298-308`. If the original `dev` attempt was killed *before* it
  created the session file (e.g. killed during provider connect), the resume
  target does not exist and `--resume` hard-errors → terminal failure. This is a
  real exactly-once hole. **Fix:** make every resuming invocation use
  `--resume-if-exists` against a **deterministic** id (Gap B). ~0.5 day, folded
  into Gap B.

---

## 3. The Aion side, mapped

**Norn is NOT a pinned dependency of Aion.** It is a CLI resolved on `PATH`
(`shell.rs:111-162`, `find_executable`). There is no Cargo dependency on the
`norn` crate anywhere in `aion/` (verified: no `norn` in workspace or
`norn-worker/Cargo.toml` deps). **Consequence: there is no "pin bump" — the
contract between Aion and Norn is the CLI arg surface + the `--print` JSON
envelope + the exit code.** This is good for Option B: the Norn-side change
(Gap C) is purely additive to the envelope, and Aion adapts by reading new
fields. No version coordination beyond "deploy a Norn that emits
`envelope_version` before deploying the Aion classifier that requires it" —
and the classifier should tolerate its absence (treat missing `stop` as "infer
from exit code + `result`").

**How `handlers.rs` classifies today:**
- All Norn calls go through `require_run` (`handlers.rs:681-700`): `Ok && exit==0`
  → proceed; `Ok && exit!=0` → `ActivityFailure::terminal(... exit status N ...)`;
  spawn failure → `ActivityFailure::terminal`. **No retryable failures are ever
  produced for Norn.**
- Output is decoded by `parse_report` (`handlers.rs:712-744`): try bare `T`, then
  `{"output": T}`, then first-JSON-line; else terminal "unparseable". **`result`,
  `usage`, `session_id`, `events` are all ignored.**
- Session ids: `dev` uses `input.workspace.branch` (`handlers.rs:170`); `scout`
  uses `{branch}-scout`; `dev_review` uses `{branch}-review`; `dev_resume` uses a
  caller-supplied `session_id`. `dev`/`scout`/`dev_review` pass
  `--resume-if-exists` (good, idempotent), **but `dev_resume` passes `--resume`**
  (not idempotent — §2.5). There is a stale helper doc-comment at
  `handlers.rs:674` referencing an `-attempt-N` session-id search that the code
  no longer does — dead guidance to clean up.

**What must change on the Aion side for DoD:**
1. Replace `require_run` for the four Norn calls with a `run_norn` +
   `classify_norn(envelope, exit)` that returns retryable vs terminal vs success
   and surfaces the partial. (Gap A)
2. Make every Norn invocation idempotently resumable against a **deterministic**
   session id derived from `(workflow_id, activity_role)` and always use
   `--resume-if-exists` (never bare `--resume`). (Gap B)
3. Spawn Norn in its own process group and kill the group on
   worker-kill/retry/cancel to avoid orphan double-writers. (§2.5)
4. Order the Aion activity timeout strictly *after* Norn's `--step-timeout` so a
   slow step yields a typed `timed_out` envelope, not an opaque worker kill.

---

## 4. Gap table (the only DoD-blocking work)

| Gap | Side | Problem (evidence) | Fix shape | Effort | Risk | Order |
|---|---|---|---|---|---|---|
| **A** | Aion | `require_run` makes every non-zero Norn exit terminal; `parse_report` ignores `result`/`usage`; retryable stops die, partials lost (`handlers.rs:681-700,712-744`) | `classify_norn`: parse envelope first, branch on typed stop, retryable vs terminal vs success | 1.5–2.5 d | Retry policy is a product call | after C |
| **B** | Aion | non-idempotent resume: `dev_resume` uses `--resume` which hard-errors if first attempt never created the session (`handlers.rs:298-308`); session-id strategy not uniformly deterministic | deterministic `(workflow,activity)` id; `--resume-if-exists` everywhere | 0.5–1 d | id-collision charset (Norn validates ids) | parallel w/ A |
| **C** | Norn | `--print` envelope exposes stop only as lossy `result` string, no typed reason/partial/version (`print/output.rs:175-191`) | additive `envelope_version` + `stop{reason,retryable}` + `partial` on `JsonEnvelope` | 0.5–1 d | serde stability; keep `result` | first |
| **D** | Norn (policy) | print-mode hardcodes `DurabilityPolicy::Flush` — survives kill -9, NOT power loss (`print/session.rs:97,99,108`) | optional `--durability fsync\|fsync-every:N`; default decision | 0.5–1 d | per-event fsync latency cost | optional |
| **E** | Aion | orphan Norn double-writer after worker kill (no process-group kill) | spawn Norn in own pgid; `killpg` on retry/cancel | 1 d | signal plumbing | after A/B |
| **F** | both | crash-injection + multi-process race test harness (none today exercises kill-mid-step end-to-end) | see §6 | 1.5–2 d | flaky timing | last |

**Total DoD-critical (A+B+C+E+F): ~5.5–8.5 engineer-days.**
**With the optional power-loss hardening (D): ~6–9.5 days.**

This is far below a "re-fix the forensic findings" estimate because H19/H18/H4/H6
are already done. The honest framing: **Option B is now an integration +
classification job at the Aion↔Norn CLI boundary, plus one small additive Norn
envelope change, plus tests.**

---

## 5. Ordered Phase-1 plan

1. **C — Typed envelope (Norn).** Add `envelope_version` + `stop{reason,
   retryable}` + `partial` to the `--print` JSON envelope; keep `result` and the
   non-zero-exit-on-stop behavior. Serde-stable enum mirroring `AgentStopReason`.
   Tests: one per variant asserting the envelope shape and that exit code matches.
   Note it in `MERIDIAN-HANDOFF.md`. *(0.5–1 d)*
2. **B — Idempotent resume (Aion).** Deterministic session id per
   `(workflow_id, activity_role)`; `--resume-if-exists` on every invocation incl.
   `dev_resume`; delete the stale `-attempt-N` doc-comment. Add `--step-timeout`
   to every Norn invocation set below the Aion activity timeout. *(0.5–1 d)*
3. **A — Classifier (Aion).** `run_norn` + `classify_norn`; replace `require_run`
   for the four Norn calls; map retryable stops to retryable `ActivityFailure`,
   terminal stops to terminal, `Cancelled` to cancellation; surface partials.
   Fake-CLI shim tests for each envelope. *(1.5–2.5 d)*
4. **E — Orphan containment (Aion).** Spawn Norn in its own process group; kill
   the group on retry/cancel/teardown. *(1 d)*
5. **F — Crash-injection harness (both).** §6. *(1.5–2 d)*
6. **D — (Optional) durability level (Norn).** `--durability` flag; decide the
   default for the worker path. *(0.5–1 d)*

Steps 1–3 are the critical path to DoD; 4 hardens sub-case 2; 5 proves it; 6 is
the power-loss upgrade if "worker kill" must include hardware loss.

---

## 6. Definition of Done + test harness

**Definition of Done:** *An Aion worker `kill -9`'d mid-Norn-step, on
restart/redial, resumes the same session and the workflow completes
exactly-once with the correct AI result.* Concretely:
- the durable workflow reaches a terminal **success** with a schema-valid result
  identical to the no-kill run (modulo legitimate LLM nondeterminism — assert on
  schema validity + a deterministic field, not byte-equality);
- the session file is loadable after the kill (no brick), prior events intact;
- no duplicate side effects (exactly-once): one landed result, one index entry,
  no double-counted usage;
- a non-completion stop is recorded with its **typed reason** and retried iff
  retryable; a deliberate `Cancelled` is not a failure.

**Crash-injection harness shape (the missing piece, Gap F):**

*Layer 1 — Norn unit/integration (in `norn` repo):*
- A test that writes a real session file, truncates it mid-final-line, then calls
  `read_session_events` and asserts all complete prior events load and
  `skipped_lines == 1`. (Codifies H19 end-to-end on a real file, not just the
  in-memory `TornWriter`.)
- A multi-process test (`std::process::Command` spawning the built `norn` twice,
  or two threads each taking `lock_index`) that hammers `--session-id X
  --resume-if-exists` concurrently and asserts the index ends with exactly one
  entry for X and no `NotFound`. (Codifies H18.)
- An envelope test: drive `run_async` (already public, `orchestrator.rs:96`) with
  a `MockProvider` scripted to exhaust iterations / time out, assert the JSON
  envelope carries `stop.reason` + `retryable` + non-zero exit.

*Layer 2 — Aion worker (in `aion/examples/stacked-dev/norn-worker/tests`):*
- Extend the existing fake-CLI-shim harness (`tests/wire_compat.rs`,
  `handlers_shims.rs`) with a `norn` shim that:
  (a) on first invocation **emits a partial envelope then `exit 137`** (simulates
      kill -9 of the Norn child), and
  (b) on second invocation (resume) reads a sentinel file proving the same
      `--session-id` was passed, then emits a `completed` envelope.
  Assert `classify_norn` retried (not terminal) and the workflow's final report
  is the completed one. This directly exercises sub-case 1 + Gap A + Gap B
  without a real LLM.

*Layer 3 — end-to-end (manual / CI-gated, optional but the demo Tom wants):*
- Run a real stacked-dev workflow against a live Norn+OpenAI; `kill -9` the
  worker mid-`dev`; let Aion restart it; assert the workflow lands the same brief
  with a schema-valid dev report and exactly one merge. This is the visceral
  "kill it, watch it recover, correct result" demo and reuses the Sydney-failover
  demo assets.

**Why the harness can be mostly LLM-free:** the durability and classification
logic is entirely deterministic given a scripted Norn envelope/exit; only Layer 3
needs a live model, and only to prove the seam end-to-end.

---

## 7. Open decisions for Tom (product calls, not mechanical)

1. **Retry semantics for `MaxIterations`/`TimedOut`:** retry with the same budget
   (relies on session resume making forward progress — recommended, it does) or a
   larger budget? And a max-retry ceiling before giving up terminally?
2. **`SchemaUnreachable` policy:** terminal, or bounded-retry (the model may
   produce a schema-valid result on a fresh attempt)? Recommend bounded-retry.
3. **Caveat D — does "worker kill" include power loss?** If yes, pay the
   per-event fsync cost (`--durability fsync`) for the worker path. If "kill -9 of
   a process on a healthy box" is the real requirement, `Flush` is already
   correct and free. Recommend: default stays `Flush`, expose the flag, and set
   `fsync` only if the deployment can lose power mid-run.
4. **Orphan containment (Gap E):** process-group kill in the worker (recommended,
   cheap) vs. relying on resume-reconciliation alone (simpler, slightly messier
   logs).

---

## 8. One-paragraph honest summary

The forensic findings the brief was built around (H19 torn file, H18 index race,
H4 provider timeout, H6 OAuth) are **already fixed on Norn `main`** and verified
in-code; Option B is therefore not a re-fix but an **Aion↔Norn CLI integration
job**: teach the worker to parse Norn's stop reason and retry resumable stops
against a deterministic, always-resuming session id (Gaps A+B), make Norn's
`--print` envelope expose a typed, versioned stop reason + partial (Gap C),
contain orphaned Norn children on worker kill (Gap E), and prove it with a
crash-injection harness (Gap F) — roughly **5.5–8.5 engineer-days**, plus an
optional power-loss durability upgrade (Caveat D, +0.5–1 day) if "worker kill"
must include hardware loss. Norn is a PATH CLI, not a pinned dep, so there is no
version coordination beyond deploying the additive envelope before the consumer
that reads it.
