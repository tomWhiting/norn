# P0 whole-phase correction review — Gate D verdict

- **Review date (Australia/Melbourne):** 2026-07-15
- **Reviewer:** external Gate D seat (coordinator) + three independent read-only
  seats and one whole-diff seam seat
- **Campaign base:** `41ea210`
- **Controlling review:** `c6bf1e2` / `2026-07-14-p0-whole-phase-gate-d-review.md`
- **Corrected source head reviewed:** `e1bf7f2`
- **Correction range:** `c6bf1e2..e1bf7f2`
- **Documentation/evidence packaging:** `7648159`, `c029de5`, `1096628`
- **Full P0 range (seam sweep):** `41ea210..e1bf7f2`

> **Post-publication traceability correction:** the original `7ce29d7` review
> text mislabeled the failure/TUI cluster as GD-13 and the `claude_runner` pin
> as GD-16. The corrected labels below are GD-12 and GD-14 respectively; GD-11
> owns startup status hydration, GD-13 owns the admission-record ordering
> correction, and GD-16 owns redirect refusal. This editorial correction does
> not change evidence or verdict. Generic local interpreter paths in this review
> and the two reviewer-battery records were also replaced by path-free toolchain
> descriptions; no username, credential, or result changed. The later
> [`P0 acceptance evidence supplement`](2026-07-15-p0-acceptance-supplement.md)
> makes three exhaustive manual inspections explicit; it does not replace or
> expand this verdict.

## Verdict: READY — P0 accepted

Every GD-1 through GD-18 finding from the controlling review is closed and
independently verified. The one new finding surfaced by the deferred whole-diff
seam sweep (SS-1) received an explicit owner disposition on 2026-07-15: starting
MCP servers an operator placed in the local `.norn/settings.local.json` file is
correct, intended behavior, not a gap. SS-1 is recorded as an observation. No
finding blocks acceptance.

The security core this phase exists to close is proven dead: repository- and
working-directory-controlled configuration cannot redirect Codex OAuth or any
provider destination to exfiltrate credentials (SEC-01 class). That was
re-confirmed here, not promoted from prior scoped rounds.

## Reproduction

Clean detached worktree at `e1bf7f2` under the main repository's ignored
`target/worktrees/`, native toolchain `rustc 1.94.0`, Python
3.14.3 from the explicitly selected native interpreter,
macOS-26.3.1-arm64. Legs run serially, alone on the host (no overlapping Cargo
processes — the controlling round's
load-contention error was not repeated). Exit codes captured to files, never
piped.

| Leg | Result |
|---|---|
| Retained evidence hash chain (6 correction artifacts) | all SHA-256 match the handoff table |
| Policy (`run_p0_policy_evidence.py`, `41ea210..e1bf7f2`) | **byte-identical** to `2026-07-14-p0-correction-policy-e1bf7f2.json` (schema v2, pass) |
| Policy contents | 359 changed Rust files, 65 test-only, 97 writer candidates; 0 bypass matches (unwrap/expect/panic/allow/ignore/todo/marker); 0 files >500 LOC; 0 module-shape / thin-entrypoint violations |
| Gate (`run_p0_integrated_evidence.py gate`) | **38/38 cases pass**; `repository_integrity_passed`, `policy_contract_passed`; head `e1bf7f2` |
| Distributions (`--concurrency-runs 50 --other-runs 20`) | **830/830 observations, 0 failed**, 1,250 Rust executions |
| Attestation (`attest_p0_evidence.py`) | **pass, 0 errors**; binds gate/distributions/policy to head `e1bf7f2`, worktree clean |

Policy determinism (byte-identical reproduction) is confirmed independently of
the heavy legs. The gate/distributions/attestation legs ran serially and alone
on the host and returned green with no flake — unlike the controlling round's
concurrent-load gate reds, which do not recur here. Reproduction record:
[`evidence/2026-07-15-p0-correction-reviewer-battery.json`](evidence/2026-07-15-p0-correction-reviewer-battery.json).

## Correction verification (GD-1 … GD-18)

Three independent read-only seats verified the corrected failure and
cancellation paths, not only success fixtures. All findings PROVEN except the
writer inventory (methodology GAP, not an unreconciled writer — see below).

**MCP transport & live-control (seat A):**

- **GD-3** dead-client liveness/reconnect/quarantine — PROVEN. `McpClientInner`
  `live` atomic + `ClientRequestGuard` marks the client dead on any
  dropped/cancelled in-flight call; `reusable_client` filters on `is_live()` so
  a dead `Arc` cannot be handed out; `reported_server_status` re-checks
  liveness so `/mcp list` shows Failed, not a stale Connected; recovery on the
  next mutation/refresh via a fresh connect.
