# Decisions for Owner Sign-off — Norn Hardening Campaign (2026-07)

This file consolidates every decision the Wave 1 hardening-campaign agents recorded while
implementing (tracks T1-T9) and integrating (seams I1-I3) the `hardening/final-state` briefs.
Nothing here was invented by this document — every line is drawn from an agent's own
structured `decisions` report at the end of its run. Wave 2's planning journal
(`wf_5c41d368-666`) recorded a brief plan, not implementation decisions, so it contributes
nothing to this list.

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
  I1-error-taxonomy-reasoning). Deliberately out of scope (`session/**` was allowed for literal
  fallout only); resumed sessions rebuild with empty reasoning and the next turn regenerates it.
  Agent flagged: *"for a follow-up decision if resume-time replay is ever wanted."* — **Discuss**
- **`loop/runner.rs` exceeds the CLAUDE.md 500-LOC module limit** (911 lines at last check, 852
  at HEAD; seam I2-loop-agent-seams). The overage is dominated by `run_agent_step_inner`, a
  single ~650-line function (lines 388-1264) that IS the core step loop and is called into by
  two other concurrently-owned files (`assembly.rs`/`classify.rs`). The agent declined to split
  it mid-flight and reported it instead as **a blocker for a dedicated exclusive-ownership
  pass**, rather than risk an unplanned rewrite of code every other track integrates against.
  This is an open CLAUDE.md-compliance gap, not a design question. — **Discuss**
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

## 4. Held items (from `docs/HOLD-FOR-DISCUSSION.md`)

These two items were deliberately **not** wired and **not** deleted anywhere in the campaign;
nothing in Wave 1 or Wave 2 touched them (confirmed above — the I3 seam explicitly skipped the
one remaining rustdoc warning in `monitor.rs` to honor the hold).

1. **`RunMonitored` — AI-monitored background tasks** (`crates/norn/src/agent/monitor.rs`).
   Exported scaffolding, zero production callers, unused `_provider` parameter, static-string
   heartbeat. Vision intent: long-running commands/sub-agents watched by a cheap model instead
   of consuming the parent's context. Three options on the table: wire it properly (model/config
   from the builder, real heartbeat, query interface, alert routing via `MessageRouter`), delete
   until scheduled (reviewer's recommendation), or redesign first (the wake/linger +
   `signal_agent` + delegation-budget machinery that landed since may supersede a bespoke
   monitor type). — **Discuss** (owner design decision required)

2. **`ToolEnvelope.runtime_inputs` + `ToolContext.runtime_args`**
   (`crates/norn/src/tool/envelope.rs`, `crates/norn/src/tool/context.rs`). `RuntimeInputs`
   always default, zero readers; `runtime_args` has no writer and no reader. Vision intent: a
   third envelope section (inbound messages, diagnostics, filesystem/working-tree changes since
   the last tool boundary) delivered to the model without explicit conversation injection, plus
   runtime-injected policy arguments. Held because the inbound-channel + `MessageRouter` +
   rules-engine work that landed since the vision was written overlaps heavily with this design
   — a real architectural fork (envelope vs. existing message-injection paths) that needs a
   decision, not a default. — **Discuss** (owner design decision required)

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
  (`step_timeout` graceful redesign) to two explicit "needs sign-off" search-tool behaviors and
  one CLAUDE.md 500-LOC compliance gap (`loop/runner.rs`) that was deliberately deferred rather
  than risked mid-integration.
- **Section 4 (held):** 2 items, untouched, as before.
