# Prompt-Cache Invalidation in the Per-Iteration Prompt

**Status:** implemented 2026-07-09. The managed dynamic-context Developer
message is now attached at the tail of every request (after history, after the
new user input) and re-synced fresh each iteration; the System message plus
history form one stable, fully-cacheable prefix. See
`crates/norn/src/loop/dev_context.rs` and `runner/prompt.rs::build_request`.
**Author:** Frodo (design + source verification). **Date:** 2026-07-09.
**Provenance:** surfaced by an external adopter (brup/ANKS, running norn headless via
aion stacked-dev, gpt-5.5 / ChatGPT-auth) with measured token telemetry; root cause
confirmed against norn `main` HEAD by reading the assembly path directly.

---

## Summary

norn re-syncs a single managed **dynamic-context Developer message** at **`messages[1]`**
(i.e. *ahead of the conversation history*) on every loop iteration. That message contains
a `# Environment` section whose `Time:` field is a second-resolution wall-clock stamp, so
its bytes change **every turn**. Because the provider prefix cache extends
`messages[0] → messages[1] → messages[2..]` in order, a change at `messages[1]`
invalidates the cache for *everything after it* — i.e. the entire growing history. The
cacheable prefix collapses to just the static System message; all of history is re-billed
uncached, every turn.

**Measured impact (ANKS, ~1 week):** ~554M input tokens across 147 sessions, prompt-cache
hit rate ~13% (agent loops normally sit >90%), output only ~0.5% of input (so it is *not*
reasoning cost). `cache_read` pinned at the static system+tools prefix (~15.5k tokens) for
entire 150-turn sessions while input climbed to ~163k/turn. Estimated amplification vs. a
stable prefix: **~40×**. This affects everything that runs agents through norn — the
interactive dev loop and the aion dispatch / stacked-dev workflows alike.

This is **not fundamentally an "environment" bug.** The timestamp is merely the field that
changes most reliably. Any dynamic section in that message (collaboration mode, context
rules, prompt-command output) busts the same cache whenever *it* changes. The correct fix
addresses the whole class by fixing **position**, not any single field.

---

## Full accounting — what goes into the prompt each turn

Per-iteration message layout sent to the provider (assembled in
`crates/norn/src/loop/runner/prompt.rs::build_request`):

| # | Message | Contents | Volatility | Cache effect |
|---|---------|----------|------------|--------------|
| `messages[0]` | **System** | base prompt + profile instructions + always-on `NORN.md` + `# Available Skills` catalog (`base_prefix` + loader + `base_suffix`) | **Static.** Rebuilt only when `NORN.md` changes (`refresh_context_if_stale` → `rebuild_base_section`). Deliberately kept byte-stable and *not* variable-expanded, explicitly "for prefix caching." | Correct — cacheable. |
| `messages[1]` | **Managed dynamic-context Developer message** (`dev_message.sync(dynamic_context())`) | Concatenation of all dynamic sections, re-synced every iteration: `# Environment`; `# Collaboration Mode`; materialized SystemContextAppend rules; hosted-tools prompt section; prompt-command stdout; Before-timing rule injections. | **Volatile.** `# Environment` `Time:` changes **every turn**; git branch / cwd change occasionally; mode / rules / prompt-commands change intermittently. | **The bug.** Sits ahead of history; any change invalidates the cache for all of history. |
| `messages[2..]` | **Conversation history** | user / assistant / tool messages; compaction-summary Developer messages (which live here permanently and are never overwritten). | Append-only. | Would be cacheable *if nothing before it changed* — but `messages[1]` always does. |

Dynamic-section contents of `messages[1]`, with the specific volatility:

- **`# Environment`** (`system_prompt/environment.rs::format_environment_section`): Working
  directory, Platform, Shell, **Time (`Utc::now()`, second resolution — `environment.rs:53`)**,
  Git branch (read from `.git/HEAD` each call), Session id, Model. → time busts every turn;
  branch/cwd bust on switch/`cd`.
- **`# Collaboration Mode`** (`inject_collaboration_mode`): changeable mid-session.
- **Materialized context rules** (`materialize_system_context_rules`): re-derived from
  persisted `RuleInjection` events every iteration; change as rules fire / compact out.
- **Hosted-tools section** (`hosted_tools_prompt_section`): recomputed each iteration from
  live provider capabilities; effectively static unless the provider rebinds.
- **Prompt-command stdout** (`evaluate_prompt_commands`): whatever the configured commands emit.

Iteration assembly order (`runner/prompt.rs`): `clear_dynamic_sections` → (stale?
`rebuild_base_section`) → `inject_environment_section` → `inject_collaboration_mode` →
`materialize_system_context_rules` → hosted-tools → `evaluate_prompt_commands` →
Before-injections → **`dev_message.sync(...)`** (writes `messages[1]`).

