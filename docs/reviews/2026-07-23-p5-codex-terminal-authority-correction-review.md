# P5 Codex terminal-authority correction: Gate D review

**Date:** 2026-07-23

**Reviewer:** Sable Nightwick (external Gate D coordinator)

**Handoff:** [`2026-07-22-p5-codex-terminal-authority-correction-handoff.md`](2026-07-22-p5-codex-terminal-authority-correction-handoff.md)

**Reviewed boundary:** base `6d168830ee3c4edad5893d39a0e1e67950da98ad`, product
correction `d86a4ed16335abd99fda80185a28bf74492b42ef` (tree
`1d7a97078aa750d2cc8cfd2ff49e05b236c8e8da`), branch head `b7b212e`. The two
commits after `d86a4ed` are documentation only; `d86a4ed..HEAD` contains zero
Rust. `main` untouched by this review.

**Panel:** one Opus 4.8 fallback/authority seat, one cross-model adversarial
seat (norn GPT-5.6 Sol, review/safety, xhigh; session `claude-review.4QLTP2`),
plus my own battery, boundary reproduction, and a three-guard mutation pass.

## Verdict

**READY as a narrow implementation candidate.** Both independent seats returned
READY with no blocker, major, or minor finding; my mutation pass confirms the
three load-bearing guards are precisely covered. One HARDENING recommendation
(a missing regression, below) does not gate the candidate. This verdict is
explicitly **not** P4 re-acceptance, D8 acceptance, whole-P5 acceptance, or the
still-open authenticated D7/P9 live-wire gate.

## Selection chain — proven closed (the decisive seam)

The Codex terminal-output fallback can be reached only from the compiled,
trusted Codex-subscription backend. Every hop verified by source read and by
Sol independently:

- `OpenAiBackend::resolve` (`backend.rs:35-49`) is the sole producer of backend
  identity. `CodexSubscription` is reachable **only** via `AuthSource::OAuth`
  whose `base_url` override must be the canonical
  `https://chatgpt.com:443/backend-api/codex` (scheme/host/port/path/userinfo/
  query/fragment all pinned, percent-escapes and dot-segments rejected). API-key
  auth always yields `ResponsesApi` and additionally rejects any chatgpt.com
  destination. A custom or attacker endpoint therefore gets `StrictPublic`, and
  the attacker-server threat model stays entirely on the strict path.
- `catalog_backend()` is a `const fn` over the enum returning compiled
  constants; no config, env var, provider option, prompt, event, or response
  body is consulted. `for_catalog_backend` does exact-string equality against
  the compiled constant (unknown → `Public`) → `terminal_output_policy()`.
- `ResponseReconciler::with_terminal_output_policy` has a single production call
  site (`execute.rs:272`); `ResponseReconciler::new()` defaults to
  `StrictPublic`; every other construction across the workspace is `#[cfg(test)]`.

## Fallback validation parity — enumerated, fallback is strictly stricter

The fallback authority set is `self.completed`, whose only insertion point is
reached after the full done-frame gauntlet (schema, `authoritative_items_failure`,
id-presence, identity binding, added-family, call-identity, completed-channels,
item-channel authority, duplicate/conflict). `finish` then re-runs every strict
validation on the fallback items and **adds** `validate_fallback_announcements`
(announcement coverage), which the strict public path does not enforce — so the
fallback is a superset of the strict checks, never a subset. Preview text can
never become canonical: `reconcile_authoritative_deltas` is reachable only in
the `else` branch (item not in `completed`), and every fallback item comes *from*
`completed` with identity equality by `output_index`, so that branch is never
taken on the fallback path. A protocol error cannot become `ProviderEvent::Done`:
the mapper self-poisons on any ingest error and `decode_terminal` runs only on a
`Terminal` update for `response.completed|incomplete`.

Ordering/identity: `Ord` on `ResponseItemIdentity` is **hand-implemented on
`output_index` alone** (not derived — a derived `Ord` would have sorted by
`item_id` first and broken the ordering claim), so `completed.keys()` iterates
in strict output-index order; the zero-based contiguity guard rejects gaps and
non-zero starts. Failed responses keep provider-failure authority; incomplete
responses accept complete done-authority and retain `MaxTokens`; unfinished
state fails.

## My mutation pass (each guard reverted, focused suite run, restored byte-clean)

1. **Policy dispatch** — flipping `ResponsesDialect::Codex → StrictPublic`
   failed **8** of the 11 codex_terminal tests (missing/empty fallback, tool
   calls, incomplete, reference ordering, all three fallback-rejection cases).
2. **Contiguity guard** — neutering the `output_index() != expected_index`
   check failed exactly
   `codex_fallback_requires_contiguous_zero_based_output_indices` (10/11 pass),
   a precise single-guard kill.
3. **Public-strict empty gate** — removing the `policy == StrictPublic ||` arm
   guard failed `public_responses_still_requires_terminal_output_authority`
   plus three `terminal_boundaries` cases
   (`failed_response_remains_authoritative_over_orphan_preview`,
   `bare_preview_absent_from_terminal_fails_before_done`, exact-retransmit).

All three guards are load-bearing and precisely covered. Files restored
byte-clean; worktree clean at `b7b212e`.

## HARDENING-1 (nonblocking) — bind the zero-item empty-success shape

A Codex stream of `response.completed` with absent/empty `output` and **no**
completed items yields `Terminal { items: [] }` → `Done { EndTurn }`. This is
consistent by construction (public strict already treats `output: []` with no
completions as an empty success, and the contract defines absent ≡ empty for
Codex), but no test in the 11-case matrix binds it, so the behavior currently
rests on vacuous guard passes rather than a recorded decision. Recommend one
regression asserting the chosen accept-empty semantics so a future guard change
cannot silently flip it. Owner call; does not gate the candidate.

## My battery (repository shared target, at `b7b212e` = frozen Rust)

`cargo fmt --all -- --check` clean; `cargo clippy --locked --workspace
--all-targets --all-features -- -D warnings` exit 0, no suppression; focused
`codex_terminal` **11/11**, `reconciliation_tests` **26/26**,
`response_reconciler` **117/117** (matching the handoff); doctests **8/8**;
`git diff --check` clean. Added-line token scan: zero `unwrap`/`expect`/`panic!`/
`allow`/`todo`/`unsafe` in the range. Sizes reproduce: coordinator 421,
`terminal_authority.rs` 160, new test module 457; the module split is a real
`mod` boundary, not an `include!`/`#[path]` bypass.

**Full-workspace honesty:** my first `--workspace --all-targets --all-features`
sweep observed **5,694 passed / 1 failed**; an immediate identical re-run
observed **0 failures**. The single failure did not reproduce and lies outside
the in-range reconciler files — a sandbox/contention flake of the same class the
device-auth handoff documents (mcp-stdio / process-watch / bash-redirect /
descriptor-retention). I record it as observed-once-non-reproducing, not as a
clean pass and not as a product defect.

## Boundaries

- READY is a source-correction implementation-candidate verdict only. The
  mandatory D7/P9 authenticated real-wire gate remains open: the retained smoke
  kept no raw trace, so the actual absent-versus-empty terminal-output wire
  shape is still unbound. That gate needs explicit credential/spending/redaction
  approval and its full matrix.
- WebSocket transport, retry ownership, cancellation, usage accounting,
  response-item persistence, and D8 prompt authority are unchanged by this range.
- The inherited canonical-item-before-terminal-metadata ordering predates this
  range and is shared by both paths; it produces no new reachable defect here
  (items then a typed error, never `Done`).
