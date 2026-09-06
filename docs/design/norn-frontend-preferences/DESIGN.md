---
type: design
cluster: norn-frontend-preferences
title: Saved frontend preferences
---

# Saved frontend preferences

## Intention

Remember existing frontend choices with automatic personal saving as an explicit working assumption, temporary run scope and explicit local scope, preserving startup speed and the restored appearance.

## Problem

- ViewConfig/ScreenState/display/input mode currently reset at restart.
- Independent whole-document writers could lose concurrent MCP changes; loaded tui layer provenance is discarded by the existing merge.

## Solution

Extract one locked atomic settings-document boundary, retain narrow raw tui snapshots during the existing load, and consume them through one frontend preferences owner.

### P1: Shared writer

snapshot.patch(owned_keys: &[&str], replacement: &Map<String, Value>) acquires the existing configured resource permit and shared physical document lock, then freshly reads. Compare each owned key value against the original snapshot, preserving missing-versus-present values. A changed owned key is a named typed conflict. Preserve all unowned tui/document keys; remove an owned key absent from replacement.

### P2: Foreground preferences

Ordinary preference edits save automatically to personal user settings, as the recorded root working assumption rather than a user-confirmed quote. Use /view preferences status, /view preferences run, /view preferences user, /view preferences local and /view preferences save. User/local select the current-process write scope and immediately save current values; run makes subsequent edits temporary. Each CLI launch selects User saving even when a higher-priority local tui object supplies the loaded values: the write scope itself is not serialized. Whole-tui-object precedence remains user < shared project < workspace-local. Status names the current target, publication outcome and captured winning layer; a personal save can be shadowed on restart. No implicit shared-project write, provider request, MCP start or reload follows a preference control.

### PROOF: Restart and concurrency

Use fresh actual CLI/TUI processes with isolated NORN_HOME and canonical launch roots: change existing controls, save user, exit, relaunch and verify typed/visible settings; repeat local and a second root. A new AppState in one process is insufficient restart proof. Preserve tiny/wide resize, exact source maps, input draft and restored appearance.

## Decisions

### D1: One shared writer and truthful publication

TuiPreferencesSnapshot::from_layer(scope: User|WorkspaceLocal, validated launch root, original: Option<Value>) captures the actual target path without rereading startup files. Present tui values must be objects. Capture only tui data and path/layer provenance, never whole settings containing credentials. No startup lock, directory creation or file write.

snapshot.patch(owned_keys: &[&str], replacement: &Map<String, Value>) acquires the existing configured resource permit and shared physical document lock, then freshly reads. Compare each owned key value against the original snapshot, preserving missing-versus-present values. A changed owned key is a named typed conflict. Preserve all unowned tui/document keys; remove an owned key absent from replacement.

Return the new snapshot and typed Unchanged, PublishedDurable or PublishedDurabilityUncertain(io error) outcome. Outer Err means no publication. Retain actual publication state, typed cause and path through write/rename/directory-sync and cleanup errors. Never report a post-rename failure as if nothing reached disk.

Extract the private document boundary from mcp_patch.rs into private_settings_document.rs, and replace mcp_workspace_write.rs with workspace_settings_document.rs. settings_document.rs/settings_write.rs hold shared lifecycle/publication types; tui_preferences_types.rs keeps opaque snapshot/scope/outcome shapes below 500 production lines. Preserve private .mcp-settings.lock, workspace directory lock, descriptor/no-follow handling, permissions, temp cleanup and fsync semantics. MCP writers reuse this same boundary, not an independent lock/writer.

Capture original user/project/local tui values and their whole-object winning layer in config/mcp.rs::load_resolved_settings_at_launch_root before merge consumes the loaded layers. R1 exposes that narrow snapshot through existing core ResolvedSettings only. R2 owns the separate runtime/resolve.rs threading into ResolvedInvocation and the TUI consumer; it is not R1 source work or an R1 completion requirement. No additional startup settings read, MCP transport activation, provider call or runtime reload. Core never depends on the TUI crate.

### D2: Explicit scopes and existing precedence

P2 owns only the tui.view, tui.display and tui.input object sections. Preserve composer and every unowned tui sibling unchanged. view fields: changes_open, split {conversation,changes}, upper_pane, expanded_tools, history_events, body_bytes, clipboard. display fields: thinking_visible, secondary_fields_visible. input field: submit_mode (steer|queue). Typed invalid/unknown owned fields fail by dotted path before raw mode/provider/MCP construction; no coercion or fallback. Absent fields use current declared defaults unchanged.

