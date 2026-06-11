# Norn — Thorough Review (2026-06-11)

**Method.** Ten parallel reviewers (six subsystem deep-dives: loop core, provider, file tools, exec/orchestration tools, session persistence, agent management + CLI/TUI; four cross-cutting audits: concurrency, failure modes, security, plus architecture). Every critical/high claim was then independently re-derived by an adversarial verifier instructed to refute it by re-reading the code, callers, and error paths. 29 critical/high claims were verified this way — **all 29 confirmed** (several with severity corrections, one upgraded to critical). Four of the patch-tool bugs were additionally reproduced empirically with failing tests against the real tool. The single critical finding was also manually spot-checked. Medium/low findings (listed at the end) were *not* individually verified unless noted.

**Baseline.** Workspace compiles clean (zero warnings). Test suite: **1531 passed, 5 failed** — all five in `diagnostics_check`; root-caused below (pre-existing, not extraction-related).

---

## Critical

### C1. `--output-format stream-json` hangs forever after every run
`crates/norn-cli/src/print/orchestrator.rs:375`
`install_shared_agent_event_channel(&bundle.registry, tx.clone())` (line 349) stores an owned `broadcast::Sender` clone as an extension on the registry's shared `ToolContext`, which outlives the step. Dropping `root_sender` and `tx` (lines 373–374) therefore never closes the channel; the stream renderer's `rx.recv().await` never yields `Closed` (`print/output.rs:237`) and `handle.await` blocks forever. Every stream-json invocation completes the agent step then wedges instead of printing the final envelope and exiting. *(Verified + manually spot-checked.)*

---

## Verified high-severity findings

### Loop core
- **H1. Double-appended schema tool result corrupts the persisted session** — `loop/runner.rs:819`. In the `ToolsAndSchemaValid` arm, both `accept_schema_tool_call()` and `reject_post_schema_tools()` append an accepted `ToolResult` for the same `call_id` (`helpers.rs:237-250`, `269-282`). If the step continues, the next provider request carries a duplicate `function_call_output` → 400; the duplicate is persisted to the append-only store, permanently poisoning replay.
- **H2. Developer-message sync destroys compaction summaries on resume** — `loop/runner.rs:318`. The sync locates the *first* Developer message in history; after resume-with-compaction, that's the mid-history compaction summary, which gets overwritten (or deleted) by the dynamic context. The model silently loses all compacted history — on exactly the supported resume flow.
- **H3. `SchemaInvalid` leaves post-schema tool calls unanswered** — `loop/runner.rs:663`. Only pre-schema calls are executed; calls after the schema call get no result, so the next request is malformed → non-retryable 400 and the step errors out instead of retrying the schema. The sibling `ToolsAndSchemaValid` arm handles this; this arm omits it.

### Provider / network
- **H4. `ProviderConfig.timeout` is a silent no-op** — `provider/openai/mod.rs:123`. The documented, plumbed setting (settings → `-c request_timeout` → `ProviderConfig::timeout`) is only ever read by the `Debug` impl. `build_http_client()` sets no request/connect/read timeout and the SSE loop has no inactivity timeout; `step_timeout` defaults to `None`. A stalled connection hangs the turn indefinitely.
- **H5. 429 handling permanently *speeds up* the rate limiter** — `provider/openai/mod.rs:292`. `adjust_interval(retry_after)` replaces the replenish interval while `permits_per_interval` stays 60 — a header-less 429 (fallback 1s) turns 60/min into 60/sec, permanently, for the provider's lifetime. Back-pressure inverted into amplification; a unit test even asserts the inverted behavior.
- **H6. OAuth refresh has no single-flight guard** — `provider/openai_oauth/manager.rs:79`. The mutex is released across the network exchange, so concurrent expiry across shared-provider agents races the same refresh token. With token rotation, the loser's 401 classifies as Permanent → credential discarded → spurious forced re-login under normal multi-agent load; even without rotation, last-writer-wins can persist a superseded refresh token.
- **H7. MCP stdio transport permanently desyncs** — `integration/mcp_client.rs:199`. A timeout/cancel drops the rpc future after writing the request but before reading the response; the stale response stays buffered and the next call reads it as its own. `JsonRpcResponse` doesn't even deserialize `id`, so the off-by-one is undetectable and every subsequent call returns the previous call's result. Notifications on stdout break it the same way.

