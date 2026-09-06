---
type: design
cluster: norn-iridium-composer
title: Plain Iridium composer inside the existing Norn session
---

# Plain Iridium composer inside the existing Norn session

## Intention

Replace duplicated editing with one capable kernel while preserving the user’s appearance, startup and session control.

## Problem

Norn still owns independent mutable scalar input/wrapping, paste loops and pre-admission draft clearing; Iridium public cell APIs and NFP shared persistence now exist but are not yet assembled into this composer.

## Solution

The source now assembles one plain Iridium kernel facade, borrowed cell geometry in the retained Norn Frame, explicit admission/draft handling and a persisted Enter/Alt-Enter policy in the existing NFP owner. Candidate preview.5 is not installed; two observed modifier-translation failures and remaining composed verification still block acceptance.

## Decisions

### D1: Dependency and execution authority

> **Status:** registered_from_recorded_decisions

Checked NFP baseline d27259cc1c218a44be0ce624d103b04d799dcf21 (code fe6af320) is installed preview.4. Root accepted R0 and pinned both Iridium crates with default-features=false to private checked/pushed15673cba9222489e3c3315bb1526f60a6224a187, including ICC002/003/004. Earlier09cfa793/84d026f6 observations remain historical. ICC-004 at private commit15673cba9222489e3c3315bb1526f60a6224a187 implements iridium-tui default=["syntax"] and syntax=["iridium-editor/syntax", "dep:ropey"]. Both Norn Iridium dependencies now disable defaults. Feature-off preserves public cell editing/rendering/search/selection/caret/chrome and iridium-lang rules; its Frame has no parser/span/Rope highlight-snapshot cache, while the editor text buffer still uses Rope. Palette::highlighted is syntax-only and default Iridium TUI remains highlighted. The recorded no-syntax/default scopes passed378/393 tests, both strict Clippy and fmt; standalone compile with tree-sitter0.25.10 and the normal no-syntax subtree passed. These are bounded dependency checks, not Norn assembly or GPU/full-workspace/venue/Fable proof. Parser-free means only the Iridium composer subtree; Norn/Chiron0.25 and unrelated parsers remain. Checked local previews are authorized after their own source-bound acceptance; main Fable/exact205 remain separate. Root may set default-features=false on the existing workspace claude_runner dependency at unchanged Git revision643a1166f06a1f42961acf442f654670fbe9da22. Its default toon feature enables optional toon-format; that unused converter brings ratatui0.29/unicode-width=0.2.0, conflicting with the Iridium^0.2.2 requirement. Norn has no TOON/format-converter API use; its Claude adapter uses types::OutputFormat::StreamJson, separate from optional formats converters. Keep Text/Json/StreamJson, process/route/model behavior and all dependency source revisions unchanged. Resolve the lockfile for this explicit feature selection and existing composer dependencies; do not update unrelated dependencies. Run existing Claude adapter/wrapper tests and required composed checks to verify preserved behavior. This amendment authorizes only the feature flag/necessary resolved lock change, not a Claude route redesign or a passing graph claim.

### D2: One editor and one terminal owner

> **Status:** registered_from_recorded_decisions

One plain-text Iridium Editor replaces InputEditor storage/motion/wrapping. Norn owns terminal, reader, frame publication, focus, send and session lifecycle. No Iridium driver or second mutable buffer. Root owns a behavior-preserving extraction of finish_editor_action host-effect handling from app/event_loop.rs into app/composer_effects.rs, exposed as composer_effects::finish. Idle and active-turn callers use that one owner; app/mod.rs changes only the module declaration. This resolves the measured 518-production-line event_loop.rs excess without altering input order, clipboard/send/focus effects, acceptance, errors or terminal/runtime ownership. No extra feature, test file or duplicate event handler is added; keep the resulting production files within 500 lines. Root may remove the now-dead render/text.rs input_display_width helper and its single implementation-mirroring input_display_width_treats_controls_as_visible_placeholders test. The old input/wrap tab/control-placeholder path is retired; current Iridium kernel/cell geometry and actual rendering tests own input-width behavior. Do not retain dead production code behind cfg(test), add suppressions or weaken existing live geometry assertions. Other rendering helpers and tests remain unchanged.

### D3: One current geometry

> **Status:** registered_from_recorded_decisions

