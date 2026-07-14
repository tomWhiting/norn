# Decisions for Owner Sign-off — Norn Hardening Campaign (2026-07)

This file consolidates every decision the hardening-campaign agents recorded while
implementing (tracks T1-T9) and integrating (seams I1-I3) the `hardening/final-state` briefs.
Nothing here was invented by this document — every line is drawn from an agent's own
structured `decisions` report at the end of its run, or (for §5) from a resolved decision doc
and the Wave 3-4 commit record. Wave 2's planning journal (`wf_5c41d368-666`) recorded a brief
plan, not implementation decisions, so it contributes nothing to §1-4.

**Sections 1-4** cover Waves 1-2 (the original draft). **Section 5** was added after the draft
and covers the R1 assembly-unification decisions (D1-D7, resolved autonomously while the owner
was away) and the load-bearing decisions of Wave 3 (`32fa720`/`9ac8186`/`8ac4aad`, R1 assembly
unification) and Wave 4 (`3c84682`, R2 runner state machine + deterministic tool ordering +
repo-wide LOC/purity sweep). Every §5 entry states the decision, the file path where it lives
at HEAD (`3c84682`), and whether it needs owner attention.

Per CLAUDE.md's **NO ARBITRARY LIMITS / NO ASSUMED DEFAULTS** rule, agents were required to
flag any numeric/behavioral default they touched. Section 1 below is the complete list of
those flagged defaults — in every case the agent reused a pre-existing constant rather than
inventing a new magic number, and each is exposed as configurable. Section 2 catalogs the
non-numeric behavioral/semantic calls made while implementing the briefs. Section 3 pulls out
the items agents explicitly marked as unresolved or requiring an owner decision. Section 4
reproduces the two items already held out of the campaign entirely.

Recommendation key: **Keep** = agent's rationale is sound, ratify as-is. **Discuss** = agent
implemented something reasonable but explicitly asked for sign-off or flagged an alternative.
**Revisit** = works today but has a known gap (e.g., no override plumbed yet).

---

## 0. Items most needing owner attention (read these first)

The three below are the highest-priority calls — the first is a genuine design fork, the other
two are behaviors an agent explicitly wrote "needs sign-off" against. Full detail in §3.

1. **`step_timeout` graceful-timeout redesign** (§3). — **DECIDED 2026-07-02 (owner)**: agents
   may run for hours; there is **no timeout by default** (the existing `step_timeout: None`
   default is confirmed correct and must stay). For callers that opt in via `--timeout` /
   `AgentLoopConfig::step_timeout`, today's documented hard-cut semantics stand; the graceful
   grace-period redesign is **dropped** (it would invent a grace default for a knob the owner
   considers niche).
2. **`WalkBuilder::require_git(false)`** in the search/file tools (§2, §3). — **DECIDED
   2026-07-02 (owner)**: ratified. Outside a git repo there is normally no `.gitignore`, so the
   behavior is moot there; where ignore files do exist, applying them deterministically is
   right. The escape hatch the owner asked for already exists: the per-call `include_ignored`
   parameter (default `false`) walks ignored files on request.
3. **Binary / non-UTF-8 `InvalidData` silent-skip carve-out** in content/AST search (§2, §3).
   — **DECIDED 2026-07-03 (owner)**: ratified as-is. A binary file cannot contain a text
   match, so nothing is hidden by omitting it from `skipped` (which exists to flag results
   that may be incomplete); listing every image/object file would be noise. Matches
   grep/ripgrep behavior. Every other read error is still reported in `skipped`.

4. **Zero-tool agents are supported** — **DECIDED 2026-07-02 (owner-driven fix)**: the former
   `AgentBuilder::build` rejection ("no tools available after exclusions; an agent needs at
   least one tool") is removed. A zero-tool agent (`--allowed-tools ""`, or a profile with
   `tools = []`) is a legitimate pure text-transform configuration (e.g. the owner's TTS
   rewrite pipeline): the system prompt omits its `# Tools` section and provider requests
   carry no tool definitions. The invariant predated the campaign but only reached the CLI
   when R1 unified assembly onto `build()` — it broke a previously-working invocation.
   Regression tests: `zero_tool_agent_builds_for_transform_only_use` (library),
   `empty_allowed_tools_builds_zero_tool_transform_agent` (CLI fence).