### File tools (all four reproduced with failing tests by the reviewer)
- **H8. Multi-block patch on the same file silently drops earlier blocks** — `tools/patch.rs:324`. Each Modify block stages full content from the *original* disk read; the last write wins. Reported `committed: true`, all hunks `applied: true`, but only the second block's change landed. Models routinely emit one section per change.
- **H9. Tier-2 context search patches the first match anywhere in the file** — `tools/patch_apply.rs:508`. The `@@` line is never used to disambiguate, there is no uniqueness check (unlike EditTool), and the trim-insensitive tier makes `}`-only context match almost anywhere. Reproduced: hunk targeting the second of two identical blocks applied at the first, committed silently (drift recorded, nothing gates on it).
- **H10. Claude Code patch format breaks on standard interleaved hunks** — `tools/patch_cc.rs:94`. Context lines between change runs are discarded, so the canonical ` ctx / -old / +new / ctx / -old / +new` shape fails to match (or worse, applies at a wrong contiguous match). The `@@ <anchor>` locator is discarded entirely.
- **H11. (Cross-cutting with H8/H9)** Commits are non-atomic truncate-then-write with best-effort rollback that can lose original content on ENOSPC — `tools/patch.rs:532`, and the same in-place `fs::write` pattern in `tools/edit.rs:292`. Patched files are also rewritten with forced LF endings + trailing newline (`patch_apply.rs:381`) — CRLF corruption.

### Process execution
- **H12. Bash tool can hang past its own timeout and orphan processes** — `tools/bash/mod.rs:184,196`. Timeout `start_kill()` signals only `sh`, not the process group; and the unconditional stdout/stderr drain awaits only finish at pipe EOF, which requires *all* inheritors of the write end to exit. `some-server & echo started` exits `sh` instantly but wedges the tool (and the loop) forever. Needs killpg + bounded drain.

### Agent management / wiring (the "silently configured, silently inert" cluster)
- **H13. Settings shell hooks silently dropped when programmatic hooks are present** — `runtime_init.rs:392`. `AgentBuilder::build` passes a clone of `self.hooks` into `load_runtime_base`, so `Arc::try_unwrap` is guaranteed to fail and the merge silently produces shell-hooks-only; then `self.hooks.or(runtime_hooks)` picks the un-merged programmatic registry — settings guardrail hooks never run. (The CLI copy of this same function logs a warning instead; confirmed drift between duplicated assembly paths.)
- **H14. `HookRegistry` never published on ToolContext in the builder path** — `agent/builder.rs:651`. `base.hooks.take()` at line 542 means `base.hooks.as_ref()` at 647–652 is always `None`, so `spawn_agent`'s `ctx.get_extension::<HookRegistry>()` finds nothing — subagent lifecycle hooks never fire for embedded agents. The CLI works only via its separate `publish_hooks_on_registry`.
- **H15. `signal_agent` mailbox fallback is a black hole** — `tools/agent/coord/signal.rs:163`. Falls back to `infra.mailbox.send(...)` and reports `routed_via: "mailbox"` success, but no production code drains agent mailboxes, and each root agent gets a fresh `Mailbox::new()` anyway, so cross-tree messages land in a different instance. Peer signals silently lost while the sender is told delivery succeeded.

### Security / config integrity
- **H16. The permissions consent boundary is parsed, merged, validated — and never enforced** — `config/types.rs:284`. `PermissionSettings` (allow/deny/ask) has zero runtime consumers; `tool_dispatch` gates only on hooks and tool pre-validate. `permissions.deny = ["bash(rm *)"]` provides zero protection. This is the only declared consent layer in the product.
- **H17. `--disallowed-tools` accepted, never enforced** — `norn-cli/src/runtime/builder.rs:208`. Parsed onto `RuntimeBundle.disallowed_tools`, read by nothing; advertised glob support also doesn't exist. Should gate the registry or hard-error as unimplemented.