Use existing composer_input_area, no live prefix and bare Iridium chrome. The final borrowed geometry supplies cells/caret/hits; retain typed extent/stale-hit errors and visual affinity. Measure preparation work; do not add a global layout identity/cache. Root owns the narrow render/frame/diff.rs adapter-access amendment: widen only PreparedFrame::new and PreparedFrame::put from pub(super) to pub(in crate::render), and add dimensions() -> (u16, usize) at the same render-only visibility from the owned columns/cells grid. The sibling composer cell adapter uses those existing construction/insertion operations and validates destination dimensions before insertion. No grid ownership, cell mutation semantics, glyph clipping, delta encoding, terminal publication, rollback or renderer behavior changes are authorized by this amendment.

### D4: Reversible host transactions

> **Status:** registered_from_recorded_decisions

ICC-002 is the explicit prerequisite for public Editor::replace_cell_range(range, replacement, cursor policy) and CellReplacementCursor. The current bound private revision15673cba9222489e3c3315bb1526f60a6224a187 includes that API. In Norn, completion/recall/clear use one preflighted, undoable public transaction preserving pre-action selection, grapheme joins, refusal atomicity and isolated gesture grouping. Never copy private normalization/preparation or weaken undo; any additional API need requires its own prior Iridium registration. R2 clipboard uses authoritative ClipboardOperation plus an opaque kernel snapshot for collapsed-line and multicursor copy/cut. To prepare a cut, apply the public Command to cloned Document/CursorState only and retain that exact result; these are preflight scratch, not a second live editor. Before transport compare the actual copied strings for sanitation changes (not byte lengths); refuse sanitized destructive Cut before write. Only after successful transport write and flush, validate the unchanged snapshot and commit through checked ICC-002 replace_cell_range over the whole document with Exact(result_cursor), one undo transaction. Failure, sanitation or stale snapshot leaves live text/history unchanged. No private Iridium preparation or live raw apply_command is used. Clipboard adapter/transport remains R2; the already-registered kernel facade remains its existing owner, with no Iridium wall expansion.

### D5: Send policy is host configuration

> **Status:** registered_from_recorded_decisions

Default Enter sends; alt-enter is a typed option. Popup bare Enter/Tab comes first; paste never submits; delivery steer/queue remains independent. Modifier absence is not guessed.

### D6: Restored appearance is preserved

> **Status:** registered_from_recorded_decisions

Keep full-width plain live text, dim top-left mode/token rule, bottom-right model/effort/tier/session rule and last-row hints; existing three-row chrome yields at tiny height. Preserve blue user text on every line, > transcript prompt, two-column continuation and blank margin. No fourth control row.

### D7: Admission and recall ownership

> **Status:** registered_from_recorded_decisions

Prepare/admit/clear explicitly, preserve rejected draft/undo, accepted input identities and no-resend behavior. Private submission recall remains separate from editor undo; /auth exclusions unchanged. Root owns composer_submission plus the existing event_loop/mid/run orchestration seam. Keep exactly one pending local ItemId and ComposerSnapshot in AppState. Before clearing a pending submitted draft, consume the actual ExecutionObservation::opening_input push receipt for that input. Accepted-but-unavailable is still acceptance; never infer admission from text, labels, item order, replayed events or a fabricated receipt. A missing/rejected pre-admission outcome retains the exact draft/selection/undo; no polling or duplicate runtime input is added. Typed local command acceptance propagates from slash/model_selection/mcp_slash and the existing view command function alongside all current retained diagnostics. Distinguish rejected local commands from accepted commands even when the current handler renders a notice and returns Ok. Model capability/preflight checks and the original MCP start owner stay authoritative; parse/busy/preflight refusal does not admit or clear the draft, and a successfully started asynchronous operation is not replayed if later completion or notice/persistence handling fails. Preserve acceptance versus post-acceptance failure in the returned outcome. After an effect is accepted, later history/preferences persistence or result reporting failure must explicitly retain that accepted fact and its error; it cannot be returned as a rejection that invites repeating the effect. Local errors still render their original diagnostics. Existing agent/profile slash routing remains unchanged. R4 also owns the typed acceptance result of the existing app/frontend_preferences.rs command function, coordinated with its R6 owner. Decide accepted/rejected from the actual command branch: unknown subcommands are rejected, while an accepted scope/settings change followed by save-start, persistence or reporting failure remains accepted-with-error. Never infer outcomes from notice text, roll back a published settings change implicitly, replay a save/effect, or introduce a second preference owner. Only the command outcome propagation is added to the bounded slash partition; existing save lifecycle and publication semantics remain unchanged.

### D8: NFP owns persistence

> **Status:** registered_from_recorded_decisions