---

## Root cause (precise)

The team already did the two hard, correct things: (1) all volatile content is **isolated**
into a single managed Developer message rather than scattered through the System message, and
(2) `messages[0]` is explicitly held byte-stable for prefix caching. The remaining defect is
purely **placement**: that one dynamic message is written at **index 1, ahead of the
history**, instead of after it. The provider (OpenAI automatic prefix caching here — the
lever is *ordering*, there are no manual cache breakpoints) can only cache a contiguous
prefix; the first byte that changes ends the cacheable region. With volatile content at
index 1, the cacheable region can never extend past index 1 into history.

---

## Options considered

1. **Freeze the environment block at session start** — *rejected.* It would make branch /
   cwd / time read as current when they are not (stale-implying-correct), which is worse than
   a cache miss. (Owner's explicit objection; correct.)
2. **Move just the `# Environment` block after history** — works, but only fixes one section;
   collaboration-mode / rules / prompt-commands would each still bust the cache when they
   change. Partial.
3. **Coarsen the timestamp (date-only)** — *rejected.* Only reduces frequency; still busts at
   every rollover and does nothing for the other volatile sections. A band-aid.
4. **Split the block: static fields → System, volatile fields → tail** — valid but more
   surgery than needed, given the dynamic content is already isolated in one message.

## Recommended fix — move the managed dynamic-context Developer message to the tail

Relocate the **entire** managed dynamic-context Developer message from `messages[1]` to the
**tail** of the message list — the last message before the model responds, i.e. after the
conversation history — while continuing to re-sync it fresh every iteration.

Result:
- `messages[0]` (System, stable) **+** history become one clean, fully-cacheable prefix.
- The dynamic message is still regenerated every turn — **nothing is frozen, nothing goes
  stale** — but now its per-turn change invalidates only itself (a small message at the end),
  not the history.
- Fixes the **entire class** at once (environment, collaboration mode, rules, prompt-command
  output) by fixing the container's position, not by patching individual fields.
- Reuses the existing machinery: `dev_message.sync` already tracks its own slot; this changes
  **where** that tracked slot sits, not what it contains.

This is the smallest change that closes the whole class while preserving full per-turn
freshness.

---

## What to validate before/while implementing

1. **Behavioral (needs an eyes-on check).** Moving rules / collaboration-mode to the tail
   means the model reads them as the *latest-turn* framing rather than up-front system
   context. For instructions this is usually neutral-to-better (recency raises salience), but
   confirm agent behavior is unchanged on a real run — especially anything that depends on
   the rules/mode being positioned as system-level context.
2. **Implementation.** The moved tracked slot must not collide with the compaction-summary
   Developer messages that legitimately live *in* the history (`messages[2..]`) and must
   never be overwritten. `dev_message.sync` already distinguishes its managed slot from
   history Developer messages; the move must preserve that invariant while tracking the tail
   position (which shifts as history grows).
3. **Verify the win.** After the change, confirm `cache_read` grows with history across a
   multi-turn session (rather than pinning at ~15.5k), and the per-turn input is no longer
   re-billing the full history uncached. A before/after on the ANKS repro
   (`cache_read` per turn over a ~20-turn session) is the acceptance test.

---

## Key source references (norn `main` HEAD)

- `crates/norn/src/loop/runner/prompt.rs` — `build_request`: full per-iteration assembly;
  `dev_message.sync(...)` writes the managed Developer message; comment at the sync notes
  `messages[0]` is kept stable "for prefix caching."
- `crates/norn/src/loop/runner/machine.rs` — the `dev_message` tracker ("managed
  dynamic-context Developer message").
- `crates/norn/src/loop/loop_context.rs` — `dynamic_context()` (collects the dynamic
  sections), `clear_dynamic_sections()`, `inject_environment_section()`,
  `inject_collaboration_mode()`, `materialize_system_context_rules()`, `append_system_section()`,
  `base_system_instruction()` / `rebuild_base_section()` / `refresh_context_if_stale()`.
- `crates/norn/src/system_prompt/environment.rs:31` — `format_environment_section`;
  **`:53`** — the `Utc::now().format("%Y-%m-%dT%H:%M:%SZ")` per-turn timestamp.

## Appendix — measured evidence (ANKS, reported via exchange, 2026-07)

gpt-5.5, ChatGPT-auth, norn headless via aion stacked-dev. ~554M input tokens / 147 sessions
/ ~1 week; ~13% prompt-cache hit (vs. >90% typical for agent loops); output ~0.5% of input;
`cache_read` frozen at the static system+tools prefix (~15.5k) across full ~150-turn
sessions while input grew to ~163k/turn; ≈40× amplification vs. a stable prefix. The raw
16-file diagnostic JSON and box repro are available from the reporter on request.