### Session persistence / TUI
- **H18. Session index has no inter-process locking** — `session/persistence/io.rs:260`. Per-turn updates are unlocked read-modify-rewrite (rename-over) racing `O_APPEND` creation from other processes. Two concurrent norn processes can permanently drop a brand-new session's index entry — making it unlistable and unresumable (resolution is index-only) and failing that session's next turn with `NotFound`.
- **H19. Write-through sink swallows persist errors; strict reader bricks the session** — `session/store.rs:73` + `io.rs:48-52`. ENOSPC mid-write leaves a torn line, subsequent appends concatenate onto it, and `read_session_events` hard-fails the *whole* session on the first bad line — no skip/salvage/truncate path. One disk-full or `kill -9` mid-write makes the entire history unloadable.
- **H20. TUI `/new` and `/clear` create orphaned, unresumable sessions** — `norn-tui/src/app/slash.rs:203`. Rotates to `{new_id}.jsonl` without `create_session`/index entry. Every conversation after the first `/new` is invisible to `session list` and unresumable even by full ID — practical data loss. (Print mode does this correctly; drift between duplicated session-opening paths.)
- **H21. TUI never reconciles the session index after turns** — `norn-cli/src/tui/driver.rs:187`. `event_count` stays 0, `updated_at` stays at creation. "Resume latest" (`max_by_key(updated_at)`) then prefers a stale CLI session over the TUI session just used.

---

## Test failures (5) — root-caused

All five `diagnostics_check` failures predate the extraction (introduced by yggdrasil commit `6b0c44b8`, "drop legacy convention groups"; files are byte-identical to pre-extraction). No environment involvement.

1. **`lsp_path_used_outcome_skips_server_and_inline_paths` — genuine logic bug.** The refactor deleted the D4 cascade gating: `check_convention_file` now runs `run_rule_activations` *before* `try_lsp_diagnostics_for_rules` and discards the `LspDiagnosticsOutcome` — directly contradicting the documented contract on `LspDiagnosticsOutcome::Used` (`lsp_diagnostics.rs:60-65`). **Fix the code:** capture the LSP outcome first and skip `ToolRef::Diagnostic` dispatch when `Used`.
2. **The four `server_path_*` tests — test rot.** Server fallback itself works (timing assertions pass). The tests' empty `AdapterRegistry` now produces a *deliberate* Block finding ("cannot run activated diagnostic tool") from the missing-adapter hardening (yggdrasil `5bb2140a`), where the legacy path silently skipped. **Fix the tests:** register a stub adapter (the existing `EchoDiagnosticAdapter` bound to `true`) so the inline fallback runs cleanly; keep the hardened behavior.

---

## Architecture assessment

(Authored by the architecture reviewer; claims grounded in code read during review.)

### Genuinely well-designed
- **Provider trait at the right altitude** — `provider/traits.rs:23-34`: two methods, object-safe (asserted in-tree), no request-building or auth leakage; proven by two real implementations plus a mock.
- **The loop is testable without a live provider, and actually tested that way** — scripted `MockProvider` with request capture (`provider/mock.rs`), a 1,611-line integration suite driving `run_agent_step` end-to-end, ~2,650 lines of in-file runner tests.
- **The five-phase tool lifecycle is the strongest abstraction in the codebase** — `tool/traits.rs:70-186`: pre-validate / execute / post-validate / on-success / register-follow-ups with compile-time and runtime halves; the effect system (`effect_for_args` with a documented never-narrower contract) feeds `SchedulingPlan` batching of ReadOnly/Network calls — a principled answer to parallel tool execution most harnesses fudge.
- **Event-sourced sessions with tree structure** — UUIDv7 time-sortable event IDs, parent links (fork-friendly), self-contained append-only JSONL.
- **SharedWorkingDir threading** — one `Arc<Mutex<PathBuf>>` deliberately shared across ToolContext/LoopContext/VariableStore/RuleEngine so bash `cd` propagates atomically; a subtlety most harnesses get wrong, solved once.
- **Capability negotiation exists and is used** — `ConversationRequestState` switches replay vs provider-side threading off `ProviderCapabilities`.