Extend existing typed FrontendPreferences and PreferenceOwner; no new writer/task/lock/store. Use install/edited/wait/finish/command/drain/exit_outcome and existing snapshot CAS. Add composer to same projection/owned keys; strict owned section means fixture sentinels move to an unowned sibling. Keep NFP automatic-personal assumption, run/local and truthful pending/shadowing outcomes.

### D9: Startup and input efficiency

> **Status:** registered_from_recorded_decisions

Reuse already-loaded layers and existing startup_trace; composer work adds no settings reads/locks/write, language inference/parse, MCP/provider activation, transcript replay/hydration or invented capacity/timer. ICC-004 at private commit15673cba9222489e3c3315bb1526f60a6224a187 implements iridium-tui default=["syntax"] and syntax=["iridium-editor/syntax", "dep:ropey"]. Both Norn Iridium dependencies now disable defaults. Feature-off preserves public cell editing/rendering/search/selection/caret/chrome and iridium-lang rules; its Frame has no parser/span/Rope highlight-snapshot cache, while the editor text buffer still uses Rope. Palette::highlighted is syntax-only and default Iridium TUI remains highlighted. The recorded no-syntax/default scopes passed378/393 tests, both strict Clippy and fmt; standalone compile with tree-sitter0.25.10 and the normal no-syntax subtree passed. These are bounded dependency checks, not Norn assembly or GPU/full-workspace/venue/Fable proof. Parser-free means only the Iridium composer subtree; Norn/Chiron0.25 and unrelated parsers remain. Measure actual Norn assembly without arbitrary pass thresholds. R7 may change only the existing tests/support/frontend_preferences_restart.rs App::frame helper visibility and API documentation so composer_preferences can call its existing source-position-bound, push/deadline complete-frame predicate wait. Reuse App::press for Alt-Enter instead of command/submit helpers that force bare Enter. Preserve the helper body, existing deadline semantics and all fixture assertions; no copied harness, polling, new runtime or other executable/signature behavior change. Parser-free is scoped to the Iridium composer dependency subtree, not the whole Norn graph. With default-features=false on both Iridium crates, the actual resolved graph must omit iridium-syntax, the Iridium tree-sitter0.26 edge and Iridium GPU dependencies. Norn/Chiron tree-sitter0.25 and unrelated parser-backed tools remain unchanged. Verify dependency origins/versions rather than asserting no tree-sitter package anywhere in Norn.

### D10: Registered execution and public-history boundary

> **Status:** registered_from_recorded_decisions

R0 now registers the eight documents on the checked NFP worktree. Root accepted R0; exact row dispatch and dependency readiness govern source edits; Current ICC-002/ICC-003/ICC-004 private checkpoint binding is fulfilled at15673cba9222489e3c3315bb1526f60a6224a187; composed Norn validation remains a prerequisite for its next local preview. Existing checked local-preview authority covers later build/install and successful-update speech. R0 claims only documentation/source-witness work. Main Fable/exact205 and any public-history rewrite/visibility/release remain separate; original programme IDs and historical receipts are preserved. Root owns the next local composer preview release note in docs/release-notes/UNRELEASED.md. Reserve 0.1.0-preview.5 for this assembled composer checkpoint; edit the note and already-registered Cargo version fields only after actual assembled checks pass. Describe the observed features, exact validation scope and remaining limitations; do not claim an installation before its receipt, or a main landing/public release/Fable/exact205 verdict. This registration performs neither the version bump nor the note edit. Current candidate status is assembled_pending_validation, not installed: type check completed, TUI833/835 passed with two translator defects under repair. No full/PTY/performance/preview.5 installation or main/Fable/205 verdict is claimed.

## Structure

