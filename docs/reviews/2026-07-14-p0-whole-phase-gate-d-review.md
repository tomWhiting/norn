# P0 whole-phase Gate D review — NOT READY

**Reviewer:** Claude (Fable), external Gate D seat — acting as coordinator/integrator.
**Subject:** `2026-07-14-p0-whole-phase-gate-d-handoff.md` — phase base `41ea210`, final tested candidate `13d661c`, documentation-packaging commit `f4cb7db` (recorded per handoff item 1).
**Method:** the handoff requires the verdict to rest on fresh adversarial reproduction, not on promoting the scoped-round acceptances this seat previously issued. Accordingly: five independent Fable reviewers with no prior P0 context each took one required-review item group (D1E permit lifecycles + Chiron; D1D startup; live MCP; secrets/policy/writer inventory; whole-diff + credential destinations vs pinned Codex), and this seat independently reran the complete evidence battery (gate → distributions → attester) from a clean detached worktree at `13d661c`. The verdict below is synthesized from their findings and the battery, none of which depend on the prior round reviews.

## Verdict: **NOT READY** — with the core promise proven

The credential-security core that P0 exists for **held under every attack the panel constructed**: SEC-01's config-driven-exfiltration class is structurally dead, provider redirects are refused typed, approval fingerprinting has no ride-across, generation/runtime pairing admits no split, permits pair with descriptors on every lifecycle edge but one, all 97 writer candidates are accounted for, and the fixtures/evidence contain no secret material.

What forces NOT READY is a bounded set: one ruling-conformance break, one admission undercount, a cluster of failure-path defects in the MCP slices that violate the house no-silent-failures law, and a policy-gate blind spot. All are enumerated below with exact locations; none requires design change.

## 1. Independent evidence reproduction

Clean detached worktree at `13d661c`, pinned toolchain `rustc 1.94.0`, exit codes captured to files.

| Leg | Result |
|---|---|
| Retained evidence chain (6 artifacts) | All SHA-256 hashes match the handoff table byte-for-byte |
| `13d661c` content | Confirmed toolchain pin + README MSRV-claim removal only; no Rust source |
| `DECISIONS-2026-07.md` §11 | All three owner dispositions present, attributed, scoped P0-only |
| Gate (`run_p0_integrated_evidence.py gate`) | **33/35** — `workspace_all_targets` and `norn_tests` each failed with 5 test failures (3,316/5 and 3,311/5) |
| Same two commands rerun on a quiet machine, same head/toolchain/target | **Green**: `-p norn --tests` 3,316/0; `--workspace --all-targets` all suites 0 failed |
| Distributions | **750/750** observations, 0 failures — matches the retained artifact |
| Attester on my bundle | Correctly **failed** (4 errors naming exactly the two red gate cases) — the machinery working as designed |
| Policy (my regeneration) | `policy_contract_passed: true`; 333 changed files, contract numbers match retained artifact |

**Classification of my two red gate cases (HYPOTHESIS, honestly labelled):** the gate ran while five review agents were saturating the same disk and CPU — pressure this seat created, analogous to the load sensitivity that makes the distributions runner serial. The quiet reruns are green, the distributions are green, and the same Rust source produced a fully green 4,624/0 battery in the prior round review. This cannot be upgraded to CONFIRMED because **the evidence schema retains stdout digests but not failing-test identities** (GD-17): neither I nor the implementer (sandbox precursor, same wall) can name the failed tests from the artifact alone.

## 2. Findings

### Blockers

- **GD-1 (D1D — ruling conformance).** A non-checked-in definition can acquire remembered approval. `.norn/settings.local.json` — the documented gitignored personal-overrides layer (`crates/norn/src/config/loader.rs:44-46`) — is overlaid as `McpConfigSource::Project` (`config/mcp.rs:246-250`; `mcp_state_types.rs:38-40`; `integration/mcp_control_actor.rs:461`), so both `norn mcp approve` (`norn-cli/src/commands/mcp_config.rs:316`) and `/mcp approve` persist remembered approval for it (`config/mcp_approval.rs:76`). DECISIONS §10 grants remembered approval to *checked-in* project definitions only. Fails safe in the activation direction (still pending-gated, fingerprint-bound), but approval provenance no longer matches the ruling. **Owner disposition required:** rule the layer in explicitly, or restrict `approve()` to `SharedProject`. Note the codebase performs no VCS check anywhere — "checked-in" is currently unverifiable in principle; the disposition should say what the enforceable predicate is.
- **GD-2 (D1E — admission undercount).** The search tool admits `PRIVATE_FS_OPERATION_PEAK` (5) at `tools/search/tool.rs:204` but every mode runs the recursive `ignore::Walk` (`search/helpers.rs:165`), which the project's own constant says peaks at **11** handles (`descriptor_governor.rs:40-43`). Near capacity an admitted search opens up to 6 ungoverned descriptors and can EMFILE another admitted operation. One-line fix: use `acquire_recursive_walk` (TUI autocomplete already does, `norn-tui/src/input/autocomplete.rs:592`).

