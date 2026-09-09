# NV-001 local verification

9 September 2026, Melbourne. These are local development checks for the
read-aloud candidate, not a venue battery or a live-audio acceptance claim.
The source was unchanged between the checks below and the review commit;
this proof document and captured output were added afterward. The JSON `output`
field preserves exact command output, including trailing newlines.

All Cargo output used `/Users/tom/Developer/ablative/stack/norn/target`,
with `TMPDIR` set to that directory's `tmp` child and `-j 2`.
Working directory: the repository's `.worktrees/voice-readaloud`.

| Check | Result | Verbatim output in JSON |
| --- | --- | --- |
| `cargo clippy --locked --workspace --all-targets --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2 -- -D warnings` | Exit 0 | [Clippy](proof/voice-clippy.json) |
| `cargo test --locked -p norn --lib integration::locutus::tests --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2` | Exit 0; 9 passed | [Socket tests](proof/voice-socket-tests.json) |
| `cargo test --locked -p norn-tui --lib --target-dir /Users/tom/Developer/ablative/stack/norn/target -j 2` | Exit 0; 919 passed | [TUI tests](proof/voice-tui-tests.json) |
| `cargo fmt --all -- --check` | Exit 0; no output | — |
| `git diff --check` | Exit 0; no output | — |
| Existing design-system `check-coverage.py docs/design/norn-voice` | Coverage clean; 3 checklist items, 3 stories, 1 brief | [Coverage](proof/coverage.json) |

The socket tests run typed Unix socket fixtures. They cover request correlation,
stop before admission, bounded unanswered hush, late complete-playback receipts,
untagged errors before/after admission, measured stop receipts, unrelated broadcasts, EOF with
unknown outcome, and cancellation before connection. They do not synthesize or
play audio. The TUI suite includes disabled startup, draft preservation, strict
settings, stale-source rejection, command discovery, editable shortcuts,
connect failure, busy-control refusal, and retained unknown/late-stop outcomes.

Waffles's re-review approved corrected commit `1cd0b35` at 14:52 Melbourne
on 9 September 2026, with the listed checks reproduced independently. His
Meridian receipt is `4cd90028-7e54-4e30-a79e-0783a6c6efde`. The exact-commit
205 battery and real playback and stop against Buckley's gated Locutus service
remain outstanding here. Live audio is the installation acceptance with Tom,
after the combined candidate is built. No installation is claimed.
Dictation and streamed spoken/written sections are separate deliveries.

Use the filter `integration::locutus::tests`. The misspelling
`integration::locutus_tests` matches zero tests and is not proof.

## Review corrections

Waffles's review of `6f5eced` requested D1–D5. The revised source bounds stop
supervision, preserves request ownership across untagged broadcast errors,
and retains a late stop request in a complete-playback outcome. The deadline
is his owner ruling at 14:35 Melbourne on 9 September: five seconds, covering
write and receipt wait, with an explicitly unknown result on expiry.

Proof output is now stored as JSON strings, preserving all bytes without the
trailing blank lines that made the earlier committed logs fail diff checking.
Coverage output is attached. The README build target is documented separately
in DESIGN.md as Tom's explicit repository-local build instruction.
