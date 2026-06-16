# TUI Rendering Follow-Up Tasks

This is a focused worklist from the TUI rendering review. The current TUI is structurally solid, but these items are worth addressing before more rendering features are layered on top.

## High Priority

- [x] Add a real terminal lifecycle harness.
  - Run the low-level TUI setup/teardown inside a pseudo-terminal instead of only unit-testing pure render helpers.
  - Assert bracketed paste, scroll-region setup, cursor restore, wrap restore, and scroll-region reset sequences.
  - Current coverage lives in `crates/norn-tui/tests/pty_smoke.rs`.

- [x] Add an initial parsed screen-state PTY harness.
  - Run the real `run_app` entrypoint inside a pseudo-terminal with a deterministic `MockProvider`.
  - Wait for provider output, send Ctrl+C through the PTY master, parse captured terminal bytes through a screen model, and assert visible content.
  - Current coverage checks assistant output, submitted prompt text, and status-bar model text in `crates/norn-tui/tests/pty_smoke.rs`.

- [x] Extend the terminal integration harness with full screen-state scenarios.
  - Use a deterministic fake provider so tests require no network or API keys.
  - Feed keystrokes, paste payloads, Ctrl+C, and resize events through the PTY.
  - Parse output with a terminal screen model, such as a VT100 parser, and assert final screen state.
  - First scenarios: long soft-wrapped assistant output, panel growth/shrink, resize during streaming, Ctrl+C during a turn, bracketed paste, autocomplete accept/dismiss, and child-agent activity rows.
  - The harness must assert visible screen content and panel geometry, not only raw escape sequence presence.
  - Current coverage now includes real `run_app` PTY paths for provider output, long soft-wrapped assistant output, resize during streaming, bracketed paste plus slash autocomplete, panel growth/shrink without stale input artifacts, small terminal row budgeting, prompt child-result surfacing, and child-agent activity rows.

- [x] Deliver child-agent final results as soon as each child finishes.
  - The TUI owns the root `child_result_rx` while a turn is active and listens for completed child/fork results in the main `tokio::select!`, ahead of normal provider-event traffic.
  - Completed child results render immediately in the scroll region and queue the harness-framed `<agent_result>` as a follow-up prompt.
  - The headless/library runner also drains child results at every iteration boundary before building the next provider request, so results that arrive during a parent tool batch are injected on the next model turn instead of waiting for the parent to stop.
  - Do not stream every child log into root scrollback by default; deliver final results promptly and keep live child deltas as compact status/activity UI unless an explicit transcript view is added.

- [x] Make signal delivery reliable for live active and idle root agents.
  - The route/completion race is now rechecked against registry truth: if a send loses to terminal transition, `signal_agent` reports the recorded completion; if the registry still says live but the route is gone, it reports a distinct not-receivable invariant instead of a misleading route guess.
  - Active root delivery is handled by the root inbound channel threaded into every CLI/TUI `run_agent_step`.
  - Idle root delivery is handled by the same root inbound channel staying alive between turns, plus a pre-request runner drain so a message queued between turns is injected into the next provider request instead of waiting for that step's stop boundary.
  - Current coverage includes active route delivery, route/terminal race rechecks, and `inbound_message_queued_between_steps_reaches_next_request`.
  - A signal from a child or sibling to a live visible root agent is pushed to the target route whether the root is actively in a provider call or between turns.
  - Preserve the distinction between `steer` and `update`, but make the routing/wake semantics match the user's mental model: messages addressed to a valid visible agent should arrive.

- [ ] Define terminal child/fork signal continuation semantics.
  - Completed spawned agents and forks currently have no live loop to drain inbound messages: launch wrappers deregister their route after `run_agent_step` returns, and `Agent` is a single-use value consumed by `run`.
  - A proper fix must choose one explicit model: restart/wake a persistent child actor with retained provider/executor/tool/context state, or persist a pending delivery that a deliberately resumed child/fork turn drains automatically.
  - Do not add a hidden queue that no loop consumes; that would make `signal_agent` report success while the target never sees the message.
  - The current behavior for terminal children/forks remains an honest failure with the recorded completion status.

- [x] Render unsupported or unavailable tool paths clearly.
  - Example from smoke testing: an advertised image-search path was rejected by the current text-only web surface.
  - The TUI now checks structured tool error payloads before invoking per-tool renderers, so capability/tool-surface failures render as explicit `error [kind]: message` output instead of ambiguous `0 results` or raw JSON.

- [x] Add environment diagnostics for broken executable shims.
  - Example from smoke testing: `python3` resolved to a local shim that could not execute, while `/usr/bin/python3` worked.
  - `norn doctor` now probes an allow-list of known executable names on PATH and reports first-hit shims that are non-executable, fail to spawn, time out, or exit as a cannot-execute style wrapper.