### Majors

- **GD-3 (MCP live).** Dead clients are reused forever: transport invalidation (crash, timeout, cancelled in-flight call — which also `start_kill()`s the server, `mcp_stdio.rs:191-202`) never updates `McpRuntimeServerStatus`; `reusable_client` (`mcp_runtime_candidate.rs:154-163`) checks only recorded status + fingerprint, and `Transport` exposes no liveness probe. `/mcp list` shows `connected/active` for a dead server and every reload re-commits the dead Arc.
- **GD-4 (MCP live).** Error causes are systematically destroyed in the control plane: every `map_err` in `mcp_control_actor.rs` discards the source (11+ sites), `McpControlError` is unit-variant, and there is zero `tracing` in `mcp_control_actor.rs`/`mcp_control.rs`/`mcp_state.rs`/`mcp_approval.rs`. A failed `/mcp add` is undiagnosable from any log or surface. Direct violation of "every error handled, logged, or propagated."
- **GD-5 (MCP live).** A transient tools/list refresh failure is permanently marked handled (`mcp_control_watch.rs:104-110`, then `handled = revision` at `:72`) — no retry, no re-arm; the advertised tool surface silently diverges until the server volunteers another notification.
- **GD-6 (D1D).** No inbound frame-size bound on either transport: stdio `read_line` into an unbounded `String`, read pump untied to any timeout (`mcp_stdio.rs:217-225`); HTTP `response.text()` and SSE buffer unbounded (`mcp_http.rs:124-129`, `:238-249`). One hostile connected server can OOM the whole process — worse than the per-server isolation the runtime otherwise guarantees. No oversized-frame fixture exists.
- **GD-7 (D1D).** `MCP_REQUEST_TIMEOUT = 30s` (`mcp_client.rs:35`) is hardcoded, non-configurable, and appears in no DECISIONS entry — an invented number, in tension with the §0.1 no-default-timeout ruling. Compounding: on stdio a legitimately slow (>30s) tool call times out, invalidates the channel, and kills the server for the session (no reconnect in this slice).
- **GD-8 (policy gate blind spot).** Three `mod.rs` files carry production logic — `crates/norn/src/resource/mod.rs` (~85 LOC of permit types and `acquire_*` functions), `session/persistence/mod.rs:57-62`, `tools/diagnostics_check/mod.rs:24-32` — violating the house mod.rs rule, and the gate's `thin_entrypoint_violations: 0` does not check it. Fix the files *and* widen the policy check.

### Minors

- **GD-9 (D1D).** HTTP `mcp-session-id` is remembered before the status check (`mcp_http.rs:110-115`) — an error response can rebind the session.
- **GD-10 (D1D).** Spawned MCP server stderr is discarded (`Stdio::null()`, `mcp_stdio.rs:74`); connect failures surface as generic transport errors with the server's diagnostics unrecoverable.
- **GD-11 (D1D/live seam).** Startup-connected runtimes carry an empty `statuses` map (`mcp_runtime.rs:54-58`): live `/mcp list` reports genuinely connected startup servers as inactive, and the first live mutation finds no reusable client and re-spawns every startup server.
- **GD-12 (MCP live).** `Err(_removed_server) => {}` in the non-strict child rebuild (`tools/agent/live_tools.rs:168`) — a literal swallowed-Err arm; TUI `/tools` renders a startup-frozen list (`norn-tui/src/app/event_loop.rs:256`) that lies after a live mutation; HTTP silently drops mismatched-id responses where stdio treats the same condition as fatal; `/mcp revoke` on a session-disabled project server fails `NotProjectControlled` (re-enable first, or use the CLI); approval publish-failure rollback revokes outright, erasing prior-fingerprint approval; a connecting mutation freezes the whole TUI (actor `channel(1)` awaited inline).
- **GD-13 (D1E).** The admission candidate doc misstates revalidation order (code snapshots *before* reserving — the safe direction; doc says after). Correct the record.
- **GD-14 (D1E).** `claude_runner` is not rev-pinned in `Cargo.toml:18`; `ONE_PIPE_SPAWN_PEAK`'s exactness depends on the locked runner's spawn config — a silent lockfile bump could make it an undercount. Pin it as Chiron is pinned.
- **GD-15 (secrets — disclosure).** Gate evidence discloses local operator context: `removed_ambient_variables` includes env-var *names* (`PERPLEXITY_API_KEY`, `CODEX_THREAD_ID`) and toolchain paths reveal the username. Names only, no values; decide whether name-level disclosure is acceptable in retained artifacts.
- **GD-16 (whole-diff seat).** MCP HTTP and HTTP-extension reqwest clients omit a redirect policy (`mcp_http.rs:46`, `extensions.rs:173`) and follow 30x with operator-configured auth headers. Approval-gated, operator-supplied destinations — defense-in-depth, not SEC-01 class: set `Policy::none()` to match the provider clients.
- **GD-17 (evidence schema).** Runner observations retain stdout/stderr digests and counts but not failing-test identities, so a red case cannot be classified from the artifact (bit both the implementer's sandbox precursor and this review's gate run). Retain failing test names — they are not secret.
- **GD-18 (LOC headroom).** `norn-cli/src/print/orchestrator.rs` is at exactly 499 production LOC; the next edit trips the cap. Split preemptively in the correction round.