### Structural risks
- **`run_agent_step_inner` is one ~680-line async function** (runner.rs:191–~870) with ~10 mutable locals threaded through a mega-loop; helpers are flat free functions taking 5–8 params, not a structure. Every feature lands as a diff inside this function.
- **Runtime assembly is written three times, with confirmed behavioral drift** — library (`runtime_init.rs`), CLI (`norn-cli/src/runtime/{builder,wiring}.rs`), TUI (hand-inlined skill-path and usage-format copies). The CLI's `assemble_hook_registry` warns on the `Arc::try_unwrap` failure path; the library version silently does the thing the CLI comment forbids (this is the mechanism behind H13).
- **Two parallel agent-assembly entry points** — the CLI never migrated onto `AgentBuilder`/`runtime_init` ("the pieces that must be identical whether Norn is launched by norn-cli or embedded" — written, then not adopted).
- **`LoopContext` is a god object** — ~23 public fields, mutated field-by-field at each assembly site, which is precisely why assembly can't converge.
- **Service-locator tool dependencies fail at call time** — `ctx.get_extension::<T>()` with no completeness check at build time (the H14 bug class).
- Minor: a stray tracked file literally named `crates/norn-tui/src/error.rs:7:1` (pasted compiler path) should be deleted; `tools/patch_output.rs` is dead, never-compiled code diverging from the real implementation.

### Highest-leverage moves (in order)
1. **R1 — Collapse the three assembly paths into one library-owned assembler.** Finish what `runtime_init.rs` started; delete the CLI/TUI copies (~600–800 lines of deletion). Makes "embedded norn behaves like CLI norn" structural; directly eliminates the H13/H14/H20 drift class.
2. **R2 — Restructure `run_agent_step_inner` as an iteration state machine.** A `StepState` struct + named phase methods; the phase banners are already in the comments — the seams are drawn, not cut. Highest-churn file, highest regression payoff.
3. **R3 — Make `ProviderRequest` provider-neutral; force it with a native Anthropic provider.** Today `previous_response_id`/`store`/`cache_key` are first-class OpenAI concepts; extract a `ConversationThreading` enum gated by capabilities. The second backend (Claude-CLI subprocess) dodges the question; a native Messages provider is the only honest test of the abstraction.
4. **R4 — Version the session JSONL and make the reader tolerant.** No schema version anywhere; reader hard-fails the whole session on one bad line (pairs with H19). Version header + writer version in the index + skip-with-warning on unknown variants. Cheap now, forced migration later.
5. **R5 — Declared tool dependencies.** `required_extensions()` on `Tool`, verified at build: converts the documented call-time failure class into build-time errors and gives R1 a checklist.

### Strategic gaps
- **Provider breadth:** OAuth-OpenAI + Claude-CLI subprocess only; no native Anthropic, no OpenAI-compatible/local path despite `base_url` plumbing existing.
- **Testing:** strong overall; missing SSE-level conformance/replay tests against recorded wire fixtures (sse.rs is 1,286 lines of hand-rolled parsing — the highest-risk fixture-less surface) and a cross-provider contract test.
- **Observability:** ~149 `tracing` events, **zero spans**. Span-per-iteration and span-per-tool-call with token/cost fields is the single cheapest diagnostic win; all the raw materials (Usage accumulation, LlmCallSummary hooks, AgentEvent channel) already exist.
- **Tool extensibility:** first-party tools are excellent; the dynamic MCP-tool→`Tool` bridge — the most important extensibility path for a harness — is the least finished.

**Bottom line:** the core abstractions (tool lifecycle, provider seam, event-sourced sessions, effect scheduling) are above-average for the genre and well-tested. The debt is concentrated and tractable: one oversized loop function and one runtime-assembly story written three times. R1 + R2 are deletion-heavy and low-risk; R3 and R4 are the strategic bets.

---

## Medium-severity findings (reviewer claims; only those marked ✓ were independently verified)

**Loop**
- Auto-compaction never shrinks the *in-flight* step's prompt; takes effect next step (`loop/runner.rs:414`)
- Auto-compaction replaces content with a metadata stub, not an actual summary (`session/context_edit.rs:193`)
- Provider/loop retry replays already-broadcast stream events — duplicate deltas for observers (`loop/classify.rs:113-115`)
- `RepeatedFailure` iteration monitor can never fire: `latest_errors` hardcoded `None` (`loop/runner.rs:566`)
- MaxTokens/ContentFilter-truncated responses returned as successful `Completed` (`loop/classify.rs:46`)

