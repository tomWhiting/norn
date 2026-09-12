# Norn continuation — 13 September 2026, Melbourne

Tom authorized completing the remaining list, with regular disk checks, build cleanup, AST-grep and strict Clippy. The JSON companion records the queue; September 8 planning and all 54 original programme rows are preserved beside it. Historical statuses are not fresh verification. This product queue links to the estate index rather than replacing it.

Current branch: `codex/norn-integration-candidate`, starting `cc3fd60`; installed preview.11. Existing voice/driven work must be audited before classifying it as absent. Local installation is distinct from source-bound venue verification and main landing.

## Order

- **D01** Compaction failure diagnosis, continuity policy, and terminal-safe diagnostics. in_progress.
- **D02** Resumed conversation history and earlier-page loading. queued_for_live_audit.
- **D03** Correct resume selection, action-log times, and child-history access. queued_for_live_audit.
- **D04** Tool lifecycle presentation and readable input/output. queued_for_live_audit.
- **D05** Typed channel history and faint inbound-content styling. queued_for_live_audit.
- **D06** Diff highlighting, follow-latest, and remaining reading/composer polish. queued_for_live_audit.
- **D07** In-session action-log search and compaction retrieval links. queued_for_live_audit.
- **D08** First external voice adapter: read-aloud. queued_for_live_audit.
- **D09** Dictation into the Iridium draft. queued_for_live_audit.
- **D10** Seat identity and lifetime-memory design decisions. queued_for_live_audit.
- **D11** Derived lifetime timeline, temporal queries, and retention protections. queued_for_live_audit.
- **D12** Authored memories and addenda anchored to events and time. queued_for_live_audit.
- **D13** Agent directory, selected conversations, direct @ addressing and CC. queued_for_live_audit.
- **D14** Visible queued messages. queued_for_live_audit.
- **D15** Independent live session host and attach/detach. queued_for_live_audit.
- **D16** Linked goals/plans/tasks and additive feature activations. queued_for_live_audit.
- **D17** Editable workspace, session controls, request-time output contracts. queued_for_live_audit.
- **D18** Cross-harness conversation import and continuation. queued_for_live_audit.
- **D19** Liminal extension interfaces and Manifold reconciliation. queued_for_live_audit.
- **D20** Release, installer, remaining review findings and cleanup. queued_for_live_audit.

S01 (session-storage duplication) starts with D01 diagnosis. No live session is edited, deleted or migrated during investigation. U01 (per-file version browsing) extends D06/D17. Later rows with unresolved design decisions must name those decisions while independent work continues; nothing is silently dropped.

## First executable slice: D01.1 — repeated compaction input

Problem: `loop/compaction.rs` passes the entire raw event prefix to summarization. `session/conversion.rs` explicitly requires an already-filtered prompt view. Previous summaries and their superseded originals are therefore both sent again. In the reported session, 30 persisted compaction audits contain two semantic summaries, seven context-window failures and 21 HTTP-400 failures (response bodies were omitted, so their precise causes remain unknown). The file is 57,367,754 bytes at inspection; its latest compaction row is 906,327 bytes. These are snapshot measurements, not a frozen-store claim. No transcript content was exported.

Wall: `crates/norn/src/loop/compaction.rs`, `crates/norn/src/loop/compaction_prompt.rs`, `crates/norn/src/loop/compaction_prompt_tests.rs`, `crates/norn/src/loop/mod.rs`, this JSON/Markdown queue, September 8 planning copies, and release notes. If repair requires changing persistence schemas, summary failure policy, provider contracts or diagnostic routing, create the next separately scoped slice first.

Design provenance: NEXT-WORK-20260908 D01; `loop/context.rs` authoritative prompt visibility and local-tool atomicity; `session/conversion.rs` prompt-only projection contract; NUI-005 continuity follow-ups. Retain current summary, visible older messages and their tool pairs; exclude suppressed/superseded originals and retained recent turns. Preserve cancellation, durable event identities and current fallback policy in this slice. This fixes duplicated summary input, not all potential HTTP-400 causes or stored duplication.

Acceptance: a regression must fail on the original raw-prefix implementation, then pass for repeated compaction and persisted-mark restoration; verify prior summary exactly once, old originals absent, retained turns absent, and original store untouched by selection. Include tool-pair projection and provider-request assertions. Run existing compaction/summarization/replay tests, targeted AST scans and strict Clippy. Greps: `rg 'compaction_prompt|summary_prompt_events' crates/norn/src/loop`; `rg 'repeated_compaction|restored_marks' crates/norn/src/loop/compaction_prompt_tests.rs`.

Resource baseline: 28 GiB available, 2.8 GiB current target, 176 MiB release receipts/rollbacks. Target stays `var/build-preview10` under the repository; its name identifies the existing cache, not the next version.

## D02.1 — load earlier history at the scroll boundary

Wall: `crates/norn-tui/src/app/render/navigation.rs`, `crates/norn-tui/src/app/render/navigation_tests.rs`, this queue and release notes. Provenance: NEXT-WORK D02 and NUI-005 retained history; existing `Transcript::load_older` owns asynchronous bounded page reads. A backward scroll that exhausts loaded rows while `has_older` is true requests one earlier page through the existing loader. No full-history startup load, no I/O inside paint, no competing history owner. Forward scrolling and the true beginning of history request nothing. Accepted pages retain the existing anchor and draft. The remainder of a large gesture is not replayed automatically in this slice; another scroll moves into the newly loaded page. Smooth residual motion and boundary status remain follow-ups.

Acceptance: real bound EventStore tail/older pages, actual navigation and load_visible path, coalesced in-flight history requests, no duplicate records, pinned viewport and draft preserved; forward/beginning no-load regressions. Run retained navigation/history/TUI tests, AST scans and strict Clippy.

Installation wall extension for D01.1/D02.1: Cargo.toml, Cargo.lock and README.md may record the next local preview version once these fixes pass. Installation receipts and rollback stay under repository var/releases; this does not extend either code wall.

## Verification checkpoint

D01.1: all three new provider-request regressions failed with the original raw-prefix selection, then passed after selection uses the existing prompt visibility and atomic tool projection. The full core library suite passed: 4,709 tests, zero failed/ignored. Logs: `var/verification/continuation-20260913/d01-raw-prefix-regression.log` and `d01-core-tests.log`. No live-provider request was made by these regression fixtures.

AST scans on the new modules and changed navigation files found no unwrap/expect calls, lint bypass attributes or discarded-result bindings. `loop/compaction.rs` retains two pre-existing test allowance attributes; these are recorded policy debt, not claimed clean. The installed version remains preview.11 until the preview.12 binary and TUI verification complete.

S01 observation: one live session snapshot had 11,455,311 serialized bytes in canonical assistant response items, plus 7,335,349 bytes in populated compatibility reasoning fields and 2,739,352 bytes in compatibility tool-call fields. Field totals alone do not establish which bytes can safely be removed. Older readers, replay projections and publication commitments must be checked before changing serialization. Compaction planning and commit validation also currently clone the raw event store; that allocation audit is separate from D01.1's reduced summary input.

D02.1 verification: all 939 TUI library tests passed with the installed CLI's libyggd-ast feature set, including the actual navigation-to-history-loader fixture. Strict release-profile workspace/all-target Clippy with live-api-smoke enabled passed, as did format and diff checks. Live provider smoke was compiled/linted only. Release build/install remain pending at this checkpoint. Obsolete preview.11 core/TUI test executables were removed only after checks finished; current executables, dependency cache, logs and rollback binaries remain.