- **GD-4** control-plane provenance/tracing — PROVEN. `McpControlError` carries
  `kind` + `context` + `SharedError` source with a real `Error::source()`
  chain; the actor loop logs every failed op centrally; no source-discarding
  `map_err`.
- **GD-5** failed-refresh re-arm — PROVEN. `handled`/`applied_tool_revisions`
  advance only on `Applied`; a failed refresh triggers reconnect (retry) rather
  than being permanently marked handled.
- **GD-6** unbounded inbound frames — PROVEN. 10 MiB factual default (sourced
  from the official MCP TypeScript SDK `STDIO_DEFAULT_MAX_BUFFER_SIZE`),
  per-server `max_inbound_message_bytes` override, enforced **pre-buffer** on
  stdio lines, HTTP JSON bodies (incl. a lying `content-length`), and complete
  SSE events one-at-a-time with ignored fields/comments counted toward the
  limit; typed `McpInboundMessageTooLarge`; oversize fails the read pump.
- **GD-7** invented 30s timeout — PROVEN. Constant removed;
  `request_timeout_ms: Option<u64>`, `Some(0)` rejected; absence = no imposed
  timeout; an explicit deadline wraps the whole logical request including body
  parsing and nested SSE server-request handling; both connection controls
  participate independently in the definition fingerprint.
- Follow-up seams (SSE one-at-a-time + ignored-field accounting; HTTP deadline
  over nested protocol handling; hostile `Content-Type` never rendered into a
  failure) — all PROVEN with fixtures.
- Redirect refusal (`redirect::Policy::none()`) on both — and only — the MCP
  and extension reqwest clients — PROVEN.
- Stderr non-disclosure — PROVEN. Drained under descriptor governance; no
  text/bytes/digest retained; failures report only the fixed
  `no content observed` / `one withheld line` / `multiple withheld lines`
  categories plus completion state.
- GD-9 session-id rebind and the GD-12 failure/TUI cluster (`live_tools`
  swallowed arm, HTTP id-mismatch drop, TUI stale `/tools` + freeze) — PROVEN.
  GD-11 owns the separately proven startup-status hydration path below.
- `17a3bb8` remote-error fixture — PROVEN: the `panic!`/`unwrap_err` is genuinely
  removed (replaced with `Err`-returning `let-else`), same assertions retained.

**Structural / runtime (seat B):**

- **GD-1** provenance/approval — PROVEN at the mechanism level (see SS-1 for the
  disposition). `.norn/settings.local.json` maps to `McpConfigSource::Local`;
  remembered approval is gated to `SharedProject`/`Project` only, in both the
  startup and live-control approval systems; a same-name local override never
  rides the project ledger entry; fixture `0e8fb49` proves direct non-gated
  startup.
- **GD-2** search-walk admission — PROVEN. `search/tool.rs` and TUI autocomplete
  both acquire `RECURSIVE_WALK_PEAK` (11); a workspace sweep found no other
  ungoverned recursive walk (all other `read_dir` sites collect per level).
- **GD-8** mod.rs purity — PROVEN. The three offending files are now
  declarations/re-exports only; logic moved to `admission.rs` siblings; full
  sweep clean.
- **GD-18** orchestrator split — PROVEN. `print/orchestrator.rs` 433 production
  LOC; `PrintError` moved verbatim to `print/error.rs` (68 LOC); real boundary.
- **GD-14** `claude_runner` pin — PROVEN. `Cargo.toml` rev
  `643a1166f06a1f42961acf442f654670fbe9da22`; `Cargo.lock` agrees.
- **GD-11** startup status hydration — PROVEN. `with_config_snapshot` hydrates
  statuses + fingerprints at adoption; the first mutation reuses rather than
  respawning; `mcp list` reports real state.
- **GD-12** revoke-blocked-while-disabled and rollback over-revoke — PROVEN.
- **GD-13** descriptor-admission record order — PROVEN. The candidate record
  now states that live limits and the open count are snapshotted before the
  weighted reservation, matching `DescriptorGovernor::try_acquire`.
- **GD-10** stderr drain — PROVEN. `Stdio::piped()` + governed drain; the
  `Stdio::null()` discard is gone.
- **D1E** descriptor-permit lifecycle — PROVEN. Weights exact (TWO_PIPE=7,
  ONE_PIPE=5, THREE_PIPE=8, HTTP=3, PRIVATE_FS=5, RECURSIVE_WALK=11, observer
  reserve=1); stdio THREE_PIPE split correct; HTTP permit spans body/SSE
  completion; no leak or double-release across `?`/early-return/abort paths.