Preserve existing defaults: Changes closed; requested split1:1; upper Conversation; expanded_tools=false; history_events20; body_bytes65536; clipboard unspecified; thinking visible; secondary fields hidden; submit mode steer. Positive demands and positive u16 split weights retain their current validation. Saved split stores requested weights, never clamped geometry. Clipboard saves operator transport intent, never capability or acceptance proof.

Retain existing whole-object tui precedence across user, shared project and workspace-local settings. Higher tui objects shadow lower objects rather than merging individual fields. Show active run value, selected/written scope/path and effective winning layer, including saved-but-shadowed outcomes. Do not add a CLI preference override or new merge rule.

Working assumption chosen by root after the optional question had reasonable time without a reply: ordinary frontend preference changes automatically save to personal user settings by default, consistent with the request that choices be remembered. This is NOT a user-confirmed ruling. Run scope is temporary; workspace-local persistence is explicit opt-in. Preserve an explicit save action and /view preferences status. User means the existing NORN_HOME/user settings path; local means immutable canonical launch-root/.norn/settings.local.json, not the private MCP-only per-project file. Shared-project writes remain outside this checkpoint. Show active scope plus unsaved/pending/saved/failed or shadowed state truthfully. R1 snapshot/patch API and file wall are unchanged.

### D3: Existing appearance and local runtime ownership

Pass typed initial preferences and opaque save authority through TuiInputs and all five current construction sites (CLI driver, model_selection, pty_smoke, support/mcp_channels_tui and support/retained_workspace); the TuiInputs struct definition is not a construction site. One shared frontend_preferences owner applies initial state and supervises at most one in-flight write through existing blocking/event facilities. Ordinary changes while a write is pending still update the usable current view and remain visibly unsaved until their actual value is persisted; never report the in-flight older value as saving newer changes. Reconcile against the actual completion and current eligible scope, then save the latest desired value only through the same existing event owner, without an unbounded queue. Run-only changes must not be picked up by a later completion for persistence. Observe an in-flight write on exit/cancellation. Preserve draft/run values on prepublication failure and retain truthful committed-but-unsynced outcomes. No timer, polling, debounce interval, background watcher, retry loop or new capacity is invented. As R2 settles, record the exact current-state/pending-write/completion transitions, failure-resumption rule and exit policy; do not claim a mechanism implemented before its source and tests exist.

Never persist ItemId, BodyRef, ViewSource, viewport/selection/search state, draft/submission text, pending steers or followups, session data, credentials or terminal capability replies. Preserve the restored blue prompt/plain composer/status rules and all retained source/reading/usage behavior. View preference changes/saves start or restart no MCP server and never become model input.

No Iridium dependency is needed for this checkpoint. Composer/send-key consumption remains NWP-05.3/NWP-05.4 later, using this same writer and preference owner. Do not create parallel composer preference storage or claim send-key support from P2.

### D4: Restart and interprocess proof

Use fresh actual CLI/TUI processes with isolated NORN_HOME and canonical launch roots: change existing controls, save user, exit, relaunch and verify typed/visible settings; repeat local and a second root. A new AppState in one process is insufficient restart proof. Preserve tiny/wide resize, exact source maps, input draft and restored appearance.

Exercise whole-object layer shadowing, defaults, malformed owned values before raw mode, unowned composer/sibling preservation, unsaved run changes and named write failures. Cross-session saved mode applies only to the preference, not pending input.

Use two actual processes with explicit barriers and the same user/workspace-local file to overlap an MCP patch and frontend save in both lock orders. Assert both edits and unrelated keys survive. A stale same-owned-key save refuses by key without overwrite. Preserve MCP existing regression assertions and production lock/path policy. No sleep-based race proof.

Exercise symlink/root replacement, lock/read/write/rename/sync and cancellation phases with typed not-published versus published-unsynced observations; do not duplicate a committed operation after a reporting failure. Shared source tests may use existing in-wall private writer helpers. No arbitrary timeout/default or service is introduced.