**Provider/auth**
- Successful refresh discarded when persisting auth.json fails (`openai_oauth/manager.rs:87`) ✓
- auth.json written non-atomically while shared with the Codex CLI (`openai_oauth/storage.rs:44`) ✓
- Encrypted reasoning requested via `include` but dropped and never echoed on replay (`openai/request.rs:121`)

**Tools**
- No workspace confinement or symlink checks in any file tool; `apply_patch` accepts model-supplied `working_dir` (`tool/context.rs:217`)
- Block splitter misparses diffs removing `-- `-prefixed lines (SQL/Lua/Haskell) (`tools/patch_parse.rs:54`)
- `web_fetch` has no SSRF protection (localhost/metadata endpoints reachable, redirects followed) (`tools/web/fetch.rs:254`)
- Bash risk classification inspects only the first whitespace token — trivially evaded (`tool/risk.rs:27`)
- Search tool runs synchronous recursive walks on the async executor (`tools/search/mod.rs:269`)
- `DiskTaskStore::update/complete` bypass the claim lock (`tools/task/disk.rs:340`); stale `.lock` files block claims forever (`disk.rs:241`, low)
- Spawned/forked child tasks detached (not aborted) on parent teardown (`tools/agent/handle.rs:74`)
- Extension stdio transport: no timeout, EOF-as-success, cancellation-unsafe write-then-read under shared lock (`integration/extensions.rs:313,336`)

**Agent management**
- `.agent_registry()` advertises fork/spawn it can't deliver — missing AgentHandles/ChildResultSender wiring (`agent/builder.rs:840`)
- Caller-supplied `DiagnosticCollector` silently replaced (`agent/builder.rs:591`) ✓ (corrected to medium)
- Registry never removes terminal entries — paths permanently unusable, unbounded growth (`agent/registry.rs:202`)
- Monitor watch-channel race can lose terminal `is_complete` (`agent/monitor.rs:137`)
- Fork/spawn report MaxIterations/SchemaUnreachable/TimedOut/Cancelled children as `Completed` successes (`tools/agent/fork_pipeline.rs:353`)
- `{{working_dir}}` captures process CWD, not agent working dir (`integration/variables.rs:128`)
- Shell hooks fail open on every failure path, defeating Block veto (`integration/hooks/shell.rs:192`)
- Claude runner adapter synthesizes successful `Done{EndTurn}` on EOF without checking exit status (`integration/claude/adapter.rs:162`)

**Session**
- ActionLog/MutationLedger never rebuilt on resume, contradicting the "session-lifetime record" contract (`agent/builder.rs:621`) ✓
- Mutation ledger hashes model-supplied relative paths against process CWD (`session/action_log_mutations.rs:40`)
- Sink-open failure silently degrades a persisted session to memory-only (`session/persistence/ops.rs:113`)
- `EventStore::append` does synchronous disk I/O under a parking_lot write lock on executor threads (`session/store.rs:152`)
- `compact_line` panics on multi-byte UTF-8 at the 40-byte truncation boundary (`session/action_log.rs:84`) ✓ (corrected to low — bounded blast radius)
- JsonlSink never fsyncs; durability differs between live and fork/copy write paths (`session/store.rs:82`) ✓ (corrected to low — doc promises only process-crash durability)

**CLI/TUI**
- History-file write failure fatally aborts the interactive TUI session on Enter (`norn-tui/src/input/editor.rs:205`)
- Session/runtime assembly triplicated and already drifted (`norn-cli/src/print/session.rs:64`) — see R1
- Session JSONL files written world-readable (no 0o600, unlike auth.json) (`session/persistence/io.rs:77`, low)

---

## Verification notes

- 29 critical/high claims were adversarially verified (verifier re-reads code independently, instructed to refute). All 29 confirmed; corrections were limited to severity calibration (2 downgraded to low, 2 to medium, 1 upgraded to critical).
- The remaining ~30 medium and ~30 low reviewer claims were not individually verified — treat each as a strong lead, not a confirmed bug.
- Reviewers were instructed to ignore `crates/norn/reference/` and `.norn/`.