- `Cargo.lock`
- `Cargo.toml`
- `crates/norn-cli/Cargo.toml`
- `crates/norn-cli/tests/composer_preferences.rs`
- `crates/norn-cli/tests/frontend_preferences_restart.rs`
- `crates/norn-cli/tests/support/composer_preferences.rs`
- `crates/norn-cli/tests/support/frontend_preferences_restart.rs`
- `crates/norn-tui/Cargo.toml`
- `crates/norn-tui/benches/iridium_composer.rs`
- `crates/norn-tui/src/app/autocomplete.rs`
- `crates/norn-tui/src/app/composer_effects.rs`
- `crates/norn-tui/src/app/composer_geometry.rs`
- `crates/norn-tui/src/app/composer_geometry_tests.rs`
- `crates/norn-tui/src/app/composer_submission.rs`
- `crates/norn-tui/src/app/composer_submission_tests.rs`
- `crates/norn-tui/src/app/edit.rs`
- `crates/norn-tui/src/app/event_loop.rs`
- `crates/norn-tui/src/app/frontend_preferences.rs`
- `crates/norn-tui/src/app/frontend_preferences_tests.rs`
- `crates/norn-tui/src/app/mcp_slash.rs`
- `crates/norn-tui/src/app/mod.rs`
- `crates/norn-tui/src/app/model_selection.rs`
- `crates/norn-tui/src/app/render.rs`
- `crates/norn-tui/src/app/render/composer.rs`
- `crates/norn-tui/src/app/render/composer_tests.rs`
- `crates/norn-tui/src/app/render/transcript.rs`
- `crates/norn-tui/src/app/slash.rs`
- `crates/norn-tui/src/app/state.rs`
- `crates/norn-tui/src/app/turn/mid.rs`
- `crates/norn-tui/src/app/turn/run.rs`
- `crates/norn-tui/src/app/view_actions.rs`
- `crates/norn-tui/src/app/view_actions/commands.rs`
- `crates/norn-tui/src/app/view_actions/keys.rs`
- `crates/norn-tui/src/app/view_actions/mouse.rs`
- `crates/norn-tui/src/error.rs`
- `crates/norn-tui/src/frontend_preferences.rs`
- `crates/norn-tui/src/frontend_preferences_tests.rs`
- `crates/norn-tui/src/input/autocomplete.rs`
- `crates/norn-tui/src/input/composer_clipboard.rs`
- `crates/norn-tui/src/input/composer_clipboard_tests.rs`
- `crates/norn-tui/src/input/composer_kernel.rs`
- `crates/norn-tui/src/input/composer_kernel_tests.rs`
- `crates/norn-tui/src/input/composer_keys.rs`
- `crates/norn-tui/src/input/composer_keys_tests.rs`
- `crates/norn-tui/src/input/composer_transactions.rs`
- `crates/norn-tui/src/input/composer_transactions_tests.rs`
- `crates/norn-tui/src/input/editor.rs`
- `crates/norn-tui/src/input/history.rs`
- `crates/norn-tui/src/input/keybindings.rs`
- `crates/norn-tui/src/input/mod.rs`
- `crates/norn-tui/src/input/navigation.rs`
- `crates/norn-tui/src/input/wrap.rs`
- `crates/norn-tui/src/render/composer_cells.rs`
- `crates/norn-tui/src/render/composer_cells_tests.rs`
- `crates/norn-tui/src/render/frame.rs`
- `crates/norn-tui/src/render/frame/diff.rs`
- `crates/norn-tui/src/render/frame_tests.rs`
- `crates/norn-tui/src/render/mod.rs`
- `crates/norn-tui/src/render/text.rs`
- `crates/norn-tui/src/terminal/clipboard.rs`
- `crates/norn-tui/tests/iridium_composer.rs`
- `crates/norn-tui/tests/retained_workspace.rs`
- `crates/norn-tui/tests/support/iridium_composer.rs`
- `crates/norn-tui/tests/support/retained_screen.rs`
- `crates/norn-tui/tests/support/retained_workspace.rs`
- `docs/TUI-PREFERENCES.md`
- `docs/design/norn-iridium-composer/CHECKLIST.md`
- `docs/design/norn-iridium-composer/DESIGN.md`
- `docs/design/norn-iridium-composer/USER-STORIES.md`
- `docs/design/norn-iridium-composer/briefs/NCP-001.json`
- `docs/design/norn-iridium-composer/briefs/NCP-001.md`
- `docs/design/norn-iridium-composer/checklist.json`
- `docs/design/norn-iridium-composer/design.json`
- `docs/design/norn-iridium-composer/stories.json`
- `docs/release-notes/UNRELEASED.md`

## Constraints

- **B1** — No changes outside each exact row wall; extra paths require prior amendment.
- **B2** — No second settings owner/TTY reader, Aion/Manifold dependency, runtime capacity change, side-panel file editor or daemon attach work.
- **B3** — No new lint suppression, unwrap/expect bypass or swallowed error; <=500 production LOC and declarations-only modules.
- **B4** — Local checked-preview implementation/build/install is already authorized when registered row dependencies and exact-source checks pass. R0 is documents only and source edits await root acceptance/dispatch. No public rewrite, visibility change, public release or main landing is authorized by this registration.