Observe actual provider/MCP startup counts in otherwise identical fixture launches, with and without saved preferences: no additional startup activation. During view edits/save/status, additional provider requests/MCP starts or reloads remain zero. Retain ordinary configured MCP startup behavior. Measure startup/read and input/save behavior with reported samples, not an invented timing cap.

Verify the selected automatic-personal-saving behavior using actual fresh CLI/TUI processes: ordinary view changes are restored after restart without an explicit save command; temporary run-scope changes are not persisted; local opt-in affects only that launch root. With a write deliberately held by an explicit test barrier, make further changes and assert at most one in-flight operation, truthful unsaved state, observed completion, and eventual persistence of the latest eligible state without an unbounded queue. Change to run scope while pending and prove the newer temporary value is not accidentally saved. Exercise exit during a pending write, named failure and explicit save/status; no timer-based quiescence proof.

### D5: Stages and release boundary

R1 root source authoring may begin after registration. R2 consumes the frozen common API; R3 final execution requires R1/R2. No parallel writes to common paths/manifests. The composer draft R5 is fulfilled by this shared implementation and is not implemented twice. Existing programme IDs04.2/04.3/04.4/07.5 retain this required checkpoint;05.3/05.4 remain future consumers. No new programme row, Iridium wait or changed appearance. Run meaningful unit/real-process tests, formatting, strict workspace/all-target Clippy and no-bypass/500-line checks on captured source. Checked local previews follow existing version/hash/rollback and say Norn updated procedure. Main landing/public release still require their exact venue/review proof; this brief is not a pass or install claim.

### D6: Observe saved preferences during active provider work

R2 also owns app/turn/run.rs under crates/norn-tui/src solely to observe preference-save completion in the active-turn select while the provider is running. Do not postpone completion/reconciliation until the idle loop, block provider/input event handling, or create a second preference owner. Keep this nearly full file within 500 production lines by extracting orchestration into the already-owned new app/frontend_preferences.rs owner. Preserve the current provider/turn/cancellation/source behavior. Any genuinely needed additional module must be named and registered before editing; this amendment adds no other path.

### D7: Deterministic contention at the real settings lock boundary

After the first R1 configuration suite completed235/0 and root explicitly thawed core source, the four files config/private_settings_document.rs, workspace_settings_document.rs, settings_document.rs and mod.rs transfer temporarily from root/R1 to channels_fixture/R3 for deterministic test instrumentation. The separate new config/settings_write_process_tests.rs holds bounded process tests. No simultaneous writes to these files; root resumes ownership only after an explicit handback.

Use test-only, opt-in thread-local callbacks. A callback immediately before the real lock.lock receives the actual lock descriptor and path. Another test phase holds the first writer after the actual fresh read, and another holds it after publication while its guard remains alive. Non-test builds contain no hook execution, shipping environment reads, added lock costs or new runtime behavior.

Drive typed explicit subprocess barriers: while writer one holds the actual document lock after fresh read, writer two uses try_lock on the exact descriptor supplied at its real acquire boundary and proves contention. Hold writer one after publication with its guard alive and prove the same descriptor remains contended. Release writer one, allow the normal blocking lock acquire by writer two, and verify its subsequent fresh read sees the first committed edit. Unexpected try_lock success and other errors are distinct failures; never substitute a different lock/path.

Repeat actual MCP and frontend writers in both orderings for both personal-user and workspace-local documents. Assert exact edits, unowned values and unrelated JSON survive. No sleeps, negative-duration assertions, guessed scheduling or parent-held unrelated lock stands in for observed internal contention. Existing typed mutation outcomes, no-follow authority and all production behavior remain unchanged.

### D8: Separate restart proof and versioned preview checkpoint

Reuse the existing retained_screen fixture observer read-only rather than weakening geometry assertions. Root alone may register the harness=false frontend_preferences_restart process fixture and portable-pty and vte workspace dev-dependency references plus direct unicode-segmentation="1.13.2" and unicode-width="0.2" dev dependencies matching the existing norn-tui declarations and already-resolved versions in crates/norn-cli/Cargo.toml. Cargo.lock may change this workspace dev-dependency membership and only the four workspace Norn package versions (norn, norn-cli, norn-macros, norn-tui) to0.1.0-preview.4 alongside the root Cargo.toml workspace.package version. Third-party versions, sources and checksums remain frozen; no dependency upgrades.

