# TUI PTY Follow-Ups

These are follow-up items exposed by the real pseudo-terminal screen-state harness. The current harness has made the TUI much harder to fake-test accidentally, but it also exposed a few product and coverage gaps worth tracking separately from the completed rendering checklist.

## Resume Transcript Display

- [x] Restore prior visible conversation history when resuming a TUI session.
  - `norn-cli` already opens `--resume` / `--resume-if-exists` sessions with a replayed `EventStore`.
  - The model prompt path rebuilds provider context from that store on the next turn.
  - The TUI startup path clears the terminal and paints fresh UI state, so the human-visible scroll region starts empty.
  - Add a startup transcript projection from the session events into the scroll region before the first prompt is accepted.
  - Keep this separate from provider-context construction: the event store is the audit log, provider context is the prompt projection, and the TUI scrollback is the visual projection.
  - Implemented with startup replay through the same `render_event` path used for live transcript rows, including `TerminalGuard` scroll-cursor accounting.

- [x] Add a real PTY resume-history scenario.
  - Create or seed a persisted session with user, assistant, thinking, and tool-result events.
  - Start `run_app` with that non-empty store.
  - Assert the parsed screen model shows the prior transcript before any new input is submitted.
  - Verify later input still appends after the replayed transcript rather than overwriting fixed-panel rows.
  - Covered by `run_app_replays_resumed_session_history_in_screen_model`.

## Rendering Coverage Exposed By PTY

- [ ] Add PTY coverage for table rendering.
  - Include a markdown table that should render with rounded top/bottom corners and solid internal separators.
  - Assert parsed screen content, not only raw bytes.

- [ ] Add PTY coverage for thinking blocks.
  - Include GPT-style `**Heading**\n\nBody` reasoning summaries.
  - Assert the first body character is preserved, the heading is lifted into the `Thought about ...` line, and the block does not overwrite adjacent output.

- [ ] Add corpus-style PTY fixtures for tool rendering.
  - Cover bash stdout/stderr and non-zero exits.
  - Cover action-log list/detail/context/mutations/follow-ups/events output.
  - Cover agents list/messages output.
  - Cover LSP diagnostics/symbol output.
  - Cover patch/edit diffs.
  - Cover structured tool errors and unavailable capability paths.

- [ ] Upgrade remaining raw-byte resize evidence to screen-geometry assertions.
  - Resize tests should prove the final visible layout is coherent after DECSTBM changes, not only that the expected escape sequence appeared.

- [ ] Add scrollback-aware harness support.
  - The current `TerminalScreen` model verifies the final viewport.
  - Some TUI guarantees are about content moving into native scrollback rather than being lost, especially panel growth and long transcript replay.

- [ ] Make queued follow-up and cancellation state more visible.
  - Child-result delivery can queue follow-up prompts while the root turn is still active.
  - Ctrl+C can cancel active work and then queued work; the UI should make that sequence legible.