5. **Auto-compaction is armed by default for every library-launched agent (root and
   children); reserve-token semantics** — **DECIDED 2026-07-03 (owner)**, after a real
   driven-mode run died `ContextWindowExceeded` at 269k input tokens
   with zero compactions (session `faaa1b04…`, 133 steps). Root cause: the trigger required
   BOTH `context_window_limit` AND `auto_compact_threshold_pct`, and both defaulted `None` —
   an over-reading of the no-defaults rule. Owner clarification (now in CLAUDE.md): the rule
   is **no ARBITRARY defaults** — factual defaults (the generated model catalog's per-model
   `context_window`) and owner-ruled defaults are fine. Rulings: (a) `context_window_limit`
   defaults from `model_catalog::smallest_context_window_for_model` when not explicitly
   configured (explicit config always wins; models absent from the catalog stay `None`);
   (b) the percentage threshold is REPLACED (no compat alias) by
   `auto_compact_reserve_tokens: Option<u64>`, default **`Some(30_000)`** (owner's value) —
   trigger fires when estimated tokens exceed `window − reserve` (gpt-5.5: 272_000 − 30_000
   = 242_000); explicit `off` disables (settings string `"off"` / `-c
   auto_compact_reserve_tokens=off` — for orchestrators that manage context themselves;
   owner guidance 2026-07-03: on very large-window models, prefer lowering the effective
   ceiling — an explicit `context_window` below the catalog value, e.g. 500k on a 1M model —
   over turning compaction off);
   `reserve ≥ window` warns loudly and disables (would otherwise fire every step; the builder
   also warns once and drops the system-prompt compaction guidance). Reserve is absolute, not proportional, because it is sized by
   turn overhead (next input + compaction summary call), which does not scale with window
   size. (c) **The trigger signal is usage-anchored**: the chars/4 client estimate cannot see
   the true cost of encrypted reasoning items (incident numbers: estimate ~236k vs provider-
   reported 269k), so the trigger and the advisory `loop.token_warning` fire on
   `max(estimate, usage_floor)` where the floor is the previous step's provider-reported
   `input_tokens + output_tokens`. The floor lives on `ContextEdits` and is cleared by every
   conversation-shrinking mutation (suppress/summarize/compact/commit/mark_superseded — a
   structural invariant, preventing a stale floor from re-firing compaction in a loop) and is
   never seeded from resumed events. Caveat (flagged): providers reporting cache reads
   outside `input_tokens` understate the floor — safe direction (`max` with the estimate),
   but the anchor is weaker there. The client estimate counts persisted reasoning items
   (encrypted blob + summary/content parts; deliberate overestimate — safe direction), so the
   first post-resume call is covered even though the floor is never seeded from resumed
   events. Related: **(i) child agents have no compaction machinery — RULED 2026-07-03
   (owner): auto-compaction covers ALL agents**, sub-agents and forks included ("we can't
   have them dying mid-work deep in the stack"; children *shouldn't* normally run long
   enough to need it, but coverage is required). **DONE**: one shared
   `arm_auto_compaction(loop_context, config, model)` (agent/assembly.rs) installs the
   estimator + `ContextEdits` and fills the catalog window per-agent; the builder AND all
   three child launch paths (spawn, fork, rhai `spawn_agent`) call it — root and children
   cannot drift. Children compact on their in-memory stores session-lessly. Scoping notes:
   child system prompts have no compaction-guidance seam (pre-existing; the runtime trigger
   is what mattered), and "all agents" means all *library-launched* agents — an embedder
   hand-rolling `run_agent_step` on a self-built `LoopContext` (e.g. the demo examples)
   must call `arm_auto_compaction` itself; the function's rustdoc is the discoverable
   contract. Still open: (ii) reactive compact-and-retry on
   `ContextWindowExceeded` (currently
   Terminal) as the safety net for catalog-miss models; (iii) the owner's planned
   session-storage/event-prominence redesign (per-event prominence levels so low-value
   events — search results, bash output — can be dropped or substituted before
   summarization-grade compaction is needed). Reasoning persistence: ruled and done — see
   the §3 entry.

6. **Skills & delegation rulings (owner, 2026-07-03/04):**
   (a) **Skill scan tiers**: the `cwd/.meridian/skills` tier is legacy experimentation —
   removed; `~/.norn` (NORN_HOME-aware) is the central home-level store, project `.norn` the
   project store; `.agents`/`.claude` convention tiers remain.
   (b) **Confinement carve-out APPROVED**: under a workspace root, agents may READ the
   discovered skill search paths and profile/config directories (home-level skills'
   companion files were reported but unreadable). Write allowance + a Claude-Code-style
   progressively-merged permission config surface for it: follow-up design, owner-sketched.
   (c) **`ChildLoopConfig.max_iterations` REMOVED outright** (the model-suppliable spawn
   grant): it was a silent cutoff — the child was never told its budget, just stopped at the
   gate, so granted children "always error"; models chronically underestimate action counts.
   The root/embedder config knob stays; the model-facing grant goes.
   (d) **Nested delegation: default depth 1 → 2** (children may spawn one level of their
   own), configurable via settings/`-c` (owner ruled the value; inherit-with-decrement and
   narrowing-only invariants unchanged). Prebuilt role profiles (PM/scrum-master, developer,
   reviewer) noted for the internal-agents assistant-hats phase.
   (e) **Cron tool requirements** (for the brief): relative wake-ups ("in N"), time-of-day,
   and looping intervals (minutes/hours/days); fired schedules deliver as injected messages;
   schedules persist as session events (resume restores); no caps on count or interval;
   in-session first, daemon phase later. **Extension applied 2026-07-04 (flagged for owner
   confirmation)**: the resume ruling's no-backfill semantics are extended to LIVE catch-up —
   after a process suspend (laptop sleep freezes timers while wall time jumps), a recurring
   schedule fires ONCE and re-arms from now with the skipped occurrences logged, instead of
   replaying every missed occurrence as a burst (an 8-hour sleep on `every: "1m"` would
   otherwise inject ~480 stacked steers). Same semantics, same rationale, new code path —
   surfaced by the N-026 Fable review.
   (f) **NP-001/NP-002 in-flight rulings (flagged for owner confirmation)**: spool paths gain
   a per-run subdirectory (`…/processes/<run-uuid>/pN.log`) because the flat scheme let a
   resumed session's fresh manager clobber the prior run's `p1.log` — contradicting the
   brief's own "spools persist" intent (NP-001 re-review residual). Recorded NP-001
   residual, accepted: a model-cursor region claimed before a failing disk read is not
   re-offered (loud error; rollback would risk double-delivery under concurrency).
   NP-002 rulings (reviewer-approved, flagged for owner confirmation): filter exits 126/127
   are watch-ERRORS, not no-matches — under `sh -c` a nonexistent filter binary exits 127
   rather than failing to spawn, so R4's "broken filter must alert" is unsatisfiable under a
   literal "any non-zero = no match"; the reserved shell codes are the only distinguishing
   signal and the failure mode is conservative. And a WATCHED process's completion notice
   waits for its output pipes to close (drain-join) so the final region is filtered losslessly
   before "it's done" — an immortal pipe-holding grandchild therefore defers a watched
   process's completion until pipe close, kill, or shutdown (unwatched processes keep NP-001
   timing exactly; `op=kill` is the escape hatch; documented in the process guidance).
   Recorded NP-002 residual, accepted under the no-timeout ruling: an agent-authored filter
   that never exits wedges its own watch run (in-flight runs are deliberately uninterruptible
   by unwatch; `op=kill` does not reach the filter — it is norn's child, not the watched
   group's) and defers that process's completion notice until shutdown; `kill_on_drop`
   guarantees nothing outlives norn. Self-inflicted, loud at shutdown, on the ledger.

Beyond these, §5.1 (R1 D1-D7) were **applied autonomously while the owner was away** and are all
reversible on `hardening/final-state` before merge — the owner may override any of them.

---

## 1. Documented overridable defaults

| Default | Value | Provenance | Configurable via | Track | Recommendation |
|---|---|---|---|---|---|
| `DEFAULT_MAX_RESULTS` | 50 | pre-existing constant | search tool per-call param | T3-search | Keep |
| `OAuthHttpOptions::request_timeout` | 10s | pre-existing constant (refresh/revoke/code-exchange) | `OAuthHttpOptions` builder | T1-provider | Keep |
| `OAuthHttpOptions::callback_timeout` | 5 min | pre-existing constant (login callback wait) | `OAuthHttpOptions` builder | T1-provider | Keep |
| `DEFAULT_RETRY_BACKOFF` | 1s | pre-existing, owner-approved; consolidated from two duplicate copies into one constant in `provider/exec.rs` | provider retry policy | T1-provider | Keep |
| `RA_EXTENSION_REQUEST_TIMEOUT` | 10s | pre-existing (carried over from prior `relatedTests` wiring) | **not currently plumbed** through the backend surface for these extension calls | T2b-lsp | Revisit — no override path exists yet |
| `MODEL_OUTPUT_INLINE_CHAR_LIMIT` | 64,000 chars | pre-existing constant | `ToolOutputBudget::for_context_window` | T2a-tools-core | Keep |
| `DEFAULT_PROMPT_COMMAND_TIMEOUT` | 5s | pre-existing constant mirroring `integration::variables` | `AgentLoopConfig::prompt_command_timeout` (falls back when `None`) | T4-loop-agent | Keep |
| `BROADCAST_BUFFER_CAPACITY` | 256 | pre-existing constant | driven JSON-RPC transport | T8-cli-jsonrpc | Keep |
| Index-lock wait deadline | `None` (indefinite) | **not** a new default — matches today's exact pre-existing behavior | `SessionManager::with_index_lock_deadline` | T5-session | Keep |

---

## 2. Semantic / behavioral choices

### Search & file tools — T3-search, T2a-tools-core
- `include_ignored` defaults to `false` (filtering on by default, explicit opt-in). — **Keep**
- Ignore rules use `WalkBuilder::require_git(false)`: `.gitignore`/`.ignore`/global excludes apply even outside a git repo, for deterministic behavior across repo and non-repo trees. Agent explicitly flagged **needs sign-off**. — **Discuss**
- Binary/non-UTF-8 files (`io::ErrorKind::InvalidData` on read) are skipped silently in content/AST modes (no matchable text); every other read error is still reported in `skipped`. Agent explicitly flagged **needs sign-off** on the carve-out. — **Discuss**
- `files` mode reimplemented on the walker with root-relative pattern matching instead of `glob::Pattern::escape`-ing the base path — removes the base-path-injection class entirely (root-cause fix for R5) and lets `files` mode honor ignore rules. — **Keep**
- Confinement refusal returned as a tool failure output (`kind=confinement_refused`, `ToolErrorKind::PermissionDenied`), matching the Read/Write/Edit convention, rather than a Rust-level `ToolError`. — **Keep**
- AST partial compile failures surfaced in a new additive `query_errors` output array, preserving graceful degradation for mixed-language trees. — **Keep**
- Symlinks are not followed during walks and symlink entries are never content-read; the search root itself is symlink-checked by confinement's canonicalization. — **Keep**
- An explicitly named file passed as `path` is always searched even if gitignored — explicit reference is treated as intent. — **Keep**
- `search/mod.rs` restructured to declarations/re-exports only (CLAUDE.md mod.rs rule); logic split into `tool.rs`/`content.rs`/`file_find.rs`/`helpers.rs`. — **Keep**
- `SkillToolConfig::default` keeps `shell_execution = true` (preserves established `agentskills.io` `!\`command\`` expansion behavior); embedders disable via `SkillTool::with_config`. `effect()` reports `Process` when enabled, `ReadOnly` when disabled. — **Keep**
- `WebFetchTool::with_client` deleted rather than "verifying" the redirect policy on a caller-supplied client (unauditable once built) — the tool now owns all client construction (`Policy::none`, resolver pinning). Breaking API change, per no-backwards-compat rule. — **Keep**
- Bare-registry direct dispatch surfaces `tool_use_description` via `tracing::debug` rather than extending `ToolEnvelope` (that path has no action log to attach intent to). — **Keep**
- R7: dispatch made to match the documented `register_follow_ups` contract; `edit.rs`'s non-committing ambiguous-match result confirmed load-bearing (drives `apply_at_occurrence_N`), kept as-is rather than "fixed" as a gate-failure workaround. — **Keep**
- Read tool image handling: `tokio::fs::metadata` stat runs before any classification; missing files are I/O errors for all kinds; success handlers register text/binary/image kinds only after a real filesystem observation. — **Keep**

### JSON-RPC driven protocol — T8-cli-jsonrpc
- R4: `initialize` result's `protocolVersion:"2.0"` replaced (not added alongside) by `protocol:"norn-driven/1"` (`DRIVEN_PROTOCOL_VERSION` constant) — the JSON-RPC version already rides every frame's `jsonrpc` tag. — **Keep**
- R5: typed stop carries per-variant detail beyond the bare `{reason}` sketched in the requirement — `schema_unreachable{attempts, validation_errors}`, `timed_out{elapsed_ms, iterations}`, `truncated{truncation, iterations}` — because with no `retryable` field on the wire, callers need variant detail to make the retry judgment themselves. — **Keep**
- R5: the `result` string label removed outright (replaced by `stop.reason`); `ENVELOPE_VERSION=1` introduced as the documented contract version. — **Keep**
- R2: in degraded mode, **both** `inject` and `cancel` are answered `-32603` and no cancel token is threaded into the step, per the requirement's literal contract. Agent noted an alternative was considered and rejected: cancel doesn't strictly need the router (token is local), so degraded mode *could* still serve `intervene/cancel` honestly. Flagged: **"flag if the owner prefers that partial-service behavior."** — **Discuss**
- R3 adjunct: mid-run `initialize` is re-served with capabilities (idempotent, read-only) instead of erroring; busy error is `-32000` (implementation-defined server-error range). — **Keep**
- R9: cancel ack status renamed from `"cancelling"` to `"cancel_requested"` — the ack only guarantees the signal was applied, not that the run has stopped; the terminal `stop.reason` is declared authoritative. — **Keep**
- Stop envelope deliberately omits any `retryable` field, per a **pre-made owner decision** (documented in `DRIVEN-PROTOCOL.md`; Gap C annotated SUPERSEDED) — included here for traceability, not new. — **Keep (already decided)**
- e2e round-trip asserts `event/progress` (not `event/message`) because the openai-compatible provider only ever emits `TextDelta`, never `TextComplete`. — **Keep**
- A `run/execute` prompt resolving entirely to a local slash command returns `result: null` (documented, existing behavior left as-is). — **Keep**
- Cancel-ack race resolved by documentation + the existing biased-select narrowing, not further code changes — ack means "signal applied," terminal `stop.reason` is authoritative. — **Keep**

### Macro / schema derive — T9-macros
- Untagged enums with more than one unit variant are rejected with a spanned compile error (serde deserializes every untagged unit variant from `null`, so a second is unreachable and the schema can't represent it faithfully). — **Keep**
- A single untagged unit variant emits `{"type":"null"}` — the exact shape serde accepts; the variant name never appears on the wire. — **Keep**
- Container `#[serde(default)]` does NOT relax a flattened field's inner required list (verified empirically against serde's own behavior). — **Keep**
- The split `(serialize = ..., deserialize = ...)` form of rename attributes is now supported, taking the deserialize side (schemas describe model input). — **Keep**
- Rename-rule precedence mirrors serde exactly: field `rename` > variant `rename_all` > container `rename_all_fields` (verified by probe). — **Keep**
- `trybuild = "1"` added as a crate-local dev-dependency (not workspace-level, since root `Cargo.toml` is out of ownership; matches how `syn`/`quote`/`proc-macro2` are declared). — **Keep**
- Diagnostic type rendering normalized via a display helper (`std :: path :: PathBuf` → `std::path::PathBuf`). — **Keep**
- Unknown `#[serde(rename_all)]` rule names are a hard spanned compile error, matching serde's own derive rejection. — **Keep**
- Tagged enum root schemas declare `type:"object"` (sound — every tagged variant is an object); untagged roots stay untyped since variants may be non-objects. — **Keep**
- `tool_args/mod.rs` and `follow_up/mod.rs` reduced to declarations + re-exports per CLAUDE.md; expansion logic moved to `tool_args/derive.rs` / `follow_up/expand.rs` (no API change). — **Keep**

### Provider / auth — T1-provider
- Error-body read timeout reuses the existing `ProviderConfig::timeout` stall deadline rather than a new knob — same per-phase stall semantics already bounding headers/inter-chunk gaps. — **Keep**
- Stalled error-body reads classify as retryable `Timeout` for **both** 5xx and 4xx statuses (the stall is a transport fault regardless of status); reason string deliberately avoids the `"HTTP 5"` prefix so classification doesn't get confused, status is still included for diagnostics. — **Keep**
- The existing `ProviderError::StreamInterrupted` (retryable `ConnectionReset`) is reused with chunk/event diagnostics rather than adding a new variant, matching what `openai_compatible` already raised for the same condition (`error.rs` was out of this track's ownership). — **Keep**
- Service-tier backend discriminator mirrors `capabilities()` exactly via one shared `is_chatgpt_backend()` so the two can never disagree. **Consequence flagged by the agent:** a Fast-tier request on an API-key Responses connection now fails typed `InvalidRequest` instead of silently borrowing the subscription mapping ("priority"). — **Discuss** (behavior change, worth confirming it's intended)
- State-mismatched `/auth/callback` requests are treated as foreign (404 + keep listening) rather than aborting the flow, so a forged/stale request can't kill a legitimate in-flight login. — **Keep**
- `parse_sse_bytes` kept as a `#[cfg(test)]` wrapper over `SseParser` so there is exactly one parsing implementation. — **Keep**
- `AuthManager` constructors became fallible (`Result<Arc<Self>, AuthManagerBuildError>`) because they now eagerly build the shared `reqwest::Client`; mapped to `ProviderError::ConnectionFailed` at the call site, matching existing precedent. — **Keep**
- `login()`/`logout()` keep their public signatures and use `OAuthHttpOptions::default()` internally — changing `LoginConfig`'s field set would break a struct literal outside this track's ownership (`crates/norn-cli/src/commands/auth.rs`). Full configurability is available via `AuthManager` + `OAuthAuthProvider::from_manager`; CLI-level plumbing is a recorded blocker, not done here. — **Keep** (note: CLI plumbing gap remains)

### LSP — T2b-lsp
- `LspLocation` convention fixed as **one-based** (requirement's stated preference) — producers already emitted one-based, so docs/tests were aligned to producers rather than converting every producer/consumer. — **Keep**
- `LspBackend` trait defaults for `test_runnables`/`related_tests` kept; production `WorkspaceLspBackend` impl is now real. Non-rust-analyzer servers report zero runnables with a debug log rather than a hard error; `METHOD_NOT_FOUND` maps to empty (degraded capability logged, protocol errors still propagate). — **Keep**
- A failed `didChange` for any stale tracked file evicts that entry and propagates the error to the in-flight call, rather than silently skipping it — caller sees the server-side failure instead of results computed against a known-stale view. — **Keep**
- Drop-based graceful shutdown only fires when the backend is the last `Arc` holder (diagnostic bridge may share it); with no async runtime at drop time, `kill_on_drop` remains the documented fallback. — **Keep**
- Stub-server tests gate on `python3` availability at runtime with a `tracing::info` skip line (per the no-`#[ignore]` rule), probing well-known absolute interpreter paths before `$PATH`. — **Keep**

### Config / profiles / skills — T6-config-profile-skill
- R1: `HookEntry.timeout` is **required** at the type level (plain `u64`, milliseconds) — omitting it is a typed deserialization error naming the field/file. A stale doc comment claiming it was optional was corrected. — **Keep**
- R3: `serde_ignored 0.1` added (pre-approved) for nested unknown-key detection. Documented limitation: `#[serde(flatten)]` sections buffer their content, so unknown keys inside a flattened section can't be reported. — **Keep**
- R5: capability resolution — `capabilities/` dirs are siblings of each `profiles/` scan dir; first-dir-wins shadowing mirrors profiles; an unresolvable capability reference is a typed error (never silently dropped); profile-level `disallowedTools` flows through a synthetic `_profile_disallowed` capability into `resolved_disallowed`. — **Keep**
- R6: skill shadowing precedence — earlier scan dir wins; within a dir, directory-form `SKILL.md` beats flat `.md`; lexicographic path order breaks same-name ties. — **Keep**
- R7: trigger glob compilation is cached process-wide (compile-once); a pattern reaching evaluation without parse-time validation is error-logged exactly once and cached as a permanent non-match — loud, never silently indistinguishable from no-match. — **Keep**
- R10a: undefined positionals (`$N` with no value) pass through literally; recognized tokens with a missing value resolve to empty string. — **Keep**
- R10d: an unavailable bash binary surfaces via an inline `[skill shell command failed: …]` marker rather than aborting expansion, consistent with the stage-1 failure policy. — **Keep**

### Session — T5-session
- R2 off-executor design: `EventStore::checkpoint_off_executor(self: Arc<Self>)` is the documented step-boundary async wrapper (spawn_blocking) around the sync checkpoint, which remains the primitive (required by `JsonlSink::Drop` and sync embedders). `JoinError` maps to a typed `SessionError::StorageError` with explicit "flush landed is unknown" semantics. — **Keep**
- `tree::branch` failure ordering: `Fork` event appends before child insertion; the theoretically unreachable parent-vanished-after-append case returns a typed `StorageError` rather than silently inserting an unlinked child. — **Keep**
- `RevertStatus` gained an `Unknown` variant (breaking enum change) for files unreadable for reasons other than absence, so evidence-free comparisons never miscalibrate the revert baseline. — **Keep**
- `conversion.rs` uses `serde_json::Value`'s total `Display` (compact JSON) instead of a fallible serializer round-trip, removing the silent empty-string collapse without introducing a new fallback. — **Keep**

### Loop / agent runtime — T4-loop-agent, seam I2
- Item 2: step-exit sweep re-stamps `msg.to_id` to the loop's own agent id before re-queuing; when the loop context has no agent identity or pending store, the loss is logged at error level per message rather than passing silently. — **Keep**
- Item 2: `InboundChannel::recv()` added as the idle-park wake primitive; it currently has no in-crate production caller — deliberate, so the recorded blocker fix isn't itself blocked on this track's files. `steer_ready()` was deliberately not reused (its update-suppressing wake protects a *running* loop; a parked agent has none, and parking must not strand acknowledged messages). — **Keep**
- Item 8 (verified from a prior pass): an in-band `ProviderEvent::Error` now fails the call immediately in `loop/classify.rs::call_provider` with its typed `ProviderError`, so the retry policy classifies the real error; the `loop/assembly.rs` Error arm is documented as unreachable through the loop. — **Keep**
- Item 11: compaction summarization retry stays single-attempt with digest fallback, per the **June owner decision**; now additionally cancel-responsive (raced against the step's `CancellationToken`; a cancelled trigger commits nothing). — **Keep (already decided)**
- Seam 1 (final shape): `apply_persisted_compactions` runs exactly once per loop context, gated by `LoopContext.compaction_marks_loaded` — covers drivers resuming with a fresh `ContextEdits`; every compaction appended afterward marks supersession at commit time. Constant-time per step, no information loss. — **Keep**
- Seam 2: offload primitive chosen was `tokio::task::block_in_place`, not `spawn_blocking`/`checkpoint_off_executor` — the step API hands the loop `&EventStore` (borrowed, not `Arc`); `block_in_place` is the borrowed-data form of the same offload, keeps appends strictly ordered per session, and surfaces the `Result` exactly as an inline append would. Gated on `RuntimeFlavor::MultiThread`; current-thread runtimes append inline. — **Keep**
- Seam 3: the idle-park select arm carries an `inbound_open` guard — once all senders drop, `recv()` returning `None` permanently disables the arm instead of resolving instantly forever. — **Keep**
- Seam 3: sweeps treat `Steer` and `Update` identically (FIFO into the pending store) — a stranded message has no live loop, so the kinds' delivery-timing distinction doesn't apply at requeue time. — **Keep**
- `agent/pending_messages.rs`: audit appends now route through `append_off_executor` for the same off-executor guarantee, since they ride the drain/requeue hot path. — **Keep**

### Error taxonomy / reasoning replay — seam I1-error-taxonomy-reasoning
- `StreamError` uses a single `transient: Option<TransientKind>` field instead of separate status+transient fields — `TransientKind::ServerError` already carries the status, avoiding a contradictable second field. — **Keep**
- `ConnectionFailed` was also structured (`kind: TransientKind`) even though the spec named only `StreamError` — `class()` string-matched `"timed out"` on its reason too, and the seam's goal is finishing the magic-string kill. Producers only ever set `Timeout` or `ConnectionReset`. — **Keep**
- `server_is_overloaded`/`slow_down` keep `ServerError{status: 503}` as the structural classification (preserves every existing status-specific retry policy), but the `"HTTP 503:"` reason-prefix encoding hack was removed — reasons now carry the provider message verbatim. — **Keep**
- In-band error messages with no transport semantics (Chat Completions error frames, claude-runner error events, mock/lock-poison errors) are now always terminal (`transient: None`); previously a free-text `"timed out"` inside such a message could accidentally classify as retryable. Agent's own words: **"Deliberate, honest edge-behavior change."** — **Discuss**
- Missing/empty `tool_call_id` on `ToolResult` replay reclassified from `ResponseParseError` to `RequestSerializationFailed` in both serializers (it's a request-construction failure); classification (Terminal) unchanged. — **Keep**
- Reasoning replay wire shape follows the Codex CLI reference: `type:'reasoning'` with tagged `summary_text` parts, optional content parts, `encrypted_content` verbatim, `rs_*` item id omitted; items replay before the assistant message/tool calls of their turn; only items with `encrypted_content` are echoed. Codex's content-skip heuristic (skip content arrays lacking `reasoning_text`) was **not** copied — content is serialized whenever present. Agent flagged this explicitly as **"the one wire-format ambiguity."** — **Discuss**
- A wire reasoning item that fails to deserialize is dropped with a `tracing::warn` (mirroring `function_call`/`custom_tool_call` handling) — display text already flowed via thinking deltas; a fabricated partial item would corrupt replay. — **Keep**
- Malformed-item and unknown-`response.failed`-code paths never opt into retry (`transient: None`). — **Keep**

### CLI / TUI / config integration — seam I3-cli-tui-config-seams
- `SkillToolSettings.shell_execution` is `Option<bool>`; `None` defers to `SkillToolConfig::default()` (shell enabled) — no duplicated default in config code (NO ASSUMED DEFAULTS compliant). — **Keep**
- `tools.skill` merges field-by-field like `tools.write` (`merge_skill` mirrors `merge_write`); no validation entry added since a bool has no invalid range (sibling `write` settings have none either). — **Keep**
- Unknown-key allowlisting is structural: `parse_settings_with_unknown_paths` uses `serde_ignored` over the typed structs, so adding the typed field *is* the allowlist change (pinned by a loader test in both directions). — **Keep**
- TUI slash-dispatch path made async end-to-end (`try_dispatch_slash`, `handle_new`, `handle_compact`, `rotate_store_dependents`) instead of blocking the executor; added `with_scroll_region_cursor_async` (edition-2024 `AsyncFnOnce`) sharing cursor-reconcile logic with the sync wrapper. — **Keep**
- `turn.rs` awaits the checkpoint **before** the sync scroll-region closure (turn events are already appended, so ordering is unchanged); the failure message is carried into the closure for error-line rendering. — **Keep**
- `rotation.rs` now mirrors libnorn's `restore_session_state`: one `ReplayArtifacts` snapshot feeds both `ContextEdits::mark_superseded` and `rebuild_action_log`; `handle_new` resets `ContextEdits` **before** rotation so replayed marks are never wiped. — **Keep**
- Renderer `body()` restructured so `skipped`/`query_errors` render even when `matches`/`paths` are empty — previously the early-return silently dropped them (bug fix). — **Keep**
- Intra-doc link fixes used `crate::agent_loop` (existing pub re-export of `r#loop`, since rustdoc can't resolve `r#loop` in link paths); links to private items demoted to plain code spans rather than `#[allow]`ed. — **Keep**
- Shell-disabled `SkillTool` schedules as `ToolEffect::ReadOnly` (no shell can spawn), asserted end-to-end. — **Keep**
- `crates/norn/src/agent/monitor.rs` was deliberately **not** touched despite holding the last remaining rustdoc warning in the tree — it is on the HOLD-FOR-DISCUSSION list, which overrides the doc-comment ownership exception. — **Keep** (correctly honors the hold — see Section 4)

---

## 3. Proposed-but-not-implemented / explicitly flagged for owner discussion

These are the items agents themselves marked as unresolved, proposed-only, or requiring an
explicit owner call — either because implementing them would require inventing a default, or
because a design fork exists that the agent didn't feel authorized to resolve alone.

- **`step_timeout` graceful-timeout redesign** (T4-loop-agent, Item 6). Current hard-cut
  semantics (inner future dropped mid-tool-batch) are documented as-is. Agent explicitly wrote:
  *"PROPOSED FOR OWNER DISCUSSION, not implemented: a graceful-timeout redesign where elapsing
  the budget triggers the cancellation token plus a bounded grace period before the hard cut —
  the grace value would be an invented default, so it needs an owner decision."* — **Discuss**
- **Mustache reading session variables from user-supplied argument names** (T6-config-profile-skill,
  R10a). Left functional as-is. Agent's own tag: *"OWNER FLAG (as instructed): stage-3 mustache
  can read session variables whose names arrive from user-supplied arguments — left functional;
  needs an owner decision on whether user args should be able to name session variables."* — **Discuss**
- **Reasoning items not persisted into session `AssistantMessage` events** (seam
  I1-error-taxonomy-reasoning). — **DECIDED 2026-07-03 (owner): persist them.** The gap's other
  face surfaced in the `faaa1b04…` incident: a resumed session was ~30k tokens lighter than its
  live counterpart *because* the reasoning items evaporated — resume was acting as an
  accidental compaction while silently losing the model's reasoning state.
  `SessionEvent::AssistantMessage` now carries the captured `ReasoningItem`s
  (`encrypted_content` included; serde-defaulted so legacy files read as empty, key omitted
  when empty so non-reasoning sessions are byte-stable) and `session/conversion.rs` rebuilds
  `Message.reasoning` on resume. Session files grow accordingly — accepted; feeds the planned
  session-storage/prominence redesign (§0.5).
- **`loop/runner.rs` 500-LOC compliance gap — RESOLVED at Wave 4 (`3c84682`).** The draft
  flagged `loop/runner.rs` (852 LOC at the time) as an open CLAUDE.md-compliance gap dominated
  by the ~650-line `run_agent_step_inner`, deferred to a dedicated exclusive-ownership pass. That
  pass is Wave 4's R2 runner state machine: `loop/runner.rs` is now the `loop/runner/` directory
  (`machine.rs` 201, `dispatch.rs` 380, `entry.rs` 325, `setup.rs` 211, `provider_call.rs` 205,
  `prompt.rs` 170, `stop.rs` 131, `mod.rs` 37 — all non-test source under the 500-LOC cap), with
  the step loop reshaped as an explicit `StepMachine` (`StepMachine::initialize` →
  `StepMachine::run`). No longer an open gap. — **Resolved** (see §5.2)
- **T8-cli-jsonrpc R2 degraded-mode alternative** — implemented per the requirement's literal
  contract (both `inject` and `cancel` fail in degraded mode), but the agent flagged that
  `cancel` doesn't strictly need the router (its token is local) and a degraded run *could*
  still serve `intervene/cancel` honestly. — **Discuss**
- **T1-provider service-tier discriminator consequence** — a Fast-tier request on an API-key
  Responses connection now fails typed `InvalidRequest` instead of silently reusing the
  subscription "priority" mapping. Implemented deliberately for consistency with
  `capabilities()`, but it is a user-visible behavior change worth an explicit nod. — **Discuss**
- **T3-search `WalkBuilder::require_git(false)`** — implemented, but the agent explicitly wrote
  "needs sign-off" in its own decision record. — **Discuss**
- **T3-search binary/non-UTF-8 `InvalidData` silent-skip carve-out** — implemented, agent
  explicitly wrote "needs sign-off on the InvalidData carve-out." — **Discuss**

---

## 4. Held items (formerly `docs/HOLD-FOR-DISCUSSION.md`) — RESOLVED

Both items were held untouched through the campaign, then talked through with the owner on
2026-07-03 and **DECIDED: deleted**. The hold doc is retired; the forward design record is
`docs/design/norn/INTERNAL-AGENTS.md`.

1. **`RunMonitored` — AI-monitored background tasks** — **DECIDED 2026-07-03 (owner):
   deleted.** The scaffolding (`agent/monitor.rs`: zero production callers, unused `_provider`,
   static-string heartbeat around an in-process Rust future) implemented none of the actual
   intent, which the discussion surfaced as much larger: a taxonomy of *internal agents*
   (processors / watchers / assistant / speaker) built on a managed background-process manager,
   with watcher alerts riding message injection. None of the deleted code contributes to that
   design. See `docs/design/norn/INTERNAL-AGENTS.md` §5 (watchers) and §3 (process manager).

2. **`ToolEnvelope.runtime_inputs` + `ToolContext.runtime_args`** — **DECIDED 2026-07-03
   (owner): deleted; the architectural fork is ruled — boundary signals ride the durable
   message-injection path (`MessageRouter` + pending store + rules engine), never the tool
   envelope.** Rationale: injected messages are persisted as session events (resume-safe where
   envelope-ridden signals would silently vanish), they deliver on turns with zero tool calls
   (exactly when an interrupt matters most), and the envelope flows *to the tool* — splicing
   signals toward the model through it would pollute tool-result semantics and attribution.
   `runtime_args` separately: every policy input it was designed to carry became a typed,
   enforced surface (`workspace_root` confinement, `ToolOutputBudget`, pre/post checks,
   `extensions`, the permission policy); an untyped JSON blob beside those is a regression.
   Deleted: the `runtime_inputs` field, `RuntimeInputs`, `InboundMessage`, `DiagnosticReport`,
   `FileChange`, `FileChangeType`, and `ToolContext.runtime_args`. The envelope keeps its live
   parts (`model_args`, `tool_use_description`, `metadata`). Future diagnostics/filesystem
   feeds are producers into the injection path, per INTERNAL-AGENTS.md.

---

## 5. R1 assembly-unification (D1–D7) and Wave 3–4 decisions (post-draft)

Everything in §1–4 predates Wave 3. This section covers what landed after the draft: the seven
R1 open decisions (resolved autonomously per the campaign's standing "keep going" directive, all
reversible before merge — **owner may override any of these; flag on review**) and the
load-bearing decisions of the Wave 3 assembly-unification and Wave 4 runner/ordering/LOC work.
The R1 subsection reproduces `docs/design/norn/R1-DECISIONS-RESOLVED.md` (that file is the
authoritative source and is left unedited); the framing "applied autonomously, owner may
override" is intact.

### 5.1 R1 open decisions — applied autonomously (owner may override)

Source: `docs/design/norn/R1-DECISIONS-RESOLVED.md`, resolutions for
`BRIEF-R1-ASSEMBLY-UNIFICATION.md` §7. Owner was away at the decision point; these are the
recommended defaults. All verified against HEAD (`3c84682`).

- **D1 — Session-hook ownership: `Agent::run` fires them.** `Agent::run` fires
  `on_session_start`/`on_session_end` with `info.session_id`
  (`crates/norn/src/agent/instance.rs:247-257,295-297`); `into_parts` drivers (TUI, print
  step-loop) get explicit `fire_session_start`/`fire_session_end` helpers on `AgentParts`
  (`instance.rs:117,128`). Fixes the confirmed bug at the source — embedded agents (including
  every Meridian path) previously fired no session hooks. The `run` body even names Meridian in
  its rationale comment. Meridian can drop its hand-firing (see MERIDIAN-HANDOFF §9.1). —
  **Discuss** (owner-overridable; the applied default)
- **D2 — Root registry registration: opt-in.** `build()` reserves the `AgentRegistry` "/root"
  entry only when BOTH `.agent_registry()` and `.register_root(path, role)` are set; never
  mandatory. Embedders like Meridian wire no coordination and must not be forced to register a
  root; TUI/print opt in. — **Discuss** (owner-overridable)
- **D3 — Terminal-reclamation control: `.terminal_reclamation(bool)`, default `true`.** `true`
  preserves today's unconditional `install_terminal_reclamation`; the TUI passes `false` (its
  status panel owns reclamation). The default is the existing documented behavior, not an
  invented value. — **Discuss** (owner-overridable)
- **D4 — CLI session front door: `.open_session`.** `builder_from_cli` uses
  `.open_session(SessionManager, SessionSpec, DurabilityPolicy::Flush)` at build time;
  `--no-session` maps to `.session(EventStore::new())`. Print's post-build ordering for
  `debug_dump_file` is preserved by reading `parts.info.session_id`. — **Discuss**
  (owner-overridable)
- **D5 — Skill-tool registration gate: `load_runtime_base` path only.** `SkillTool` registers
  where the catalog + `SkillToolConfig` exist — the `load_runtime_base` extension path — gated on
  `!base.skill_catalog.is_empty()` (`crates/norn/src/agent/builder.rs:360-366`), mirroring the
  CLI's `!catalog.is_empty()` gate. Library agents built without `load_runtime_base` carry no
  catalog and so get no skill tool — correct. Registration happens before `from_profile` gating
  so allow/deny lists apply to it as in the CLI. — **Discuss** (owner-overridable)
- **D6 — Meridian migration scope: OUT of scope (norn only).** R1 exposes the library surfaces so
  Meridian *can* delete its copies (`NornSessionStore`; see MERIDIAN-HANDOFF §9.2) via
  `.open_session`, but the actual Meridian edits are a separate PR. The capability-discovery
  helper (§C) and shared provider defaults (§D) are NOT added to the library now — intentional
  Meridian copies until a future ask. — **Keep** (scoping decision; keeps this campaign
  norn-only and independently mergeable)
- **D7 — `event_schemas` / `variables` on the builder: yes.** `.event_schemas()` and
  `.variables()` added to `AgentBuilder` (additive; the CLI needs them and the builder is the
  assembler). Minor public-surface expansion. — **Keep**

### 5.2 Wave 3–4 load-bearing decisions

Decisions recorded in the Wave 3–4 commit record, verified against HEAD.

- **`SessionSpec` "latest" is working-dir-scoped, never global**
  (`crates/norn/src/agent/session_spec.rs:39,56`). The no-argument `--resume`/`--fork` sentinels
  map to `SessionSpec::ResumeLatestInWorkingDir` / `ForkLatestInWorkingDir`, which select the
  most-recently-updated session whose indexed working directory matches the current project —
  deliberately NOT the globally most-recent session across every directory, which would
  cross-contaminate unrelated projects. The empty-string "latest" sentinel is these variants, not
  `Resume`/`Fork`. — **Keep**
- **`ToolRegistry::names()` returns lexicographically-sorted names**
  (`crates/norn/src/tool/registry.rs:174-183`). The backing store is a `HashMap` (per-instance
  randomised iteration order); every prompt- and request-visible projection of the registry (the
  system prompt `# Tools` section, the provider tool-definition array via
  `collect_function_definitions`, the tool catalog, the MCP listing) is built from this iterator,
  so a stable order keeps those byte-identical between process runs and **preserves provider
  prompt caching** (the doc comment states this verbatim). — **Keep**
- **`ToolRegistry::is_registered` is the unmatched-flag-warning reference**
  (`crates/norn/src/tool/registry.rs:156`; used at
  `crates/norn-cli/src/runtime/wiring.rs:100`). It answers "is this a real tool at all?" (physical
  registration, ignoring effect-gating) so the CLI can warn when an `--allowed-tools` /
  `--disallowed-tools` flag names a tool that matches nothing, without false-flagging a tool that
  was correctly gated out. — **Keep**
- **`Box::pin` at the runner step seam** (`crates/norn/src/loop/runner/entry.rs:324`,
  `Box::pin(machine.run()).await`). The per-step driver future carries the whole step state
  (~16 KiB); pinning it on the heap keeps every embedder's future — spawned child steps, the TUI
  event loop, the CLI drivers — small instead of inlining that state into each
  (`clippy::large_futures`). One allocation per step; `initialize` is a separate statement so the
  completed init future is not held alive across the run. — **Keep**
- **R2 runner state machine** (`crates/norn/src/loop/runner/machine.rs`, `StepMachine::initialize`
  → `StepMachine::run`). The core step loop is reshaped from one ~650-line function into an
  explicit `StepMachine` split across the `runner/` submodules (setup / prompt / provider_call /
  dispatch / stop), resolving the §3 LOC gap. Behaviour-preserving refactor. — **Keep**
- **`Agent::run` auto-fires session hooks** — the D1 mechanism, restated here because it is a
  Wave 3 behavioral decision with external (Meridian) impact: the end hook fires only on the
  normal-exit path; an error short-circuits via `?` and skips it, matching the driver contract
  (`crates/norn/src/agent/instance.rs:250-252`). — **Discuss** (overlaps D1)
- **Skill-tool gate on non-empty catalog** — the D5 mechanism (see §5.1). Restated as a Wave 3
  decision because it is the one that lets embedded agents get the skill tool "for free" on the
  library path. — **Discuss** (overlaps D5)

---

## Summary

- **Section 1 (documented defaults):** 9 items, all pre-existing constants, all reused not
  invented, all Keep.
- **Section 2 (behavioral/semantic choices):** ~80 items across 10 subsystems, the large
  majority Keep; 4 flagged Discuss inline (degraded-mode cancel alternative, service-tier
  consequence, in-band-error terminal reclassification, reasoning-replay content-skip
  ambiguity).
- **Section 3 (explicitly proposed/flagged):** 8 items the agents themselves called out as
  needing an owner decision, ranging from a genuine feature-design question
  (`step_timeout` graceful redesign) to two explicit "needs sign-off" search-tool behaviors. The
  CLAUDE.md 500-LOC compliance gap (`loop/runner.rs`) is now **Resolved** at Wave 4 (R2 runner
  state machine, `loop/runner/`), leaving 7 open owner items in this section.
- **Section 4 (held):** RESOLVED 2026-07-03 — both items (`RunMonitored`,
  `ToolEnvelope.runtime_inputs` / `ToolContext.runtime_args`) owner-ruled **deleted**;
  boundary signals ride message injection. Forward design:
  `docs/design/norn/INTERNAL-AGENTS.md`.
- **Section 5 (R1 D1-D7 + Wave 3-4):** 7 R1 decisions applied autonomously (owner-overridable, 5
  Discuss / 2 Keep) plus 7 Wave 3-4 load-bearing decisions (mostly Keep). The three items in §0
  remain the highest-priority owner calls.

---

## 6. Meridian integration-audit follow-ups (C1, C4, C7, C8) — 2026-07-04

Four embedder-facing improvements surfaced by the meridian integration audit
(`meridian/docs/reviews/2026-07-04-norn-integration-audit.md`, items C1/C4/C7/C8).
Each decision below is recorded for owner sign-off; silence = ship.

### C7 — `ToolCallDelta` carries `call_id` for live tool-input correlation

- **Ordering verdict (Responses API): GUARANTEED.** The OpenAI Responses stream announces
  every output item with `response.output_item.added` (carrying both the streaming `item_id`
  `fc_*`/`ctc_*` and the correlation `call_id` `call_*`) *before* emitting that item's
  `response.function_call_arguments.delta` / `custom_tool_call_input.delta` events. Evidence:
  the codex reference mapper (`crates/norn/reference/codex-rs/sse-responses.rs:410-415`) parses
  `output_item.added` into a `ResponseItem::FunctionCall` carrying `call_id`, and the Responses
  lifecycle (`created → in_progress → output_item.added → …deltas… → output_item.done →
  completed`) fixes the ordering. So on the Responses path the correlation is always available
  before the first delta needing it.
- **Decision — variant carries `call_id: Option<String>`, not bare `String`.** The
  `ProviderEvent::ToolCallDelta` variant is shared across three provider families with
  *different* delta/id ordering. Responses (always `Some`) and Chat Completions (id on the
  first chunk of each call, `Some` thereafter) both surface it; Anthropic's incremental
  `input_json_delta` fragments arrive with no id in the same event, so forcing a non-optional
  field would require fabricating an empty string there — exactly the "pad with empty string"
  anti-pattern the SSE mapper elsewhere refuses. `Option` keeps the absence honest and lossless
  (the delta still streams; only correlation is unavailable). The Responses path is documented
  and guaranteed `Some`.
- **Correlation lives in the stateful `ResponsesMapper`** (`provider/openai/execute.rs`): it
  holds an `item_id → call_id` map populated from `output_item.added`, stamps every
  `ToolCallDelta` produced by the (still stateless) `map_sse_event` dispatcher, and clears the
  map at response end (terminal `Done`/error) so nothing leaks across responses. A per-response
  fresh mapper is created per `execute()` call, so cross-response leakage was already impossible;
  the clear is belt-and-suspenders and honors the explicit lifecycle.
- **FOLLOW-UP flagged for owner (Fable review, 2026-07-04):** the Anthropic integration path
  (`integration/claude/adapter.rs`) emits `call_id: None` on its incremental `input_json_delta`
  fragments because that event carries no id. This conforms to the Option decision and is honest,
  but the same "announcement precedes deltas" structure exists on Anthropic
  (`ContentBlockStart{ index, ToolUse{ id }}` precedes `ContentBlockDelta{ index }`), so an
  index→id map — the exact `ResponsesMapper` technique — *could* make Anthropic deltas correlate
  too (and would also fix the pre-existing `item_id: String::new()` on those fragments). Not done
  here (meridian consumes the Responses path, and the Anthropic `map_claude_event` is a stateless
  free function that would need threading state). Recommend owner rules whether to close this
  asymmetry as a follow-up. Silence = leave Anthropic deltas uncorrelated (`None`).

### C8 — granular `AgentBuilder::context_window_limit(u64)` setter

- **Decision — no `AgentConfigPresence` flag.** `context_window_limit: Option<u64>` carries its
  own presence (`Some` = explicitly set); the merge already overlays it via
  `explicit.context_window_limit.is_some()` (`agent/assembly.rs`), so the setter is a one-liner
  mirroring the `max_iterations`/`step_timeout` `Option`-field setters. Unlike `.agent_config()`
  (which marks the whole config explicit via `AgentConfigPresence::all()`), the granular setter
  overrides only this field and leaves the settings-derived runtime base intact. `arm_auto_compaction`
  fills the catalog window only when the merged value is still `None`, so the setter's value
  survives arming unchanged.

### C4 — live compaction event for embedders

- **Decision — new `AgentEventKind::Compaction(AgentCompaction)` variant.** `ProviderEvent::Compaction`
  is an unrelated opaque OpenAI Responses stream item (not auto-compaction) and is the wrong shape;
  a dedicated typed payload is added rather than reusing it. The event carries honest accounting:
  `tokens_before`/`tokens_after` (estimator counts either side of the rewrite), `events_compacted`
  (count of newly-superseded events), `summary_source` (`llm` vs `mechanical_digest_fallback` with
  its error), the summarization call's real `Usage`, and the compaction event id.
- **Completion-only, no "begin" event.** The two problems C4 names — an unexplained history
  rewrite and invisible summarization spend — are both resolved only *after* the summarization
  round-trip completes (before/after token deltas and the real `Usage` do not exist until then).
  Pre-turn compaction is synchronous and holds the turn, so a "compacting…" begin marker would
  carry no accounting and interleave nothing. Emitting a single completion event at the point the
  rewrite lands is the honest, minimal contract; a `Started` phase can be added later without
  breaking it.
- **Summarization sub-call stays event-tx-free.** The event is broadcast from the preflight caller
  (`loop/inflight_compaction.rs`), not by threading `event_tx` into `summarization.rs` — the
  summarization provider call's own `Done`/`TextDelta` events must never pollute the agent stream
  (preserved). Wired only through the auto-compaction path (pre-turn preflight, which is also the
  in-flight application site). The manual `/compact` CLI/TUI surfaces run outside the agent loop
  with no live subscriber and are out of scope (confirm with owner if UI coverage is wanted there).

### C1 — live event for unsequenced inbound injections

- **Decision — reuse `AgentMessageLifecycle::Delivered` (no new event kind), with two shape
  changes.** All injection paths (cron `norn:cron`, process-manager `norn:process-manager`, watch
  `norn:watch`, generic embedder sends) already funnel through
  `inject_inbound_messages` (`loop/delivery.rs`); the fix removes the `seq.map` gate so the same
  dual-carrier Delivered (durable audit `SessionEvent::Custom` + live broadcast) fires for every
  injection, not just router-sequenced ones.
- **`Delivered.seq: u64 → Option<u64>`.** Unsequenced injections have no router seq; fabricating
  one (e.g. `0`) is an arbitrary invented value. The existing shape genuinely cannot express an
  unsequenced delivery, so the field becomes optional (`None` = unsequenced). NO BACKWARDS COMPAT:
  the shape is mutated, not duplicated.
- **Add `Delivered.from: String`** (the sender label, mirroring `Sent.from`). Internal sources use
  `sender_id = Uuid::nil()`, so `from_id` alone cannot attribute the stimulus; the human label
  (`norn:cron` etc.) is what makes the live event self-describing — the whole point of C1
  (transcripts no longer show responses to invisible stimuli). Populated from `ChannelMessage.from`
  on both sequenced and unsequenced paths.
- **Aggregation follow-through (Fable review, 2026-07-04):** the `agents` `messages` edge
  projection (`tools/agents_messages.rs`) now folds the delivered `from` label into the edge when
  no `Sent` supplied one, so cron/watch/process edges render `norn:cron` instead of the bare nil
  UUID. Note (deliberate, minor): all nil-sender sources to one recipient collapse into a single
  `(nil, to_id)` edge that shows the first-seen label; distinguishing cron from watch on that
  projection would need the label folded into the edge key — left as-is (secondary tool; the live
  event and the persisted audit both carry the exact source). The edge's `last_seq` stays `0` for
  an unsequenced-only edge; this is an honest sentinel, since router-minted seqs start at `1`.

## 7. Responses API remediation decisions (2026-07-10)

### P0 D1 — credential, workspace-authority, and private-artifact boundary

- **Codex OAuth has one compiled destination.** No `base_url` override, or a
  normalized spelling of `https://chatgpt.com:443/backend-api/codex/` with the
  default port and trailing slash omitted or present, selects the Codex
  subscription backend. Accepted input is discarded and the request uses the
  compiled `https://chatgpt.com/backend-api/codex` value. Every other OAuth
  destination is rejected before auth initialization or network I/O.
- **No trusted proxy exists in P0.** Repository settings, user settings, and CLI
  overrides cannot extend the OAuth allowlist. Proxy support requires a later
  provenance-preserving security design and a separate owner decision.
- **Working-directory settings cannot grant runtime authority or sensitive side
  effects.** `<cwd>/.norn/settings.json` and
  `<cwd>/.norn/settings.local.json` remain untrusted even when gitignored. Raw
  presence is rejected before merge even when a later CLI value would replace
  it, and the same validation runs in CLI and shared-library loaders. In both
  layers:
  - top-level providers and provider profiles may not set `base_url`,
    `api_key_env`, `auth`, `debug_dump_dir`, `runner_path`, free-form
    `provider.options`, or profile `api_shape`;
  - model aliases may not select `provider_profile` or `api_shape`, and a
    working-directory default model or bare-name workspace-profile model may
    not activate a backend-selecting alias from a trusted layer; same-name
    alias/profile collisions cannot borrow a trusted backend bundle, and an
    explicit CLI model remains trusted;
  - no non-empty shell-hook slot is accepted;
  - `variants.<variant>.prompt_file` is rejected before eager file loading,
    while inline prompts remain supported; and
  - `tools.skill.shell_execution = true` is rejected, while an untrusted layer
    may narrow authority by setting it to `false`.
- **Raw settings layers are not a public authority surface.** Loading,
  provenance validation, and merge form one compiler-enforced transaction.
  Mechanical raw-layer access and merge become crate-internal; advanced
  embedders receive only a sealed validation witness with private fields.
  Documentation telling callers to remember validation is not a security
  boundary. The current candidate implements this refinement; phase acceptance
  remains gated on whole-phase Gate D.
- **Source-bearing automation retains provenance.** A single rule scan records
  whether a rule came from user configuration or the working directory.
  `shell_source` is rejected from `.norn/rules`, `.claude/rules`, and
  `.meridian/rules`; static working-directory rules remain supported. Bare-name
  workspace profiles may be static but may not declare `prompt_commands`, and
  a rejected workspace profile does not fall through to a same-name user
  profile. The root loop and spawned-agent paths enforce the same rule.
  User-level rules and profiles, programmatic hooks, explicit profile paths,
  and explicit `--rules` files remain trusted surfaces.
- **The workspace root is immutable after launch.** The builder canonicalizes
  the working directory once and forwards that root through root, spawn, and
  fork assembly. Automatic settings, root/nested `NORN.md`, rule, profile,
  capability, skill/resource, variant, and `CONVENTIONS.toml` reads are resolved
  against it rather than a mutable process CWD. User-configured paths that
  physically resolve beneath the workspace are normalized at launch so a
  re-pointed alias cannot change trust tier.
- **Workspace reads on supported descriptor-capable Unix targets are
  descriptor-relative and no-follow.** Every path component is opened without
  following links, final inputs must be regular files, and directory enumeration
  stays pinned to the opened directory.
  Repository symlinks are rejected even when they point elsewhere inside the
  same repository. Alternate physical spellings such as macOS `/var` and
  `/private/var` are recognized without canonicalizing/following the final
  candidate. The sole scoped exception is validated `.git` metadata needed for
  branch/commit display; it does not grant a general read path.
- **Redox, ESP-IDF, and non-Unix workspace input currently fail closed.** There
  is no weaker compatibility fallback to ordinary link-following reads. This is
  an intentional release limitation until equivalent platform primitives are
  implemented and reviewed.
- **Trusted roots are absolute.** Relative `HOME`, `NORN_HOME`, `CODEX_HOME`,
  user prompt files, and user search paths cannot become credential/config
  authority relative to a repository-controlled current directory.
- **Repository-to-process confused deputies are disabled.** Workspace-sourced
  skills never execute shell expansion, regardless of a user-global enable bit.
  Workspace `CONVENTIONS.toml` retains non-process LOC and pattern checks but
  loses `lsp`, `diagnostics`, `remediation`, and `reports` process categories.
  Any profile selected by the model is rejected if it carries
  `prompt_commands`, including a trusted user profile. These are intentional
  compatibility breaks; restoring them requires a provenance-preserving
  consent design and an owner decision.
- **There is no implicit repository-command consent.** Re-enabling hooks,
  `shell_source`, or profile prompt commands from a working directory requires
  a later provenance-preserving consent design and a separate owner decision;
  ordinary merge precedence is not consent.
- **Trusted custom API-key endpoints remain supported.** They must parse as an
  HTTPS URL with a non-empty authority and no userinfo, query, or fragment.
  Plaintext HTTP is allowed only for exact loopback hosts (`localhost`, loopback
  IPv4, or loopback IPv6). API-key authentication cannot select the private
  compiled Codex endpoint. Remote plaintext support requires a later explicit
  insecure-transport design and owner decision.
- **Credential-bearing clients do not follow redirects.** Responses, compatible
  Chat Completions, OAuth refresh, revoke, and authorization-code exchange
  clients all return the original 3xx response without replaying credentials,
  account headers, or request bodies to the redirect target.
- **Generic injected-auth provider construction remains unit-test-only.** The
  previously public constructors could pair an OAuth credential provider with
  an API-key configuration and custom destination, bypassing the auth/backend
  matrix. Production embedders may instead supply only a validated sealed
  static Codex credential through a constructor requiring OAuth/Codex backend
  identity and the compiled ChatGPT destination. It accepts neither an arbitrary
  `AuthProvider` trait object nor caller-selected token/request authority. Until
  P2 defines acknowledged credential-owner sinks, the P0 static path cannot
  refresh or rotate; a 401 returns a typed owner-refresh requirement. The
  constrained replacement is implemented and covered by an in-repository
  public-API fixture; Meridian migration remains separate downstream evidence.
- **Children inherit provider authority; model text is not authority.** Spawned
  and forked children reuse the parent provider instance. Profile, variant, or
  model-selected strings may select request model behavior but cannot reconstruct
  a provider, resolve a backend-bearing alias, read a credential environment
  variable, or change endpoint/auth mode. A real-entry regression covers
  variant, workspace-profile, and fork paths against a hostile backend alias.
- **Arbitrary OAuth token-authority constructors are unit-test-only.** Enabling
  the public `test-utils` Cargo feature cannot expose constructors that load or
  hold OAuth credentials while selecting a custom refresh-token destination.
- **Raw API dumps are trusted opt-in artifacts.** Only user-level settings or an
  explicit CLI option may enable them. On supported descriptor-capable Unix
  targets, the final target must be a non-symlink regular file opened without
  following symlinks and forced to mode `0600`; non-regular targets fail closed.
  Unsupported targets fail before artifact I/O.
- **Sensitive persistence artifacts share one private policy.** Session
  data/index/lock/atomic-temporary files, full-output session spools, foreground
  threshold-output logs, background-process spools, and persistent task records
  can contain prompts, reasoning, tool arguments/results, or process output. On
  supported descriptor-capable Unix targets their directories are `0700`, files
  are regular `0600` targets, and every ancestor and final component is traversed
  descriptor-relatively without following links. Redox, ESP-IDF, and non-Unix
  targets return `Unsupported` before artifact I/O. Links/non-regular targets
  fail closed across create, reopen, rewrite, and resume. Sensitive persistence
  has no relative `.norn` fallback; missing absolute configured/trusted roots are
  typed failures.
- **Concurrent same-UID replacement has a portable confinement boundary.** A
  link or non-regular entry present when an operation begins is rejected. If a
  same-UID process replaces the final name between validation and a POSIX
  `*at` mutation, the operation remains pinned to the opened parent descriptor,
  never follows the replacement, never reads/writes/deletes its outside target,
  and either fails or affects only the raced entry inside the private root.
  Portable POSIX APIs cannot promise strict rejection or serializable updates
  against that actor; tests therefore assert outside-sentinel preservation and
  in-root confinement rather than an impossible no-race claim.
- **Artifact hardening is implemented but not yet accepted.** The current
  candidate routes the P0 artifact family through one descriptor-pinned,
  no-follow private-root primitive and removes relative fallbacks. Targeted
  adversarial tests are green, and the independent private-artifact closure
  review reports `READY` on its owned surface. Whole-phase Gate D remains
  required before `SEC-15` can close.
- **Credential diagnostics are structural within the credential-bearing
  runtime/request boundary.** OAuth tokens, PKCE material, user/account claims,
  free-form request options, and sensitive runtime provider state are absent
  from their `Debug`; every response-header value is redacted; OAuth and
  provider errors retain locally authored structural classification, never raw
  authority/provider text. On the generic error-status path, non-redirect bodies
  are streamed and discarded under the existing request timeout, preserving
  stalled-body timeout/retry semantics. Specialized 401 refresh and 429 backoff
  paths drop their bodies without draining; redirects are classified and dropped
  immediately. The legacy raw provider-settings container still derives
  `Debug`; no reachable logging call was found, but P0 does not claim that type
  is safe to log.
- **Unknown provider terminal discriminators remain distinguishable without raw
  provider text.** Known values use typed mappings. An unknown exact byte
  sequence is represented only by its terminal category and a domain-separated
  full HMAC-SHA-256 tag under an OS-random process-lifetime key. The key and raw
  value are never persisted or logged. The deterministic-key test seam is
  crate-private and compiled only under `cfg(test)`; production exposes no
  public, configuration, or environment override. OS-random initialization
  failure is a typed fail-closed diagnostic error, never a fixed-key fallback.
  Tags support equality only within one process and are not durable cross-run
  fingerprints. Raw value and byte length are not exposed. This is D1A.
- **Response-header correlation remains deliberately redacted.** D1 applies to
  every response-header value, including upstream request IDs. `NF-4` is an
  accepted diagnostic limitation, not permission to expose one header without a
  later owner decision and a separate non-disclosure design.
- **Redirect refusal is explicit.** A 3xx from a credential-bearing request is a
  terminal policy refusal naming the status and no-follow rule, never an
  ordinary stream error. `Location` and response bodies remain undisclosed.
- **Dormant authority is not pre-approved authority.** `mcp_servers` and its
  environment map remain merged but have no production runtime consumer. Any
  future wiring must add source provenance and explicit consent first.
- **Known filesystem residuals stay explicit.** Workspace text reads remain
  unbounded pending an owner-approved streaming/size policy. Public `Scanner`,
  `scan_rule_dirs`, and `discover_skills` convenience APIs are trusted-input-only;
  secure runtime assembly does not route repository-controlled roots through
  them.

The owner approved execution of the staged remediation plan on 2026-07-10; this
section records the P0 policy and candidate boundaries. Three provisional
reports reviewed frozen snapshot `7d121c9` and drove the closure round. Targeted
credential/config, transport/streaming, and private-artifact re-reviews now
report `READY` on their owned surfaces. The candidate is committed through
`ebb82c8`; the complete machine-gate suite and manual policy audits are green
and recorded in `reviews/2026-07-11-p0-gate-c-handoff.md`. The added-line audit
reports zero campaign-added unwrap, unwrap_err, expect, expect_err, or panic
calls and no new/widened lint suppression. The separately reported
exchange-review artifact remains absent. The plan now exposes seven open
progress records: two retrospective Gate A claims, two Gate B traceability
records, two missing Norn fixtures, and separate downstream Meridian evidence.
The two Gate A timing claims cannot be repaired retroactively; P0 cannot receive
`READY` under the current non-negotiable gate text unless the owner records an
explicit P0-only process exception. Whole-phase Gate D has not occurred, and P0
acceptance remains pending.

## 8. OAuth multi-account direction (2026-07-11)

- **Explicit named accounts are a conditional P2 objective.** The owner wants to
  sign in to multiple ChatGPT/Codex accounts and select one without overwriting
  another. D9 Gate A selects one branch. The supported branch requires an
  owner-approved live validity experiment before implementation. If independently
  stored credentials remain valid, P2 designs Norn-owned named storage plus
  explicit login, list, use, status, and logout. Invalidation returns D9 to Gate
  A for the unsupported-branch decision. If live approval/evidence is unavailable,
  the owner may select the unsupported branch directly: Norn removes the named
  surface and returns a typed documented unsupported outcome without claiming
  that the provider technically invalidates isolated credentials. The existing
  `--codex-home`/`CODEX_HOME` mechanism is only a low-level isolated-path
  primitive, not the final account model.
- **Simultaneous token validity is evidence, not an assumption.** The current
  Norn/Codex shared-file integration represents one cached login, and the owner
  reports that logging into another account can invalidate prior tokens. An
  opt-in, redacted
  two-account live experiment must establish whether independently stored
  credentials remain refreshable before Norn claims concurrent named-account
  support or rotation.
- **Account selection is trusted authority.** It changes billing identity,
  entitlements, workspace policy, retention, and potentially data residency.
  Only explicit CLI, a Norn-owned active selection, or trusted user
  configuration may select it. Repository configuration, model-selected
  profiles, prompts, tools, and model output cannot choose or broaden accounts.
- **Providers pin one credential identity.** `auth use` affects newly
  constructed providers only. An in-flight provider, turn, agent, or resumed
  session cannot silently switch identity. Until P5 binds an opaque identity to
  persisted session state, every resume requires an explicit trusted account
  choice and may not consume an active-account default.
- **The Codex-shared source is specifically file-backed.** P2's foreign-writer
  contract applies to `$CODEX_HOME/auth.json`. Current Codex can alternatively
  [store credentials in the OS credential store](https://learn.chatgpt.com/docs/auth#credential-storage);
  D9 must select a safe library interface before Norn claims keyring support,
  otherwise it remains out of P2 scope. Norn never scrapes or shells out to
  recover keyring secrets.
- **Local status does not claim remote usability.** The side-effect-free state
  model is missing, malformed, access-expired, refresh-candidate, locally-valid,
  or unknown. Refresh conflict and undurable persistence are typed operation
  outcomes unless D9 separately approves a durable recovery journal. Doctor may
  perform a separately authorized active probe, reported separately from local
  classification.
- **Automatic rotation is not a P2 feature.** It remains `ROUTE-01`, owned by
  P6 after the P3 transcript and P5 turn-state decisions. The current
  [OpenAI Terms of Use](https://openai.com/policies/terms-of-use/) prohibit
  circumventing rate limits or restrictions, so exhaustion-triggered rotation
  remains unsupported unless an authoritative product/contract determination
  says this behavior is permitted. Even then, a switch may occur only before
  dispatch or after a typed provider outcome guaranteeing no execution or state
  mutation; absence of observed output is insufficient. It also requires a
  trusted allowlist, clean account-scoped state, cache isolation, and explicit
  resume rules.
- **D9 remains open.** Storage layout, alias-to-opaque-ID mapping, migration of
  the file-backed foreign Codex source, keyring scope, unknown-expiry policy,
  static/embedder refresh ownership, accepted `provider.auth` spellings and
  companion fields, and the live-validity branch require Gate A decisions and
  evidence before implementation.
- **D10 remains open.** Automatic rotation requires both authoritative permission
  under the governing product/contract terms and a pre-dispatch or guaranteed
  non-execution state-machine proof. If either is absent, `ROUTE-01` closes as an
  explicitly unsupported capability rather than an implicit retry behavior.

## 9. P0 Gate D corrective rulings (2026-07-11)

- **Fetched content is private session state.** New fetched documents must never
  be written beneath the working directory. They belong to an immutable
  artifact subtree owned by the active root session under the trusted user-level
  Norn store. Built-in read/search may receive a narrow read-only capability for
  that active artifact subtree; this does not expose credentials, the global
  session index, raw child transcripts, or other sessions. P0 establishes the
  scope and migrates new fetch writes. P3 decides broad transcript/storage
  migration, historical references, fork copying, and retention.
- **File-descriptor exhaustion is a P0 availability boundary.** The official
  CLI must raise its inherited soft `RLIMIT_NOFILE` only to a finite
  OS-reported ceiling, report inherited/effective limits and a labelled current
  descriptor snapshot through `doctor`, and preserve typed process-local
  `EMFILE` versus system-wide `ENFILE` errors through the inventoried P0 private,
  session, and process paths. Library embedders opt in to process-global
  hardening. Raising the limit is mitigation, not permission to leave avoidable
  per-agent descriptors permanently open; sharing or lazy reopen remains a
  separately owned follow-up.
- **Core-dump hardening remains separate.** Setting both soft and hard
  `RLIMIT_CORE` to zero would also disable traditional core dumps for commands
  spawned by Norn. It is not implied by the NOFILE ruling and remains open until
  that inherited behavior is explicitly accepted.
- **Raising `RLIMIT_NOFILE` is no longer sufficient closure.** On 2026-07-13
  the owner rejected residual Norn-owned descriptor growth as an acceptable
  risk. Idle retained objects must converge to zero descriptors where secure
  identity-verified reopen is possible; local lock contenders must wait before
  opening; descriptor-heavy active work must share one weighted process-wide
  admission authority and release capacity on every terminal path. Evidence
  must run in re-executed low-limit children so the parallel test runner's
  process limit is never mutated. The guarantee is scoped to Norn-owned
  descriptors: unrelated embedder code and system-wide `ENFILE` remain outside
  Norn's control and must still surface typed diagnostics.
- **Gate evidence is distributional and enumerable.** Concurrency-sensitive
  claims require a checked-in repeatability command or script and the complete
  pass/fail distribution. The macOS convergence correction requires 50 recorded
  repetitions. Claims using `all`, `every`, or `complete` include the exact
  generated inventory they quantify. A single successful suite invocation is
  an observation, not stability evidence.
- **Dormant MCP settings will become the real layered client surface.** The
  `mcp_servers` schema is retained and completed, not removed. User, shared
  project, private project-local, per-agent, CLI, and live-session scopes are
  all supported under the contract in section 10. P0 must preserve source
  provenance and prove that a shared project definition is not activated
  before its remembered project approval; subsequent MCP slices complete
  startup consumption and live mutation.

## 10. Layered and live MCP client direction (2026-07-13)

**Owner ruling:** Tom confirmed this product direction on 2026-07-13. In
particular, MCP roots communicate working context rather than a filesystem
security boundary, and Norn must support machine-wide capability with
user-, project-, agent-, CLI-, and live-session-level customization.

- **MCP is a first-class runtime capability.** Norn keeps the existing stdio
  and HTTP client, tool discovery/proxying, `mcp connect`, and `mcp serve`, and
  wires configured clients into normal agent execution. The dormant settings
  surface is not deleted merely because its consumer is incomplete.
- **Configuration is layered and ergonomic.** Server definitions may come from
  user scope, shared project scope, private project-local scope, explicit CLI
  input, or the live session. Same-name precedence is
  `session > CLI > local > project > user`; replacement is whole-definition so
  command, arguments, environment, URL, and headers never combine accidentally
  across scopes. This follows the useful local/project/user shape established
  by other coding agents while adding the live session as an explicit scope.
- **Shared project servers use remembered approval, not prohibition.** A
  checked-in project definition is visible immediately and may be approved for
  the canonical project. Approval is keyed to the project and normalized
  server definition; changing the executable, arguments, environment, URL, or
  headers asks again. User, private local, CLI, and live-session definitions
  are direct operator actions and do not require a second project approval.
  Headless mode surfaces an actionable pending-approval notice instead of
  hanging; an explicit command can approve it.
- **Agents receive independently selectable views.** A root agent may select
  all enabled servers or a named subset. Variants and spawned children inherit
  by default and may select a different subset from the session's connected
  pool; forks inherit their source agent's current view. This is customization,
  not filesystem confinement: an MCP server may work across the computer as
  its own configuration and operating-system access allow. A child changing
  its view does not mutate the parent's view. Tool names are server-qualified
  so two servers may expose the same leaf tool without nondeterministic
  replacement.
- **Live changes are session operations.** List, inspect, add, remove, enable,
  disable, approve, and reload must be available without editing files or
  restarting the whole application. A live definition is ephemeral unless the
  operator explicitly chooses a persistent scope. Adding or removing a server
  refreshes both dispatch and the provider-visible tool catalogue at the next
  request boundary; an in-flight tool call retains its connection until that
  call completes.
- **Project roots follow agent context.** Each MCP client exposes the agent's
  current workspace roots and can notify a capable server when they change.
  Roots communicate intended project context rather than pretending to be an
  operating-system sandbox, matching the MCP roots model. Norn does not use an
  MCP root to prohibit a user-selected server from accessing other machine
  locations.
- **Implementation is sliced, not deferred indefinitely.** The first slice
  preserves provenance, validates and redacts definitions, implements approval,
  connects selected startup servers, and installs per-agent qualified tools.
  The second slice adds live catalog mutation and provider-tool refresh. Later
  protocol work may add OAuth, elicitation, sampling, resources, and prompts
  without reopening the scope model.

Reference behavior: the MCP specification describes roots as dynamically
changeable project/workspace context, and Claude Code demonstrates practical
user, local, and shared-project server scopes with approval for shared project
configuration. Norn adopts those useful interaction patterns without treating
roots as an enforcement boundary.

**Implementation status (2026-07-13):** the first startup slice is implemented
as a review candidate. It preserves all five sources, fingerprints normalized
definitions, supplies `mcp list/approve/revoke`, keeps unapproved project
servers inactive, independently connects direct and approved stdio and
Streamable HTTP servers, negotiates the current `2025-11-25` protocol, handles
JSON or SSE POST responses and server ping requests, follows paginated tool
discovery, and installs server-qualified tools through an owned runtime.
Root, variant, and spawned-agent settings can select startup views from that
pool. Live session mutation, request-boundary tool-catalogue refresh, connecting
a child-only server outside the current pool, dynamic roots, HTTP GET listening,
reconnection/resumption, and DELETE shutdown remain open and are not claimed by
this slice.

**Subsequent implementation status (2026-07-14):** the P0 live-control slice
now implements serialized add/remove/enable/disable/reload operations,
request-boundary generation/runtime publication, contextual roots, per-agent
views, provider-visible tool refresh, and bounded watcher/descriptor release.
Idle standalone HTTP GET notification listening, reconnect/resumption, HTTP
session DELETE, MCP OAuth, sampling, resources, and prompts remain outside this
slice. Whole-phase adversarial acceptance subsequently passed at `7ce29d7`.

**P0 transport correction contract (2026-07-14):** the whole-phase Gate D
correction keeps the owner-ruled usability model while making its resource and
failure behavior explicit.

- One inbound stdio JSON-RPC line, JSON HTTP body, or complete SSE wire event
  is bounded. The default is exactly 10 MiB, factually derived from the
  [`STDIO_DEFAULT_MAX_BUFFER_SIZE` in the official MCP TypeScript SDK v1](https://github.com/modelcontextprotocol/typescript-sdk/blob/v1.x/src/shared/stdio.ts#L2),
  and a positive `max_inbound_message_bytes` may override it per server. SSE
  framing is processed one event at a time; ignored fields and comments count
  toward the wire-event limit rather than providing an unbounded side channel.
- `request_timeout_ms` is optional and positive when present. Absence means no
  Norn-imposed request timeout, preserving the section 1 no-default-timeout
  ruling instead of replacing the removed 30-second constant with another
  invented number. An explicit deadline covers the complete logical request,
  including body parsing and SSE server-request handling, not merely socket
  reads. Both connection controls participate independently in definition
  fingerprints.
- A stdio server receives the normal inherited parent environment plus its
  configured overlay. This preserves ordinary machine-wide command usability;
  the configuration and diagnostic surfaces still expose only key counts or
  presence, never values.
- Stdio stderr is drained under descriptor governance but is never retained as
  text, bytes, a digest, a length, or a count. Failures may report only the
  fixed categories `no content observed`, `one withheld line`, or `multiple
  withheld lines`, together with completed/interrupted/read-error state. This
  is useful failure shape, not recovery of the server-authored diagnostic.

## 11. P0 retrospective exceptions and pinned Rust gate (2026-07-14)

**Owner ruling:** Tom approved all three dispositions in-thread on 2026-07-14.

- **Gate A receives a P0-only retrospective process exception.** The owner
  decisions and implementer/reviewer evidence-method agreement were not
  durably recorded before P0 implementation began. The two historical Gate A
  boxes remain unchecked because later approval cannot make those timing
  claims true. For P0 only, this exception substitutes for them when Gate A and
  the universal exit gate are evaluated. It creates no precedent for P1 or any
  later phase.
- **Gate B receives a P0-only baseline-evidence exception.** Where Git contains
  no native pre-fix executable regression, P0 may use the retained source or
  positive-characterization proof together with the exact candidate
  regression. The native macOS `openat` defect-red/corrected-green sequence
  remains required. This exception waives no implementation, regression,
  lint, LOC, secret review, Gate C, or Gate D requirement, and the historical
  Gate B checkbox remains unchecked.
- **The repository Rust gate is pinned to 1.94.0.** `rust-toolchain.toml`
  selects Rust 1.94.0 plus the required Clippy and rustfmt components. This is
  the exact toolchain on which the strict P0 gate passed, so plain repository
  commands no longer inherit a floating local `stable` alias. Toolchain
  upgrades are explicit reviewed changes. No lower minimum Rust version is
  claimed until one is separately proven and encoded.

## 12. P0 historical evidence disclosure disposition (2026-07-15)

**Owner ruling:** Tom approved deletion of the exact six superseded schema-v2
evidence reports below from current `HEAD`. They contain local username/path and
tool-location metadata, removed ambient-variable names, fixed sanitized build
values, and value hashes, but no values of the removed ambient variables and no
credentials. The path-free correction bundle replaces them as current machine
evidence: gate and distributions use schema v3; policy and attestation use
schema v2.

| Artifact removed from current `HEAD` | Introducing package | SHA-256 |
|---|---|---|
| `docs/reviews/evidence/2026-07-14-p0-final-distributions-bfa0b8e.json` | `e9b02d0` | `c625f79668441f59c27a5b168a0aba1180562aac146d9965db2d428b95c37d9a` |
| `docs/reviews/evidence/2026-07-14-p0-final-gate-82e44f4.json` | `e9b02d0` | `95e126ba12e558d049c91791289ffa2f2622abc57a7f9cc4c20ef71263541b6d` |
| `docs/reviews/evidence/2026-07-14-p0-final-gate-bfa0b8e.json` | `e9b02d0` | `d42429d66a0e4c68fee5cb45ba09e074fcf51d40da26eecfe0ed519090e81856` |
| `docs/reviews/evidence/2026-07-14-p0-toolchain-13d661c/distributions-native.json` | `564af2d` | `0153db4715a8b5cc72181a9c1deb7988312ce75ac508d8359fed297f155b4c14` |
| `docs/reviews/evidence/2026-07-14-p0-toolchain-13d661c/gate-native.json` | `564af2d` | `b75fae12d37c8414b4ed1a83234054294cb1b094e80f16cb3e200946ca3b69d1` |
| `docs/reviews/evidence/2026-07-14-p0-toolchain-13d661c/gate.json` | `564af2d` | `878165a0fccce565cf318dda939a24cec823618d8071dcd737f821692509d911` |

- Historical documents retain the plain filenames, introducing commits, and
  content hashes, but do not link to absent current-head files.
- The files remain retrievable from their introducing commits. This disposition
  does not rewrite or purge already-pushed Git history.
- Current acceptance claims use only the path-free schema-v3 gate/distribution
  artifacts and path-free schema-v2 policy/attestation artifacts at source head
  `e1bf7f2`.

## 13. P0 acceptance and final review dispositions (2026-07-15)

**Owner record:** Tom confirmed P0 is accepted after the focused independent
Gate D review committed at `7ce29d7` returned `READY`. The accepted source head
is `e1bf7f2`; evidence and documentation packaging runs through `1096628`.

- The review independently reproduced Gate C 38/38, distributions 830/830, the
  full-range policy, and zero-error attestation, and completed the deferred
  whole-diff seam sweep. GD-1 through GD-18 are closed.
- SS-1 confirms, rather than changes, section 10: an MCP server explicitly put
  in `.norn/settings.local.json` by the operator starts without a second
  approval prompt. Rejecting a git-tracked local file remains optional
  hardening and is not adopted as a P0 or P1 requirement.
- The pre-existing invented `EXTENSION_TIMEOUT = 30s` is non-gating for P0 and
  is assigned to P6's transport configurability work so it is not lost.
- The three historically false P0 Gate A/B timing and baseline boxes remain
  unchecked under the section 11 P0-only exceptions. Acceptance does not
  rewrite those historical facts or create an exception for P1.
