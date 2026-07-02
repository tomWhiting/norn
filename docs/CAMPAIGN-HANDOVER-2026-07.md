# Hardening-campaign handover — 2026-07-02

Audience: anyone building against norn while the `hardening/final-state`
branch is in flight — in particular a **driven-mode (`--protocol jsonrpc`)
consumer**. This says where the branch is right now, what is guaranteed to
change before it merges to `main`, and what you can safely build against
today.

Companion docs: `docs/design/norn-cli/DRIVEN-PROTOCOL.md` (the normative
protocol contract), `MERIDIAN-HANDOFF.md` §9 (embedder-facing changes),
`docs/DECISIONS-2026-07.md` (every decision made, sign-off pending),
`docs/design/norn/INTERNAL-AGENTS.md` (forward design for the formerly
held scaffolding, resolved 2026-07-03).

---

## 1. Where we are

Branch `hardening/final-state`, forked from `main` @ `4869ef6`. Five of six
waves are **committed and Fable-reviewed**; the sixth (final adversarial
review of the whole diff) is mid-flight.

| Wave | Commit | What landed |
|---|---|---|
| 1 | `27df51d` | Subsystem hardening: provider stream executor, structured error taxonomy, session resume via single-pass replay, search/LSP path confinement, message-loss windows closed, **driven mode formalized** (typed stop envelope + DRIVEN-PROTOCOL.md) |
| 2 | `5dd47f0` | Rules engine lifecycle made real (persisted `RuleInjection` events, presence rebuild, nested scanning) |
| 3 | `32fa720` (+`8ac4aad`, `9ac8186`) | **R1 assembly unification**: `AgentBuilder` is the only assembler; the CLI's parallel 2,900-line assembly stack is deleted; print/TUI/driven all build through `builder_from_cli` → the same path a library embedder uses. Working-dir-scoped `--resume`/`--fork`. Session hooks auto-fire from `Agent::run`. |
| 4 | `3c84682` | **R2 runner state machine** (`Gate → BuildRequest → CallProvider → Dispatch → ResolveStop`); deterministic tool ordering (system prompts and provider tool arrays are byte-stable → provider prompt caching works); repo-wide: no production file >500 LOC, all `mod.rs` pure |
| 5 | `35701cd` | Docs truth pass (VISION, IMPLEMENTATION-STATUS, DECISIONS, MERIDIAN-HANDOFF §9, README) |
| 6 | in flight | 8-dimension adversarial Fable review of the full diff (282 files, +31k/−18k) + fix round + merge |

Gate discipline throughout: `cargo fmt --check`, `clippy --workspace
--all-targets -D warnings`, `cargo test --workspace` all clean at every
commit; no `#[allow]` outside test code; no bypasses.

## 2. Driven mode: what you can build against today

The contract is `norn-driven/1` — `docs/design/norn-cli/DRIVEN-PROTOCOL.md`
is normative and current. Stable to build against now:

- **Transport**: stdin+stdout newline-delimited JSON-RPC 2.0; stderr is
  logs only; single serializing writer (frames never interleave).
- **Lifecycle**: one-shot — `initialize` (idempotent, any time) → one
  `run/execute {prompt}` → streamed `event/*` notifications → id-matched
  terminal Response carrying the **versioned stop envelope**
  (`envelope_version: 1`, tagged `stop.reason`, `output`, `usage`,
  `session_id`, `events`, `diagnostics`).
- **Interventions mid-run**: `intervene/injectMessage`
  (`normal`/`interrupt` priority) and `intervene/cancel`; everything else
  is capability-gated `-32601`. Cancel acks are advisory —
  `stop.reason` in the terminal response is always authoritative.
- **Guarantee**: every *accepted* `run/execute` is answered — errors come
  back as the id-matched error response, never EOF.

None of the Wave 6 findings touch the frame shapes, envelope, or method
set. **The protocol contract will not change before merge.**

### Driven-mode bug found and FIXED in Wave 6

- **`/exit` / `/quit` as the prompt violated the answer guarantee**
  (was: accepted but the process exited without the id-matched Response —
  the peer saw EOF). Fixed in the Wave 6 fix round: the locally-handled
  success Response (`result: null`) is always emitted before the exit is
  honoured, in driven and plain-JSON modes alike, pinned by real
  process-boundary regression tests
  (`crates/norn-cli/tests/jsonrpc_driven_mode.rs`).

### Practical notes for a driven-mode workflow consumer

- Gate on the `protocol` field of `initialize`, not the `jsonrpc` tag.
- The process is one-shot: one `run/execute` per spawn. Budget a process
  per step.
- `--no-session` skips persistence entirely; otherwise pass an explicit
  `--session-id` per step if you're correlating runs (empty `--resume`
  is now scoped to the **working directory's** latest session, not the
  globally-latest — changed in Wave 3, deliberate fix).
- Prompt caching now works across processes (Wave 4 made tool ordering
  deterministic) — same config → byte-identical system prompt.