- [x] Escape or sanitize control characters in the input renderer.
  - `render_input_frame` currently writes input text directly into the fixed panel.
  - Pasted escape/control bytes should not be able to corrupt terminal state.
  - Render control characters visibly, strip them, or normalize them before paint while preserving the editor buffer semantics intentionally.

- [x] Make visible thinking output cursor-safe.
  - `ThinkingDelta` currently writes dim text without advancing the software scroll cursor.
  - When thinking is visible, multi-line or wrapped thinking output can move the hardware cursor while `TerminalGuard` still believes it is elsewhere.
  - Either route visible thinking through the same tracked dim-preview path or call the appropriate cursor tracking hooks when thinking output is painted.

- [x] Add graceful row budgeting for very small terminals.
  - Activity rows already degrade when they would squeeze the scroll region too far.
  - Apply the same principle to popup rows, child-agent rows, and other optional fixed-panel surfaces.
  - Prefer hiding lower-priority panel sections before reaching a `terminal too small for TUI` error.

## Medium Priority

- [x] Fix `write_to_scroll` newline translation semantics.
  - The comment says only bare `\n` should become `\r\n`, but the implementation blindly replaces every `\n`.
  - Existing `\r\n` input currently becomes `\r\r\n`.
  - Update the implementation or the comment, and add tests for bare LF, CRLF, CR-only, and mixed input.

- [x] Truncate status bar fields explicitly.
  - `compose_left_right` currently allows over-width output and relies on terminal truncation.
  - Long model names, session IDs, or future `service-tier` / `effort` badges could make the status line look messy.
  - Truncate the left side, right side, or individual fields predictably with display-width-aware helpers.

- [x] Show current service tier and reasoning effort in the status bar.
  - `/fast`, `/service-tier`, and `/effort` currently write a confirmation line, but there is no persistent visual state.
  - Add compact badges such as `tier:fast` and `effort:high`.
  - Keep the badges hidden when values are default/none unless there is enough room.

- [x] Complete slash autocomplete coverage.
  - The TUI recognizes `/new`, `/quit`, and `/tools`, but the slash autocomplete list does not currently expose all of them.
  - Keep autocomplete, help text, and dispatch coverage in sync.

## Structural Cleanup

- [x] Lift slash command definitions into a shared registry.
  - Common built-in command metadata, surface filtering, effort parsing, and service-tier helpers now live in libnorn's shared slash catalog.
  - CLI and TUI adapters keep their UI-specific rendering/state mutations while consuming the same command rows for help, autocomplete, and dispatch.
  - `/compact` remains UI-adapted at the action boundary but shares the estimator and catalog metadata.

- [x] Remove duplicated TUI compaction estimation logic.
  - `/compact` in the TUI mirrors CLI token-freed estimation.
  - Share the estimator and compaction action once slash command logic is moved into a shared layer.

- [x] Decide whether session lifecycle end hooks should run on TUI errors.
  - The CLI driver currently fires session end hooks only after normal `run_app` success.
  - If hooks are cleanup-critical, wrap the TUI run in a finally-style path.

## Validation Checklist

- [x] `cargo check -p norn-tui --all-targets`
- [x] `cargo check -p norn-cli --all-targets`
- [x] `cargo test -p norn-tui --lib --quiet`
- [x] `cargo test -p norn-tui --test pty_smoke --quiet`
- [x] `cargo test -p norn manual_compaction_estimate --quiet`
- [x] `cargo test -p norn signal_agent --quiet`
- [x] `cargo test -p norn child_results --quiet`
- [x] `cargo test -p norn inbound_message_queued_between_steps_reaches_next_request --quiet`
- [x] `cargo test -p norn-cli commands::doctor --quiet`
- [x] `cargo test -p norn-cli commands::slash --quiet`
- [x] `cargo test -p norn-cli --test build_runtime_integration slash_state_builder_seeds_loop_context_with_all_cli_builtins --quiet`
- [x] `cargo test -p norn-tui app::tool_calls --quiet`
- [x] `cargo test -p norn-tui app::slash --quiet`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] PTY smoke test for parsed provider output, prompt text, and status bar state
- [x] PTY smoke test for resize plus streaming output
- [x] PTY smoke test for long soft-wrapped assistant output
- [x] PTY smoke test for panel growth/shrink without stale input artifacts
- [x] PTY smoke test for paste plus autocomplete
- [x] PTY smoke test for small terminal row budgeting
- [x] PTY smoke test for prompt child-result surfacing while a root turn is active
- [x] PTY smoke test for child-agent activity rows