R3 parallel authoring partition: review_tui exclusively owns the new crates/norn-cli/tests/frontend_preferences_restart.rs and tests/support/frontend_preferences_restart.rs for fresh executable PTY restart, scope/activation and saved-preference proof. channels_fixture retains the core internal-lock partition and the two existing public mutation-helper test files. Reuse the retained_screen observer read-only and preserve its actual geometry/activation evidence; no shared source writers. Final R3 verification/completion still requires R1/R2 integration.

Root exclusively owns the R3 Cargo.toml, crates/norn-cli/Cargo.toml, Cargo.lock and docs/release-notes/UNRELEASED.md checkpoint edits after the usable stage is verified. The next installed candidate uses0.1.0-preview.4 rather than reusingpreview.1. Verify candidate and installed version/hash, preserve a verified rollback copy and receipt, and invoke macOS say with the exact words Norn updated after successful local installation. This registration performs no version bump, install, tag, public release or main landing; existing venue/review gates remain unchanged.

### D9: Integrate the unchanged preference contract over preview.2

Continue the existing NFP-001 contract in /private/tmp/norn-frontend-preferences on codex/norn-frontend-preferences-integrated, based on 718ab1272375a4cc8edf9852018191b24ec6d52d. That baseline includes installed preview.2 code 119fbf0780a5dcc628002d548a28a49b50b96bdb and its documentation checkpoint. Preserve original authoring baseline 039d0371c69e48753e3633e22f160ed4355e8377, ownership and local R1 proofs as historical source-bound records. The original dirty worktree remains untouched; native patch replay succeeded after an initial external visual-diff refusal. The replay itself is not integrated build, test, restart or installation proof. Existing R1/R2/R3 walls and programme IDs are unchanged; preview.4 remains reserved for preferences.

### D10: Current preference commands and process-local save scope

Ordinary preference edits save automatically to personal user settings, as the recorded root working assumption rather than a user-confirmed quote. Use /view preferences status, /view preferences run, /view preferences user, /view preferences local and /view preferences save. User/local select the current-process write scope and immediately save current values; run makes subsequent edits temporary. Each CLI launch selects User saving even when a higher-priority local tui object supplies the loaded values: the write scope itself is not serialized. Whole-tui-object precedence remains user < shared project < workspace-local. Status names the current target, publication outcome and captured winning layer; a personal save can be shadowed on restart. No implicit shared-project write, provider request, MCP start or reload follows a preference control.

review_selection owns only the new docs/TUI-PREFERENCES.md operational guide in R3. Ground its JSON fields, enum values, defaults, exact commands and save outcomes in the current frontend decode/projection and single save-owner implementation. Explain whole-tui-layer precedence; automatic User saving as a root working assumption; run/local scope as process-local rather than persisted policy; named malformed-field/conflict refusal; one pending save and restart recovery; no provider/MCP activation from controls; and preview.3 not yet installed. Preserve all production walls and existing acceptance. Root retains UNRELEASED.md ownership.

### D11: Document the existing shared MCP test fixture

R3 review_tui may modify crates/norn-cli/tests/support/mcp_launch_fixture.rs only to document its existing reusable test-support APIs. The actual strict-Clippy run reported 35 missing-doc findings because the new restart test publicly imports that fixture. Preserve executable code, signatures, visibility, scenario behavior and every assertion; do not duplicate the MCP implementation or introduce a lint bypass. review_tui retains its two restart fixture paths and exclusively owns this documentation-only amendment; no simultaneous writer is allowed.

### D12: Preview.4 follows the urgent tool-description repair

Root intentionally interrupted the first integrated full-workspace test attempt during compilation after 599.881 seconds (SIGINT, exit -2); its source was unchanged and it supplies no test verdict. The priority NUI-002 repair is reserved for preview.3. NFP is now reserved as preview.4 and must inherit that tool-description envelope repair before fresh composed verification, build and installation. Existing production scope and programme IDs are unchanged. Prior scoped Clippy/fmt/AST passes remain historical and do not verify a future composed candidate.

## Goals

- Actual restart persistence for existing preferences
- Preserved unrelated MCP settings and typed publication outcomes
- No extra startup read/service or UI appearance change

## Non-Goals

- Iridium/send-key consumer, live attach, new appearance, shared-project writes, per-key layer merge, installer/background updater or public release

## Structure

