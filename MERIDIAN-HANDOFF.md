# Meridian Handoff — norn Phase 0+1 hardening

**Date:** 2026-06-12
**Pin:** the current `main` head of `tomWhiting/norn`. `2955a27` is the code
commit; every commit after it to date is documentation-only (this file) and
code-identical, so pinning the head is equivalent and keeps this handoff in
the pinned tree.
**Status:** reviewed (two Fable rounds, final verdict READY), gates green —
clippy `--workspace -D warnings` clean, fmt clean, **2928 tests / 0 failures**.

This document is for the agent working on the meridian/yggdrasil side. It lists
what changed in norn that affects you, what you must adapt when you bump the
pin, what you can delete, and which meridian-side fixes can proceed now versus
which must wait. Background: `REVIEW.md` (verified findings) and `PLAN.md`
(phased plan, owner decisions) in this repo.

---

## 1. Update the pin

All four consumers (meridian-services, meridian-tools, meridian-aion,
meridian-vm-daemon) should move their git reference to the current `main`
head (≥ `2955a27`). The changes below are breaking; bump everything in one
pass.

## 2. Breaking API changes you must adapt to

| Old | New | Notes |
|---|---|---|
| `attach_sink(sink, dir, id)` (infallible) | `attach_sink(store, dir, id, DurabilityPolicy) -> Result<…>` | Use `DurabilityPolicy::Flush` for the historical behavior. It can now fail — handle it; there is no silent in-memory fallback anymore. |
| `read_session_events(..) -> Vec<Event>` | `-> SessionFileRead { events, skipped_lines }` | `skipped_lines` counts unparseable/torn/duplicate lines. Surface or log it — that's the point. |
| `SessionIndexEntry` | gains `format_version` | Match exhaustively / update constructors. |
| `from_auth_for_testing` | `from_static_auth` | Rename. **You currently call the old name in production code** — see §3, the right fix is deletion, not rename. |
| fork/spawn child results | non-`Completed` stop reasons now report as **failures** | Previously a truncated/stopped child looked successful. If you branch on child success, your logic gets stricter for free — verify your handling. |
| `ProviderConfig.timeout` | now actually enforced | If you set absurd values "because it was ignored," fix them. |
| Provider errors | new `ProviderError::Truncated { stop_reason, .. }` | Deterministic stops (max-tokens / content-filter). **Never retry it.** |
| Registry status transitions | typed `StatusTransitionError`; terminal states immutable | If you drive agent status, terminal→anything is now rejected. |

## 3. Code you can now DELETE on the meridian side

- **`NornSessionStore` (~540 LOC) and `reconcile_session_index`.** The session
  index is now self-maintaining: registered sinks batch index deltas and flush
  at `EventStore::checkpoint()` / drop; resume self-heals drifted entries;
  the reader is version-tolerant. Call `store.checkpoint()` at your turn
  boundaries (cheap; one locked index write) and delete your reconciliation.
- **`from_auth_for_testing` in production** — replace with real auth flow or
  `from_static_auth` where a static credential is genuinely intended.
- **Hand-rolled tolerant JSONL reading** — `read_session_events` is the
  tolerant reader now.

## 4. New capabilities you should adopt

- **`AgentBuilder::workspace_root(path)`** — real filesystem confinement for
  read/write/edit/patch/bash, **inherited by spawned/forked children** (children
  also snapshot the parent's working dir and inherit `PermissionPolicy`,
  `ToolEffectIndex`, and operator hooks). If meridian sandboxes agents, use
  this instead of convention.
- **`settings.permissions` is now enforced on the embedded path** (deny > ask >
  allow, shell-segmented matching). If you ship permission settings, they now
  actually do something — audit them before bumping, because rules that were
  silently dead will start blocking.
- **Headless reclamation:** `runtime_init::install_terminal_reclamation` is
  installed automatically by `AgentBuilder` assembly — naturally-completed
  child agents are reclaimed after result delivery instead of leaking
  registry entries/EventStores in long-running processes (this was a real
  leak for aion workers).
- **`rebuild_action_log` (now public) + `ActionLog::with_working_dir`** — if
  you resume sessions, rebuild the action ledger; meridian currently starts
  resumed sessions with an empty one.
- **LLM compaction summaries** — compaction now produces a provider-written
  summary (mechanical digest only as a marked fallback,
  `summary_kind: "mechanical_digest_fallback"`). No API change, but expect
  one extra provider call (usage-accounted) when compaction fires.
- **New provider knobs** (all optional): `rate_limit_interval` (default 60s),
  `retry_backoff` (default 1s), `retry_after_ceiling` (unset = honor server
  header as-is, saturated). Wired through settings and `-c` in the CLI;
  embedders set them on `ProviderConfig`.

## 5. Meridian-side fixes — go / wait

**GO NOW (independent of the pin):**
- **[critical] aion activities: check `stop_reason`** — stop reporting
  non-success as success (`activities/agent.rs:204-222`). Note: once you bump
  the pin, fork/spawn already report non-Completed as failure, but your
  activity-level check is still the authoritative guard.
- `futures::executor::block_on` in `dispatch` parking tokio workers.
- Nil-UUID caller identity for meridian tools in aion activities.
- No `cancel_token` / `step_timeout` on the aion path.
- Unbounded token-pool wait; activity `config` silently discarded on the norn
  route; non-idempotent `session_id` minting per retry attempt.
- God files (`assistant/service.rs` 1219, `norn_step.rs` 728, `catalog.rs`
  2139, `agent.rs` 575); hardcoded `gpt-5.5`; silent CWD-relative session
  fallback; unformatted `dispatcher.rs`.

**DO WITH THE PIN BUMP (same PR):**
- Session API call sites (§2 table), `NornSessionStore` deletion (§3),
  `checkpoint()` at turn boundaries, permission-settings audit (§4).

**WAIT FOR NORN PHASE 2 (do not build now):**
- **Retry classification.** Do NOT build retry logic on error-string prefixes
  (`retryable:`/`terminal:`). Phase 2 lands a typed error taxonomy
  (`is_retryable()` on `NornError`/`ProviderError`) and a typed
  `RunOutcome` — aion retry semantics should be built on those. This is the
  one hard cross-repo sequencing constraint.
- Typed subagent lifecycle events (you currently reverse-engineer untyped
  JSON — Phase 2 replaces that; don't invest further in the parser).
- `AgentHandle` run bundle / `SessionManager` (will delete more meridian
  boilerplate; coming in Phase 2).

## 6. Known gaps, recorded (don't rediscover these)

- Grandchild agents (spawned by spawned agents) have no result channel, so
  their registry entries persist until the parent child's handles drop —
  recorded for Phase 2 `AgentHandle` work.
- Hook envelopes receive an empty `ToolContext` (hooks can't read agent
  identity / working dir) — pre-existing, recorded as a future brief.
- `runner.rs` (~670 real LOC) is over the god-file cap by explicit owner
  deferral to the Phase 3 state-machine rebuild (see `PLAN.md`,
  review-round decisions).
- Bash confinement cannot stop `cd` escapes within a command's shell —
  documented limitation on `BashTool`; workspace_root confines the tool's
  declared paths, not arbitrary shell behavior.
