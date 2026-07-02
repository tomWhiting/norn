# Held for owner discussion (2026-07-02)

Two items from the 2026-07-02 review are deliberately **not** wired and **not** deleted in the
final-state hardening campaign. They are held pending a design discussion with the owner.
Nothing in the campaign may modify them.

## 1. `RunMonitored` — AI-monitored background tasks

**Where:** `crates/norn/src/agent/monitor.rs` (exported scaffolding, zero production callers,
unused `_provider` parameter, static-string heartbeat).

**Vision intent (VISION.md "AI-Monitored Background Tasks"):** long-running commands and
sub-agents watched by a cheap model instead of consuming the parent's context; the parent
queries the monitor for progress, alerts, and structured summaries. Presented in the vision as
the answer to "the fundamental problem with current sub-agent models" (block on the child, or
read all its output and lose the context you delegated to save).

**Options on the table:**
- **Wire it properly** — monitor model/config comes from the builder (no assumed default),
  heartbeat driven by real output analysis, a query interface on the handle, alert routing via
  the existing `MessageRouter`/pending-store machinery, audit reintegration into the parent
  session.
- **Delete until scheduled** — remove the module and re-introduce it when the feature is
  actually designed and briefed (the reviewer's recommendation).
- **Redesign first** — the wake/linger + `signal_agent` + delegation-budget machinery landed
  after this scaffolding was written; a monitored task may now be expressible as a persistent
  child + watch rules rather than a bespoke monitor type.

**Discussion points:** which monitoring model and who configures it; heartbeat cadence and
content; query interface shape (poll vs. push vs. both); relationship to `wake_agent`/linger;
whether bash background processes and sub-agents share one monitor abstraction.

## 2. `ToolEnvelope.runtime_inputs` + `ToolContext.runtime_args`

**Where:** `crates/norn/src/tool/envelope.rs` (`RuntimeInputs` always default, zero readers,
four unused support types) and `crates/norn/src/tool/context.rs` (`runtime_args` — no writer,
no reader).

**Vision intent (VISION.md "Tool Call Envelopes" / "Runtime-Supplied Tool Arguments"):** the
third envelope section — inbound messages, diagnostics, filesystem changes, working-tree
notifications accumulated since the last tool boundary — delivered to the model at each tool
call without explicit conversation injection; plus policy arguments (path constraints, length
limits, timeouts) injected by the runtime rather than the model.

**Why held rather than deleted:** the inbound-channel + `MessageRouter` + rules-engine work
that landed since the vision was written overlaps heavily with this design. Whether boundary
signals should ride the envelope (as designed) or the now-existing message-injection paths is
a real architectural fork that deserves a decision, not a default.

**Discussion points:** which signals belong in the envelope vs. injected messages; whether
profiles supply a metadata schema for it; how `runtime_args` interacts with the (now enforced)
permission policy and per-tool config; whether the envelope section is model-visible,
orchestrator-only, or both.