- `Cargo.lock`
- `Cargo.toml`
- `crates/norn-cli/Cargo.toml`
- `crates/norn-cli/src/runtime/resolve.rs`
- `crates/norn-cli/src/tui/driver.rs`
- `crates/norn-cli/tests/frontend_preferences.rs`
- `crates/norn-cli/tests/frontend_preferences_restart.rs`
- `crates/norn-cli/tests/support/frontend_preferences.rs`
- `crates/norn-cli/tests/support/frontend_preferences_restart.rs`
- `crates/norn-cli/tests/support/mcp_launch_fixture.rs`
- `crates/norn-tui/src/app/active_input.rs`
- `crates/norn-tui/src/app/event_loop.rs`
- `crates/norn-tui/src/app/frontend_preferences.rs`
- `crates/norn-tui/src/app/frontend_preferences_tests.rs`
- `crates/norn-tui/src/app/mod.rs`
- `crates/norn-tui/src/app/render.rs`
- `crates/norn-tui/src/app/state.rs`
- `crates/norn-tui/src/app/turn/mid.rs`
- `crates/norn-tui/src/app/turn/run.rs`
- `crates/norn-tui/src/app/view_actions/commands.rs`
- `crates/norn-tui/src/app/view_actions/keys.rs`
- `crates/norn-tui/src/app/view_actions/mouse.rs`
- `crates/norn-tui/src/app/view_config.rs`
- `crates/norn-tui/src/events/display_toggles.rs`
- `crates/norn-tui/src/frontend_preferences.rs`
- `crates/norn-tui/src/frontend_preferences_tests.rs`
- `crates/norn-tui/src/lib.rs`
- `crates/norn-tui/tests/model_selection.rs`
- `crates/norn-tui/tests/pty_smoke.rs`
- `crates/norn-tui/tests/support/mcp_channels_tui.rs`
- `crates/norn-tui/tests/support/retained_workspace.rs`
- `crates/norn/src/config/mcp.rs`
- `crates/norn/src/config/mcp_patch.rs`
- `crates/norn/src/config/mcp_patch_tests.rs`
- `crates/norn/src/config/mcp_workspace_write.rs`
- `crates/norn/src/config/mcp_workspace_write_tests.rs`
- `crates/norn/src/config/mod.rs`
- `crates/norn/src/config/private_settings_document.rs`
- `crates/norn/src/config/settings_document.rs`
- `crates/norn/src/config/settings_write.rs`
- `crates/norn/src/config/settings_write_process_tests.rs`
- `crates/norn/src/config/tui_preferences.rs`
- `crates/norn/src/config/tui_preferences_tests.rs`
- `crates/norn/src/config/tui_preferences_types.rs`
- `crates/norn/src/config/workspace_settings_document.rs`
- `docs/TUI-PREFERENCES.md`
- `docs/release-notes/UNRELEASED.md`

## Constraints

- **SCOPE** — Only P1/P2 and necessary actual process proof. Preserve restored frontend styling and existing runtime policies. No new service, arbitrary limits, silent fallback, polling or lint bypass.

## Topics

### Common API

TuiPreferencesSnapshot::from_layer(scope: User|WorkspaceLocal, validated launch root, original: Option<Value>) captures the actual target path without rereading startup files. Present tui values must be objects. Capture only tui data and path/layer provenance, never whole settings containing credentials. No startup lock, directory creation or file write.

snapshot.patch(owned_keys: &[&str], replacement: &Map<String, Value>) acquires the existing configured resource permit and shared physical document lock, then freshly reads. Compare each owned key value against the original snapshot, preserving missing-versus-present values. A changed owned key is a named typed conflict. Preserve all unowned tui/document keys; remove an owned key absent from replacement.

Return the new snapshot and typed Unchanged, PublishedDurable or PublishedDurabilityUncertain(io error) outcome. Outer Err means no publication. Retain actual publication state, typed cause and path through write/rename/directory-sync and cleanup errors. Never report a post-rename failure as if nothing reached disk.

Extract the private document boundary from mcp_patch.rs into private_settings_document.rs, and replace mcp_workspace_write.rs with workspace_settings_document.rs. settings_document.rs/settings_write.rs hold shared lifecycle/publication types; tui_preferences_types.rs keeps opaque snapshot/scope/outcome shapes below 500 production lines. Preserve private .mcp-settings.lock, workspace directory lock, descriptor/no-follow handling, permissions, temp cleanup and fsync semantics. MCP writers reuse this same boundary, not an independent lock/writer.