- **Three CLI flags were broken at 35701cd and are FIXED in the Wave 6
  fix round** (regression-tested): `--workspace-root` (was silently
  ignored — confinement now validated and installed on the built agent's
  tool context), `--rules <file>` (was silently ignored — now loaded and
  merged with discovered rules, explicit file wins on rule-ID collision),
  and `--variables` (was hard-failing assembly on every persisted-session
  run — now applied to the store keyed with the resolved session id).
  Safe to use from the Wave 6 commit onward.

## 3. Wave 6 status (the last mile)

The final review is complete: 8 Fable reviewers over the full diff, every
finding adversarially verified by an independent Fable skeptic. The tools
and repo-compliance dimensions returned **zero findings**. Verdict:
**16 confirmed, 1 refuted** (an error-classification claim that turned out
to be a recorded decision). **All 16 are fixed with regression tests.**
A Fable re-review of the fix round then found 4 residual defects (a
rule-ID-collision precedence inversion in the `--rules` merge, a
step-timeout path that still dropped fired rule injections, one
regression test that didn't guard its fix, and one piece of zombie code);
all four are fixed and under final verification before merge.

Confirmed findings, all fixed (severity, one-line):

1. **HIGH** — `--workspace-root` silently ignored on the unified CLI path:
   file-tool confinement never installed (security regression vs main;
   the four confinement regression tests were deleted with the old stack).
2. **HIGH** — `--variables` hard-fails assembly on every persisted-session
   run (the CLI-built variable store carries a random session id that
   `build()` correctly rejects); only `--no-session` worked.
3. **HIGH** — `--rules <file>` silently ignored: user guardrail rules
   never loaded.
4. **HIGH** — driven-mode `/exit` prompt never answered (§2 above).
5. **HIGH** — permission pattern `"bash (rm *)"` (typo'd space) validates
   cleanly but compiles to a rule that can never match → the deny is
   silently inert.
6. **MEDIUM** — explicit `-c` values that equal the library default are
   reverted to the settings value in `merge_agent_config`.
7. **MEDIUM** — explicit `prompt_command_timeout`/`linger` dropped when
   `load_runtime_base` is set.
8. **MEDIUM** — partial failure in inbound-message injection silently
   drops acknowledged messages (bypasses the durable-requeue invariant).
9. **MEDIUM** — CLI/TUI drivers pass a borrowed tool executor and lose
   `Agent::run`'s spawned-parallel tool batches (latency divergence across
   the unified seam; results identical).
10. **MEDIUM** — every timed-out session-index lock acquisition leaks a
    thread+FD blocked in `flock` (unbounded under a wedged holder).
11. **MEDIUM** — Chat Completions usage silently lost when the usage chunk
    arrives after the `finish_reason` chunk (the OpenAI-documented order).
12. **MEDIUM** — encrypted-reasoning replay is a silent no-op in the
    default configuration (include gated on the wrong condition).
13. **MEDIUM** — derive-macro rename rule diverges from serde for acronym
    idents (`HTTPRequest`): generated schema advertises names the
    deserializer rejects.
14. **LOW** — `--extension` URI validation lost (empty/whitespace accepted).
15. **LOW** — narrow race can land a session-file header at line 2, making
    it a permanently-skipped corrupt line.
16. **LOW** — fired Before-timing rule injections silently dropped when a
    step terminates at the gate (max-iterations/cancel).

## 4. Where we'll be when it's done

By merge to `main`, in order:

1. Fix round completes (in flight — five parallel agents, strict file
   ownership, every fix with a regression test); full gates.
2. Fable re-review of the fixes; commit.
3. Merge `hardening/final-state` → `main` (`--no-ff`).

End state: one assembler for every entry point, an explicit runner state
machine, deterministic prompts, a formalized driven protocol with its
answer-guarantee bug fixed, zero clippy/LOC/purity violations, and every
claim in the docs verified against the code.

**Not in this campaign** (deferred, tracked): meridian-side adoption PR
(deleting its session-store copy, adopting `.open_session`); a builder
setter accepting a prebuilt runtime base (identified as the missing piece
for per-execution runtime-base caching). The two formerly HELD scaffolding
items (`RunMonitored`, envelope `runtime_inputs`, `runtime_args`) were
resolved post-campaign (2026-07-03): owner-ruled **deleted**, with the
forward design captured in `docs/design/norn/INTERNAL-AGENTS.md`.

## 5. Needs owner sign-off (none block the merge)

- ~~`docs/DECISIONS-2026-07.md` §0 top items~~ — all three RESOLVED by
  owner (2026-07-02/03): no default step timeout (graceful redesign
  dropped), `require_git(false)` ratified, binary silent-skip ratified.
  A fourth §0 entry records the zero-tool-agent decision.
- `docs/design/norn/R1-DECISIONS-RESOLVED.md` D1–D7 (applied as
  recommended defaults; D1 session-hook ownership and D6 meridian scope
  most worth a look).
- ~~License contradiction~~ — RESOLVED by owner (2026-07-03): workspace
  `Cargo.toml` now declares `AGPL-3.0-only`, matching the `LICENSE` file.
- Diagnostics + CONVENTIONS.toml discussion (requested, queued for after
  the campaign).