- **Writer inventory (97)** — GAP, not a defect. The curated 97 was not
  byte-reproducible without the policy's exact methodology, but the correction
  range introduces **zero** new production filesystem writers, so the prior
  97/97 reconciliation is undisturbed.

**Evidence machinery & disclosure (seat C):**

- **GD-17** — PROVEN. Schema v3 retains runner identities, failed-test names,
  and counts; the retained failed-first-run artifact at `17a3bb8` carries the
  six failing names for its three red runners, honestly flagged
  `failed_test_identity_complete: false` (its parser predated the Cargo 1.94
  backtick fix landed at `e1bf7f2`).
- **GD-15** — PROVEN. All six retained JSONs are path-free and name-free; env
  handling uses a fixed sanitized allowlist (fail-closed) and rejects any
  injected ambient name; absolute-path content is separately swept.
- Deletion disposition (§12) — PROVEN. The six superseded schema-v2 artifacts
  are absent from HEAD, retrievable from their package commits (`e9b02d0` /
  `564af2d`) with recomputed hashes matching, and referenced by commit/hash not
  dead links.
- 43/43 self-tests — PROVEN honest (live collection = 43; count and module
  inventory both enforced; deleting a test fails the gate).
- Module-reachability / mod-shape checks — PROVEN to enforce the CLAUDE.md
  mod.rs-purity rule that the old thin-entrypoint check missed (GD-8 process
  half), with fixtures covering the body-bearing shapes that previously escaped.
- Attestation binding — PROVEN. Binds gate/distributions/policy to exact head
  `e1bf7f2`, refuses mismatched or dirty heads.
- Gate composition — PROVEN 38 cases (fmt, strict clippy `-D warnings`, check,
  workspace all-targets, norn/norn-cli/norn-tui, doctests, phase-diff,
  self-tests, full-range policy, 25 non-disclosure sentinels + 1 model-facing,
  repository integrity).
- Repository-local path enforcement — PROVEN (no `/tmp`; lanes required;
  bypass raises).

## SS-1 — workspace-local MCP scope (OBSERVATION, owner-disposed)

The deferred whole-diff seam sweep noted that the in-repo
`.norn/settings.local.json` (`WorkspaceLocal` layer) starts MCP stdio servers
without an approval prompt, and a spawned stdio server inherits Norn's full
parent environment (§10-ruled).

**Owner disposition (Tom, 2026-07-15):** this is correct, intended behavior.
`.norn/settings.local.json` is the operator's own local override file; servers
placed there were placed by the operator and should start without a second
prompt, matching the local-scope convention of other coding agents and the §10
direction that MCP is machine-wide operator capability, not an OS security
boundary. Requiring approval for one's own local file is friction, not
security. SS-1 is an OBSERVATION.

**Residual, for the record:** the only case with any teeth is a repository that
ships a *committed* `.norn/settings.local.json` which a user then clones and
opens under Norn. That is already bounded by the general reality that running an
agent on an untrusted repository executes its code (build scripts, tests,
agent-issued commands); the MCP-startup path is not a meaningful escalation over
that baseline. `config/loader.rs:89` loads the file unconditionally (no
git-tracked check; the gitignore convention at `loader.rs:45` is documentary).

**Optional hardening (not required, not gating):** a future change could refuse
a git-*tracked* `.norn/settings.local.json` so only a genuinely untracked
personal override is trusted, aligning the code with the "personal local file"
convention. Left to the owner's discretion; it does not affect P0 acceptance.

## What held (proven core)

Re-confirmed by fresh seats, not promoted: SEC-01 credential-exfiltration class
structurally dead (compiled OAuth destination, canonical-or-reject base_url,
provider-security field rejection pre-merge, `redirect::Policy::none()` on all
provider clients); descriptor-permit lifecycles across all sites and splits;
the D1D fingerprint model (whole-definition, invalidated v1→v2 by the new
connection controls); no split generation/runtime pair constructible; the 97/97
writer reconciliation undisturbed; retained evidence clean of credentials,
paths, and ambient names; Codex credential-relevant parity holds statically
(the pinned Codex source is not vendored, so byte-level parity is a documented
GAP, not a defect).

## Follow-up ledger (none gating)

1. Optional, owner's discretion: reject a git-tracked
   `.norn/settings.local.json` (SS-1) — a convenience/hardening alignment, not a
   P0 requirement.
2. Pre-existing, out of P0 scope: the invented `EXTENSION_TIMEOUT = 30s` in
   `integration/extensions.rs:32` (predates the phase; flag for a later
   configurability pass).