Capture original user/project/local tui values and their whole-object winning layer in config/mcp.rs::load_resolved_settings_at_launch_root before merge consumes the loaded layers. R1 exposes that narrow snapshot through existing core ResolvedSettings only. R2 owns the separate runtime/resolve.rs threading into ResolvedInvocation and the TUI consumer; it is not R1 source work or an R1 completion requirement. No additional startup settings read, MCP transport activation, provider call or runtime reload. Core never depends on the TUI crate.

### Frontend schema and behavior

P2 owns only the tui.view, tui.display and tui.input object sections. Preserve composer and every unowned tui sibling unchanged. view fields: changes_open, split {conversation,changes}, upper_pane, expanded_tools, history_events, body_bytes, clipboard. display fields: thinking_visible, secondary_fields_visible. input field: submit_mode (steer|queue). Typed invalid/unknown owned fields fail by dotted path before raw mode/provider/MCP construction; no coercion or fallback. Absent fields use current declared defaults unchanged.

Preserve existing defaults: Changes closed; requested split1:1; upper Conversation; expanded_tools=false; history_events20; body_bytes65536; clipboard unspecified; thinking visible; secondary fields hidden; submit mode steer. Positive demands and positive u16 split weights retain their current validation. Saved split stores requested weights, never clamped geometry. Clipboard saves operator transport intent, never capability or acceptance proof.

Retain existing whole-object tui precedence across user, shared project and workspace-local settings. Higher tui objects shadow lower objects rather than merging individual fields. Show active run value, selected/written scope/path and effective winning layer, including saved-but-shadowed outcomes. Do not add a CLI preference override or new merge rule.

Working assumption chosen by root after the optional question had reasonable time without a reply: ordinary frontend preference changes automatically save to personal user settings by default, consistent with the request that choices be remembered. This is NOT a user-confirmed ruling. Run scope is temporary; workspace-local persistence is explicit opt-in. Preserve an explicit save action and /view preferences status. User means the existing NORN_HOME/user settings path; local means immutable canonical launch-root/.norn/settings.local.json, not the private MCP-only per-project file. Shared-project writes remain outside this checkpoint. Show active scope plus unsaved/pending/saved/failed or shadowed state truthfully. R1 snapshot/patch API and file wall are unchanged.

Pass typed initial preferences and opaque save authority through TuiInputs and all five current construction sites (CLI driver, model_selection, pty_smoke, support/mcp_channels_tui and support/retained_workspace); the TuiInputs struct definition is not a construction site. One shared frontend_preferences owner applies initial state and supervises at most one in-flight write through existing blocking/event facilities. Ordinary changes while a write is pending still update the usable current view and remain visibly unsaved until their actual value is persisted; never report the in-flight older value as saving newer changes. Reconcile against the actual completion and current eligible scope, then save the latest desired value only through the same existing event owner, without an unbounded queue. Run-only changes must not be picked up by a later completion for persistence. Observe an in-flight write on exit/cancellation. Preserve draft/run values on prepublication failure and retain truthful committed-but-unsynced outcomes. No timer, polling, debounce interval, background watcher, retry loop or new capacity is invented. As R2 settles, record the exact current-state/pending-write/completion transitions, failure-resumption rule and exit policy; do not claim a mechanism implemented before its source and tests exist.

Never persist ItemId, BodyRef, ViewSource, viewport/selection/search state, draft/submission text, pending steers or followups, session data, credentials or terminal capability replies. Preserve the restored blue prompt/plain composer/status rules and all retained source/reading/usage behavior. View preference changes/saves start or restart no MCP server and never become model input.

No Iridium dependency is needed for this checkpoint. Composer/send-key consumption remains NWP-05.3/NWP-05.4 later, using this same writer and preference owner. Do not create parallel composer preference storage or claim send-key support from P2.

### Superseded early save proposal — historical only

The earlier P2 proposal used /view preferences save user|local and said ordinary edits did not write implicitly. That proposal is superseded and is not current operational guidance or a user-confirmed decision. Current commands and the automatic-personal-saving working assumption are described in D10 and docs/TUI-PREFERENCES.md.
