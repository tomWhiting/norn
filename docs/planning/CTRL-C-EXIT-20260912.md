# Norn Ctrl+C exit follow-up — 12 September 2026, Melbourne

Input: Tom reports that Ctrl+C exits immediately and subsequent mouse movement types escape fragments into the shell. He requests a second Ctrl+C within two to five seconds before exit. Existing retained-screen ownership is described in docs/design/norn-retained-tui/briefs/NUI-005.md; September 8 continuity/terminal work remains in NEXT-WORK-20260908.md D01/D06.

## Row 1: idle exit confirmation

Wall: crates/norn-tui/src/app/{exit_confirmation.rs,exit_confirmation_tests.rs,event_loop.rs,mod.rs,state.rs,turn/run.rs,render/composer.rs}, crates/norn-tui/src/input/keybindings.rs, crates/norn-cli/tests/support/frontend_preferences_restart.rs, Cargo.toml, Cargo.lock, README.md, docs/release-notes/UNRELEASED.md, this document.

Source patch: first idle Ctrl+C clears the draft and arms a three-second monotonic window; second distinct press before expiry exits. Expiry, other keyboard input, paste, or starting an agent turn clears confirmation. The existing render tick expires it. Existing MCP-operation exit protection remains. During a turn Ctrl+C remains turn-local cancellation; /exit remains explicit exit. Footer confirmation takes priority over other hints in narrow panes. Existing key-repeat filtering rejects repeated exit actions.

Acceptance references: rg 'ExitConfirmation|exit_confirmation' crates/norn-tui/src/app; rg 'ctrl_c_repeat_does_not_confirm_exit' crates/norn-tui/src/input/keybindings.rs. Added deterministic boundary/expiry/reset/repeat test cases. These searches are code references, not pass evidence.

Status: source patch only. No builds, tests, lint, or installation performed. Review and focused verification plus Ghostty/Manifold test drive are required before release. Unrelated working-tree changes are preserved.

## Row 2: mouse reporting after exit

Investigation only; no mouse-mode source change. crates/norn-tui/src/terminal/setup.rs already sends mouse disable sequences for modes 1002 and 1006 on guard drop and panic restoration. Ctrl+C idle exit returns through that guard. This does not prove the sequence reaches the outer terminal or is honored by the multiplexer. Next: compare Ctrl+C and /exit in plain Ghostty and Manifold, capture the emitted shutdown bytes and post-exit mouse mode, and inspect input reader ownership and alternate-screen restoration ordering. Do not claim that broadly disabling unrelated modes repairs the cause without reproduction.

## Local preview.10 integration

Tom authorized build/install on 12 September at 23:49 Melbourne. Patch moved onto integration-candidate at 7ec7ee2, preserving installed preview.9 source (f86ffac) and later documentation. Root main is still preview.8; do not build this delivery from that older checkout. The existing actual-CLI PTY fixture now observes the first-press confirmation frame before sending the second press and retains its terminal-restoration assertions. Release build and focused verification are in progress; no venue receipt or mouse-leak fix is claimed.

## Local verification and keyboard follow-up — 13 September 2026

Preview.10 installed from 9d13a7b: release build, actual-binary double-Ctrl+C PTY probe, all 934 TUI library tests, and the CLI automatic-user-restart/temporary-run PTY scenario passed. Earlier source-only status above is superseded by these results. Tom reports mouse movement works after exit, but Option+Delete emits escape fragments.

Row 3 wall: crates/norn-tui/src/terminal/{setup.rs,setup_tests.rs}, Cargo.toml, Cargo.lock, README.md, docs/release-notes/UNRELEASED.md, this document. The actual installed preview.10 PTY byte stream reproduces a keyboard push on the primary screen followed by a pop on the alternate screen; the simulated parent's stack ends [1,5] instead of [1]. The probe lives at var/releases/preview.10-ctrl-c/check_kitty_pty.py. Preview.11 moves the push inside alternate-screen ownership and shares restoration state between normal drop and panic cleanup. Focused regression cases cover screen-local stacks, repeated cleanup, pre-screen admission and failed screen flush. Verification: release build, all 937 TUI library tests, formatting and diff whitespace checks passed. Targeted AST checks found no unwrap/expect calls in the two changed Rust files; no lint-bypass attributes or discarded-result bindings were found there. The actual preview.11 binary PTY probe observes enter-alternate, push, pop, leave-alternate in that order; the simulated parent keyboard stack remains [1]. Logs are in var/releases/preview.11-keyboard. Strict Clippy, physical Herdr verification and the venue battery are not yet claimed; no main landing.

## Test-drive UI follow-ups — 13 September 2026

These are requested work, not implemented by preview.11.

- Resumed scrollback: navigation exhausting the loaded initial page must request older events through the existing request_older/load_older path. Current automatic navigation does not set that flag; /view older does. Preserve source-bound anchors and avoid eager full-history loading. References: app/session_replay.rs, app/render/navigation.rs, app/render.rs, app/view_actions/commands.rs.
- Transcript gap: investigate forward scrolling beyond a full final viewport and placement of short follow-latest windows in app/render/transcript.rs. Confirm expected geometry before changing alignment.
- Tool rows: colour the tool name, follow with tool_use_description on a plain collapsed background; reserve background emphasis for expanded details. Show readable inputs/outputs and command text, with raw input as an explicit alternate view. Retain brief live preview and single-line completion.
- File pane: one tab per changed file, syntax colouring and distinct addition/deletion highlighting are required. Follow the latest edit while idle; preserve a user's historical inspection. Allow browsing successive recorded versions/diffs within that file's tab. Identify writes versus edits clearly. Keep the conversation/composer geometry and avoid new global header rows. Version provenance and retention need a separate bounded brief before implementation.
