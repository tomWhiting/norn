# Incident: context protections mis-armed by stale global override (2026-07-05)

Status: ROOT CAUSE REVISED AND CLOSED after deeper forensics; fix set re-scoped below.

> **CORRECTION (supersedes the first root-cause statement, same day):** my initial
> finding — "protections ship disarmed; nothing reads the catalog" — was TRUE ONLY
> BEFORE 2026-07-03. Commit da0825a ("auto-compaction armed by default for every
> agent") added exactly the catalog fill Tom asked for: `arm_auto_compaction`
> (crates/norn/src/agent/arming.rs:130-141) fills an unset window from
> `model_catalog::smallest_context_window_for_model` inside `AgentBuilder::build`,
> with `auto_compact_reserve_tokens` defaulting to Some(30_000) (loop/config.rs:327).
> The installed binary CONTAINS this (verified via da0825a-era marker strings in
> ~/.cargo/bin/norn). I reported the stale story to Tom and Vespa before finding the
> arming path; this section is the honest record of being wrong first.

## Actual root cause (verified, reconciles every observation)

`~/.norn/settings.json` carries a GLOBAL explicit window:

```json
{ "agent": { "context_window": 272000, "compact_threshold": 0.88 } }
```

- Settings are an explicit source, so the catalog fill correctly defers to them
  (arming runs "only when the merged window is still None" — builder.rs:480-485).
- The spark run therefore armed at 272,000 on a 128,000-token model: token_warning
  fires at effective > 272k, compaction at > 242k — both unreachable behind the real
  128k wall. Zero warnings + zero compaction + provider overflow: exact signature.
- Vespa's rerun passed `-c context_window=128000`, which outranks settings → correct
  limit → 2 compactions → survived the full 50-turn budget. Same binary, both
  outcomes explained; no stale-binary theory needed.
- `compact_threshold: 0.88` is a dead key (never shipped; loader warns-and-ignores).
  Tom's intended trigger (~0.88 of window) roughly matches the shipped default
  anyway (reserve 30k).

The irony worth recording: the global override predates armed-by-default and was
added to COMPENSATE for the missing protections; after da0825a it became the only
thing DEFEATING them. A per-model fact stored as a model-independent global is the
underlying defect.

## Incident

`norn --print … --model gpt-5.3-codex-spark` (repo-scout prompt, aion repo) died at
~2.5 min with stderr `norn: agent error: provider error: context window exceeded`,
exit 1, EMPTY stdout (no terminal envelope). Session file preserved:
`~/.norn/sessions/019f31c3-1da4-70d1-8e20-2533f4e044eb.jsonl` — 60 events, largest
ToolResult 58KB (no single giant read; steady accumulation), **zero `loop.token_warning`,
zero compaction events**. Write-through durability held.

## Root cause (verified against source at HEAD)

The context protections ship **disarmed on every norn CLI run, on every model**:

1. `AgentLoopConfig::default()` has `context_window_limit: None`
   (`crates/norn/src/loop/config.rs:326`).
2. The entire warning + auto-compaction machinery is gated on
   `if let Some(limit) = args.config.context_window_limit`
   (`crates/norn/src/loop/inflight_compaction.rs:155`).
3. The ONLY production writers of `context_window_limit` are explicit user config:
   `-c context_window=` (`crates/norn-cli/src/config/overrides.rs:179-181`) and settings
   `agent.context_window` (`overrides.rs:229-231`).
4. **Nothing at runtime reads the model catalog's window.** `assets/models.json` has
   gpt-5.3-codex-spark at `context_window: 128000` (entry correct), and
   `norn::model_catalog::smallest_context_window_for_model()` exists
   (`crates/norn/src/model_catalog.rs:153-160`) — no production caller wires it into
   the loop config.

Signature match: zero warnings + zero compaction = never armed (not miscalibrated).
Spark died first only because 128k is the smallest catalogued window; an equally long
gpt-5.5 run dies the same way at 272k. Embedder runs (meridian) are protected only
because the embedder passes `builder.context_window_limit` itself.

This is the same disease as the meridian context-selector finding (hardcoded 967,000):
the catalog holds the fact; the runtime never asks.

## Second bug (confirmed, separate fix — sketch ready, awaiting Tom's ruling)

Plain print mode emits NO terminal envelope on agent/provider errors:
`crates/norn-cli/src/print/orchestrator.rs:351` returns `Err(PrintError::Agent(...))`
→ stderr line + exit 1, stdout empty. The driven path's post-acceptance funnel wraps
this; plain `-f json`/`stream-json` has nothing. Fleet impact (per Vespa): shell-mode
consumers see error runs as "unparseable output" rather than a typed stop.

**Fix sketch (recon 2026-07-05, verified against source):**
- `StopInfo` (`print/output.rs:54-89`) has NO error variant — add
  `StopInfo::Error { message, class }` (`agent|auth|io|session`). Driven mode can't
  lend one: `finish_with_error` answers with a JSON-RPC error Response, not a
  StopInfo, so the variant is genuinely new.
- Writer mirrors the existing minimal-envelope precedent `write_handled_locally`
  (`print/step_output.rs:123`): `output: null`, default usage, error stop; for
  stream-json, terminal event via `emit_stream_completed`.
- Emit sites: the `run_agent_step` Err at `orchestrator.rs:529-533` (the named
  target) plus the pre-run bubbles (`:351`, `:358-363`, `:368`, `:434-440`) and
  `execute()`-level assembly failures (`:192`, `:194`). Driven untouched (no
  double-emit). Keep stderr line + non-zero exit unchanged.
- Precedent that consumers already branch on `stop.reason`, not envelope
  presence: `TimedOut`/`Cancelled`/`SchemaUnreachable` already emit envelopes
  with exit 1.

**Rulings needed from Tom (owner calls, not implementer guesses):**
1. `ENVELOPE_VERSION` stays 1 (additive reason) or bumps to 2.
2. Scope: `Agent`/`Session`/`Io` only, or also `Auth` (exit 3) and pre-assembly
   failures? (`Argument`/exit-2 should stay stderr-only — clap parity.)
3. Error envelope carries usage/events accumulated before failure, or stays minimal?
4. Do `renderer_failure`/`emitter_failure` get envelopes? Their rationale is
   "stdout already torn" — a clean envelope on a torn NDJSON stream may be worse.
5. Does `--output <path>` receive the error envelope too?

## Rulings so far (Tom, 2026-07-05)

- "The context window needs to be set by the model unless we want to do an override —
  fix properly." → catalog is the source; explicit config is override-only.
- Unknown model: "if it is an unknown model and we don't have it, just error out."
  (Local models exist but are not relevant to this concern per Tom.)

## Re-scoped fix set (catalog seeding already exists — these close what remains)

> **STATUS (2026-07-05, end of day):** items 1 and 2 IMPLEMENTED and LANDED —
> `validate_context_window` (`agent/arming.rs`) +
> `largest_max_context_window_for_model` (`model_catalog.rs`), wired into
> `AgentBuilder::build` after arming; both arms pinned through `build()` itself
> (incident repro: spark + explicit 272k errors naming model/272000/128000).
> Adversarial Fable review verdict READY; fmt/clippy/full norn + norn-cli suites
> green. One latent, accepted-and-documented limitation from review: the
> largest-max check is backend-blind ("at least one backend can honour it") —
> vacuous today (every catalog model id lives in exactly one backend) but it
> weakens silently the day a multi-backend model id lands; the
> `largest_max_context_window_for_model` docstring records the semantics, and
> catalog additions should re-check it. Items 3–4 still open (item 3 needs Tom's
> hand — his user file).

1. **Explicit-window sanity check against the catalog (the incident's actual fix).**
   At `AgentBuilder::build` (covers TUI, print, driven, and embedders in one place —
   Tom's all-invocation-methods requirement), when the resolved model IS in the
   catalog and an explicit `context_window_limit` EXCEEDS the model's
   `max_context_window`: hard error naming the model, the configured value, where a
   larger value can legitimately come from, and the catalog max. Validate against
   `max_context_window`, NOT `context_window` — gpt-5.4's standard window is 272k
   but max is 1M, and an explicit 1M there is legitimate; spark's max is 128k, so
   272000 errors out. Loud error, never a silent clamp — a clamp hides config drift
   (this exact incident would have become an invisible mystery).
2. **Unknown-model guard (Tom's ruling: error out).** After arming, catalog-missing
   model + still-None window = hard error at build: "check the model id for a typo"
   first (Tom: an unknown model probably means the wrong model code), explicit
   window config second. Replaces the current silent keep-None (arming.rs comment:
   "models absent from the catalog keep None, leaving the trigger disabled" — that
   is the silently-unprotected state Tom ruled against).
3. **Tom's `~/.norn/settings.json` cleanup (needs Tom, it's his user file):** delete
   `agent.context_window` (the catalog now owns it per model — keeping it globally
   re-creates this incident for every small-window model) and `agent.compact_threshold`
   (dead key; the shipped equivalents are `agent.auto_compact_reserve_tokens` /
   `agent.compact_keep_turns`, and the default reserve of 30k already approximates
   his 0.88 intent). No compat shim for the dead key (house rule).
4. Correct the stale `compact_threshold` row in norn-config DESIGN.md's mapping
   table (doc drift).

Noted, not scoped here: `loop.token_warning` only fires when the estimate already
EXCEEDS the full limit (inflight_compaction.rs:155-156) — it is a past-the-post
alarm, not an early warning; compaction at limit−reserve is the real guard. Worth
revisiting when the road-sign/annotation design lands.

### Coverage map

- `builder_from_cli` is the single production assembly funnel: TUI
  (`tui/driver.rs:118`) and print+driven (`print/orchestrator.rs:241`) both route
  through it. One insertion point covers all CLI modes.
- CHECK during implementation: `commands/slash/actions.rs:151` builds an
  `AgentBuilder` directly (mid-session rebuild path) — verify whether it assembles
  its own `AgentLoopConfig`; if so, seed+guard there too.
- Children (fork/spawn) inherit the parent's loop config today; a spawned child on a
  DIFFERENT model keeps the parent's window. Fold per-model re-seeding at child
  assembly into the `agent-variants` unit (variant model resolution, R3) — noted
  there rather than scope-creeping this fix.
- Embedders unaffected (they set the limit; norn 3cac008 gave them the setter).

### Test plan

- Catalog model, no config → limit == catalog window (spark 128000).
- Catalog default UNDER settings: `agent.context_window` wins over catalog.
- Catalog default UNDER `-c context_window=` wins over settings and catalog.
- Unknown model, no config → `BuildError::Argument` naming model + both keys.
- Unknown model + explicit window → assembles, limit == explicit value.
- Regression: the spark repro flags (`--model gpt-5.3-codex-spark`, no window config)
  now produce `token_warning`/compaction before any provider overflow (integration
  level if harness allows; unit level on the seeded config otherwise).

## Mitigation deployed meanwhile (Vespa, prospekt 702c4d48)

Dev-pipeline worker passes `-c context_window=<per-model>` on every invocation,
with the per-model constants documented next to the disarmed-default finding.

**EMPIRICALLY VERIFIED (Vespa, 2026-07-05 ~10:28Z):** the identical undisciplined
repo-scout prompt that died at ~2.5 min unarmed survived the FULL 50-turn budget with
`-c context_window=128000` — 2 compaction events in session 019f31cd-ee08…, ~3.5M input
tokens churned, terminal stop `max_iterations`, never hit the wall. The arming path
works as designed; the fix below makes it the default.

## Open footnotes from live verification (Vespa)

1. **Possible early wedge (LOW, unconfirmed — candidates now mapped):** a
   disciplined-prompt spark run (`--session-name content-hash-scout-spark-r2`, NO
   `-c` flag) was killed by an external 8-minute timeout having produced **no
   session artifacts at all — not even a session file**. Recon (2026-07-05, full
   startup-order walk) ranked the pre-session-open hang candidates:
   - **#1 — unbounded blocking stdin read.** `read_stdin_if_piped`
     (`print/orchestrator.rs:274`) does a synchronous `read_to_string` on stdin
     before any assembly, gated only on `is_terminal()` — it runs even when a
     positional prompt was supplied. An orchestrator that spawns norn with
     `Stdio::piped()` stdin and never writes/closes the pipe hangs here forever:
     no session file, no output, exact observed signature. Vespa's worker runs
     through an external harness, so this is the prime suspect — check how
     prospekt wires the child's stdin.
   - **#2 — indefinite session-index flock.** `builder_from_cli` never calls
     `with_index_lock_deadline` (default `None` → `file.lock()` blocks forever on
     `index.lock`, `session/persistence/lock.rs:96`). A wedged concurrent norn
     process blocks session open indefinitely. A bounded path
     (`lock_with_deadline` + `IndexLockTimeout`) exists but the CLI never opts in.
   - Ruled out with evidence: OAuth construction is disk-only with a
     whole-request-timeout client (refresh happens per-request, after open);
     `--extension` URIs are validated but never connected on this path; LSP
     backend construction is lazy. **No CLI flag bounds the pre-session-open
     window** — `--timeout` starts at the agent loop, the provider timeout at the
     first request.
   Both candidates are real defects regardless of which one bit Vespa (the stdin
   read wants a positional-prompt gate or a bound; the CLI should wire the index
   lock deadline). Not started — flagged for a ruling on scope/priority.

   **RE-RANKED after Vespa's spawn-site audit (2026-07-05, clean negative on
   #1):** every `norn --print` spawn site in their harnesses wires stdin to
   null, not a dangling pipe — the pilot worker
   (`meridian_dev_pipeline/worker/src/shell.rs:134`) uses `Command::output()`,
   whose contract is that stdin is NOT inherited (child reads EOF instantly),
   and `norn-fan-worker` sets `Stdio::null()` explicitly. The gratuitous
   pre-assembly read still executes (null isn't a terminal) but returns
   immediately — a landmine for future piping harnesses, not this wedge.
   **The flock is now the lead suspect**: the doctrine pilot runs multiple
   CONCURRENT norns by design, so a slow sibling holding `index.lock` blocks
   session open indefinitely — no file, no output, silent 8-minute wedge.
   Fix shape when ruled: wire `with_index_lock_deadline` in `builder_from_cli`
   and let `IndexLockTimeout` surface as a typed error. The deadline VALUE
   needs an owner ruling or a settings key (house rule: no invented numbers);
   the stdin read demotes to hygiene (bound it or gate it on
   no-positional-prompt).
2. **Stale settings key in Tom's ~/.norn/settings.json:** `agent.compact_threshold`
   — that key never shipped. The shipped compaction keys are
   `agent.auto_compact_reserve_tokens` (reserve size or `"off"`) and
   `agent.compact_keep_turns` (config/types.rs:439-450); the loader warns-and-ignores
   the unknown path on every run (loader.rs:100-104). Correct fix: Tom's settings file
   updates to the real key — NO compat shim (house rule). The old `compact_threshold`
   name also lingers in the norn-config DESIGN.md mapping table — doc drift, correct
   in passing.
