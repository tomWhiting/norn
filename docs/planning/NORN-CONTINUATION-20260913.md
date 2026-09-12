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

## Installed checkpoint — 13 September 2026, 03:27 Melbourne

Preview.12 installed from `0d87f8a4b99af4982dc09868bccb16b8d4680847`; SHA-256 `ded53f31ea3829cfe530d8a70d4efb6cd19b85ae48e62a6cb5dad3cea20fbc0d`. `var/releases/preview.12-continuity/installation.json` records the atomic replacement and preview.11 rollback. Release build and actual-executable Kitty keyboard/restoration probe passed after the 4,709 core / 939 TUI tests, strict release workspace/all-target Clippy and formatting checks. Mac announcement completed. No live-provider or physical Herdr pass is claimed.

D01.1 and D02.1 are installed local slices; the overall D01 and D02 deliveries remain in progress. Next: compaction failure policy and semantic projection/diagnostic handling, with a separate file wall; S01 storage compatibility/publication audit continues. Tool lifecycle styling, channel distinction and file-pane highlighting/version browsing remain on D04–D06/D17, not included here.

Disk checkpoint: 25 GiB available; one 3.5 GiB repository-local build cache and 235 MiB release receipts/rollbacks. Removed 191,361,440 bytes of identified obsolete test executables during this goal. Current artifacts and all rollback binaries remain.

The installed Aion CLI confirmed an active `repo_battery_205` route (`56cd8d0a1d3ba94811c605432b0e3730a4f87d1ab9cfe732c62cda46f0f5b0e9`). This was a read-only availability check; no battery has been dispatched and main has not advanced. Inspect its current input and venue resource contract before dispatching.

## D01.2 — semantic summary failure preserves context

Tom's authorized continuity repair now replaces the existing automatic mechanical fallback policy: a provider failure or unusable summary stops the step with a typed error, leaves all context marks and the compaction trigger unchanged, and does not send the oversized ordinary request. Cancellation remains cancellation. Explicit manual mechanical compaction is unchanged. The error retains the provider cause or stop reason/text length and the known summary usage; preflight records a versioned `loop.compaction_failed` audit with that usage before returning the error. No failure is relabelled as successful compaction. Retry classification follows the underlying provider cause; unusable output is terminal.

Wall: `crates/norn/src/error.rs`, `error/{subsystems.rs,compaction.rs}`, `loop/{compaction.rs,compaction_failure.rs,inflight_compaction.rs,mod.rs}`, `loop/runner/prompt.rs`, `loop/runner/tests/local_compaction.rs`, `loop/compaction_failure_tests.rs`, Cargo.toml, Cargo.lock, README.md, release notes and this queue. Provenance: NEXT-WORK D01's explicit failure-policy recommendation, no-silent-fallback house rule, and existing cancellation/usage contracts. A later slice handles general stderr tracing ownership and semantic rendering of opaque response items.

Acceptance: permanent provider failure, empty/truncated output and cancellation commit no compaction or hidden-event marks; trigger remains re-usable; actual runner sends no normal model request after failure; original accepted input survives; known rejected-summary usage and typed error survive in the returned error and durable audit; a later successful retry can compact. Run core/runner and TUI regressions, strict Clippy, formatting and changed-file AST scans. Historical fallback records remain readable. No live session rewrite or inferred recovery of facts lost by earlier semantic fallbacks is claimed.

D01.2 also permits source-documentation correction in `crates/norn/src/loop/summarization.rs`; the summary renderer is unchanged by this slice.

## D01.2 local verification checkpoint

Source fix `6407b1765ab846d6a71c9c4273e9a037fc1a9e6c`: all 4,711 core and 939 TUI library tests passed, zero failed or ignored. Strict release workspace/all-target Clippy with live-api-smoke enabled passed (compiled/linted only, no live provider request). Format and whitespace checks passed. New compaction modules have no unwrap/expect or bypass/discard matches; scans of all 11 changed Rust files record existing counts separately with no increase. Proof logs are under `var/verification/continuation-20260913/d01-failure-*`. An initial test fixture lacked completed assistant turns and was corrected; the final suite includes the real runner error-path regression.

The next slice is D01.3's summary-only projection of opaque encrypted provider items, then terminal diagnostic ownership and S01's storage/allocation audit. This change preserves failed context; it does not yet prevent every oversized summary request.

Venue input was recovered from the first event of completed workflow `62bd3f87-a13b-4fa7-bce5-6dc07fec1229`, under the still-active package `56cd8d0a1d3ba94811c605432b0e3730a4f87d1ab9cfe732c62cda46f0f5b0e9`: `subject_repo`, exact `subject_ref`, `workspace_root=/home/aion/venue/.battery`, and `waivers_json`. Its activities dispatch to `lane_repo_battery` on `venue205`. No repo_battery_205 run was active at inspection. A new measured receipt is still required; that older run is contract evidence only.

## D01.3 and S01.1 — summary projection and compaction allocation

Wall: `crates/norn/src/loop/{summarization.rs,summary_item.rs,summary_item_tests.rs,mod.rs}`, `crates/norn/src/session/context_edit.rs`, this queue, Cargo.toml/Cargo.lock, README and release notes. Provenance: D01 continuity/summary-input diagnosis; S01's measured raw-store clones; `ResponseItem` declares canonical JSON as lossless replay authority.

D01.3 omits only the typed reasoning/compaction item's nonempty top-level `encrypted_content` from the summary's plain-text transcript. It retains all other canonical fields, readable reasoning, tool arguments/results and unknown item kinds unchanged. An explicit summary-only marker names the omitted byte count; the item identity stays in the rendered JSON. No recursive field-name deletion and no provider replay or persistence change. Acceptance: actual summary request omits a large encrypted payload while original event and ordinary replay retain it exactly; null/absent/empty fields and a similarly named tool argument/unknown-item field remain unchanged.

S01.1 replaces whole-event-store clones in planning and commit validation with bounded borrows through `EventStore::with_events`, releasing the borrow before append. Cuts, IDs, context marks and persisted event shape remain identical. Existing plan mismatch, replay, cancellation and repeated-compaction tests cover semantics; inspect that neither method calls `store.events()`. No unmeasured performance percentage or storage saving is claimed. This reduces allocations, not duplicated on-disk compatibility fields.

D01.3/S01.1 local check: 4,714 core library tests passed, zero failed or ignored, including the actual summary request/replay preservation fixture and existing plan mismatch, tool-boundary, repeated-summary and cancellation tests. Strict release workspace/all-target Clippy, format and changed-file AST checks passed. New summary modules are scan-clean; old files' existing test-policy debt remains recorded. TUI source is unchanged; its preview.13 939-test result is retained as historical evidence, not claimed rerun. Logs: `var/verification/continuation-20260913/d01-projection-*`. Release build, actual-binary probe and installation remain next. Independent final-source review and exact-commit venue evidence are still required for landing.