### Observations for the record (no action required this round)

D1E: foreground bash parks 5 idle weight per shell (safe direction, false-refusal pressure only); tokio `output()` sites overcount by 1 (safe) but inherit parent stdin (behavioral quirk); pre-handoff cancellation kills the direct child, not the group (process concern, out of D1E scope). Live MCP: `LiveChildTools::latest()` unguarded read-build-write can cache one stale-but-consistent generation (self-heals); embedder dropping all `McpControlHandle`s silently ends live reload. D1D: stdio children inherit the full parent environment — worth an explicit owner ruling given the checked-in-server threat model; two parallel resolution systems (startup vs control-plane) must be kept in lockstep by hand.

### Panel scope gap

The whole-diff seat time-boxed its general cross-round seam sweep (usage constraint) — spot checks (approval gating, model-profile validation, conventions strip) found no invalidated invariant, but the sweep was not exhaustive. Carry into the correction-round review.

## 3. What held under adversarial reproduction

- **SEC-01 class structurally dead** (whole-diff seat): OAuth pinned to compiled endpoints, canonical-or-reject `base_url`, working-dir layers reject all provider-security fields before merge, compiled token/refresh/revoke endpoints, `pub(crate)` settings loaders. Provider clients: `redirect::Policy::none()`, typed refusal, tested over 301/302/303/307/308.
- **Automatic workspace execution gated everywhere checked**: hooks/rules/prompt-commands rejected from working-dir tiers, workspace skills force `disable_shell`, project MCP requires approval before spawn, workspace reads via `openat`+`O_NOFOLLOW`.
- **Permit lifecycles** (D1E seat): 30+ production sites traced; success, spawn-fail, timeout-migrate, cancel-drop, adopt-abort, transport-drop, shutdown all pair permits with descriptors; all 7 `split` sites exact; live revalidation proven safe-direction; Chiron W8→W3 proven on all six lifecycle paths at the pinned rev; observer reserve of 1 exact.
- **D1D core**: five-layer precedence with whole-definition replacement (hostile six-layer fixture), fingerprint over all seven fields defeats every stale-approval attack, zero-activation-while-pending proven by execution-absence fixture, repeated-cursor rejection terminates, stdio cancellation invalidates with a real subprocess, pool/view separation typed.
- **Live MCP core**: no generation/runtime split-pair constructible (single-writer actor, per-request leases, self-contained snapshots); watcher identity guards hold; no credential disclosure on any `/mcp` surface (origin-only URLs, redacted Debug, history bypass); no approval ride-across.
- **Secrets and writers** (dedicated seat): all credential-shaped fixture strings provably dummies; no key formats, recorded bodies, prompts, or cache keys anywhere in evidence or fixtures; **97/97 writer candidates reconciled, none unaccounted**; adversarial re-sweep of write-API call sites found no missed default-permission workspace-visible writer; prohibited-added-line grep independently 0 across 30,740 added lines; 62/62 test-only classifications verified.
- **Codex parity**: no consequential divergence; norn's login flow parity-or-stricter (127.0.0.1 binding, one-time state, persist-then-respond); known omissions are pre-existing P2/P3-owned findings.
- **Owner dispositions** (§11) recorded, attributed, correctly scoped; toolchain pin factual.

## 4. Required correction round

Implementer: GD-2 (one line), GD-3/4/5 (MCP failure-path cluster — liveness on reuse, typed+traced control-plane errors, refresh retry/re-arm), GD-6 (bounds — values need owner ruling or factual derivation, not invention), GD-8 (move logic out of the three mod.rs files; widen the policy check), GD-9/10/11/12/13/14/17/18.
Owner: GD-1 disposition (approval scope predicate), GD-7 (MCP request timeout: rule a value or make it configurable-with-no-default per §0.1), GD-15 (name-level disclosure stance), env-inheritance observation.
Then: fresh gate + distributions + attestation at the corrected head, and a focused re-review of the corrected surfaces (a panel this size is not needed again — the cores are proven; the correction round needs one reviewer over the fixed failure paths plus the deferred seam sweep).

P0 acceptance boxes remain unpopulated per the handoff rule.
