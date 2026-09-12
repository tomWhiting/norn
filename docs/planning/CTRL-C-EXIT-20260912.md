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
