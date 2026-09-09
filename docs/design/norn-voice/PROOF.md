# NV-001 local verification

9 September 2026, Melbourne. These are local development checks for the
read-aloud candidate, not a venue battery or a live-audio acceptance claim.
The source was unchanged between the checks below and the review commit;
this proof document and captured logs were added afterward.

All Cargo output used `/Users/tom/Developer/ablative/stack/norn/target`,
with `TMPDIR` set to that directory's `tmp` child and `-j 2`.
Working directory: the repository's `.worktrees/voice-readaloud`.

| Check | Result | Unedited output |
| --- | --- | --- |
| `cargo clippy --locked --workspace --all-targets --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2 -- -D warnings` | Exit 0 | [Clippy](proof/voice-clippy.log) |
| `cargo test --locked -p norn --lib integration::locutus::tests --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2` | Exit 0; 6 passed | [Socket tests](proof/voice-socket-tests.log) |
| `cargo test --locked -p norn-tui --lib --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2` | Exit 0; 915 passed | [TUI tests](proof/voice-tui-tests.log) |
| `cargo fmt --all -- --check` | Exit 0; no output | — |
| `git diff --check` | Exit 0; no output | — |
| Existing design-system `check-coverage.py docs/design/norn-voice` | Coverage clean; 3 checklist items, 3 stories, 1 brief | — |

The socket tests run typed Unix socket fixtures. They cover request correlation,
stop before admission, measured stop receipts, unrelated broadcasts, EOF with
unknown outcome, and cancellation before connection. They do not synthesize or
play audio. The TUI suite includes disabled startup, draft preservation, strict
settings, stale-source rejection, command discovery and editable shortcuts.

Pending: Fable review, real playback and stop against Buckley's gated Locutus
service, and the exact-commit 205 battery. No merge or installation is claimed.
Dictation and streamed spoken/written sections are separate deliveries.
