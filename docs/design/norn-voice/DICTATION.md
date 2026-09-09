# D09: dictation into the existing composer

Design preparation, 9 September 2026, Melbourne. No dictation implementation
or live capture proof is claimed. D08 remains the separate reviewed candidate
`6f5ecedbef4e88dcf4eaaa774a7b76af0f3eabed`.

## Operator behavior

The person explicitly starts capture in the composer. Speech appears there
while they speak. Releasing the key finishes recognition into an editable
draft; it does not submit a message, steer a running turn, or wake another
agent. Sending remains the person's normal composer action.

The hold binding is editable through the existing key settings. It must not
consume ordinary Space while writing. Hold requires a terminal that actually
reports key release; otherwise explicit start/finish controls provide the same
operation without guessing a release from a timer. Repeated key-down events
do not begin additional captures. Leaving composer focus, changing the viewed
conversation, exiting, and losing the terminal each require capture cleanup.

Starting capture requests immediate speech hush through the native owner.
Stopping playback, ending capture, and cancelling an agent turn remain three
separate actions. The user can dictate while the model runs without the draft
becoming model input until explicitly submitted.

## Draft ownership

Capture binds these identities before any external effect:

- The Norn session and agent `ViewSource`.
- The Iridium document, revision, complete selection and insertion range.
- The native service session, registered seat and capture identity supplied
  by the agreed control contract.

Norn already captures document and selection identity in
`crates/norn-tui/src/input/composer_transactions.rs` (`ComposerSnapshot`,
`validate_snapshot`, `replace_snapshot_range`). Reuse this authority rather
than deriving a new insertion location when a transcript arrives.

`Partial.text` is a replacement for the whole current provisional transcript,
not an append-only token delta. Continuing committed chunks and the current
partial are different values; the final transcript must not repeat an earlier
chunk or its last partial. Preserve exact Unicode and newlines through the
Iridium cell adapter.

Provisional text must not produce one undo entry per recognition update. The
preferred integration is a revision-bound composition preview with one
reversible commit. A preview is not a second editable composer and cannot be
sent as if already committed. Investigate an Iridium composition transaction
before choosing the rendering implementation. No such API has been verified
in the pinned editor yet.

If the user edits the document, changes its selection, or switches recipients
while capture is pending, do not overwrite or retarget their work. Stop the
capture, retain the text and its original destination visibly, and require an
explicit insertion into a current draft. A late final must pass the same
source and revision checks as an on-time final. One accepted transcript is one
undoable insertion; cancelling a preview preserves the original draft and
undo history.

## External boundary to settle with Buckley

The inspected service source is
`/Users/tom/Developer/projects/gobetween-mouth-rows`. Its
`contract/src/door.rs` currently has untagged `Press` and `Release`, a
broadcast `Partial` with text/time/server session, and `Turn` with `to`,
`continuing`, `delivered`, and `dropped`. The published `Take`/`Give` behavior
can divert a committed turn away from the owning MCP session, but does not by
itself establish a capture lease or correlate a release with a final result.

Buckley was asked for these decisions through Meridian on 9 September:

1. The supported native capture endpoint and exclusive seat ownership rule.
2. Capture identity on provisional and final updates, or another specified
   ordering mechanism that distinguishes old audio and concurrent clients.
3. A terminal result for finish with no speech, cancellation, and failure.
4. What happens if another client changes holder or mode during capture.
5. How mode, microphone and floor ownership are released on disconnect,
   without restoring stale state over another client's later choice.
6. How native dictation avoids duplicate admission through the MCP Channels
   notifier, including the hub's retained words from before the press.

Do not select the next broadcast as our capture. Do not send global mode,
give, or hush commands whose authority has not been agreed. The service owns
audio resources; the Norn adapter must not start another speech model or a
parallel dictation path. Norn's part will use the committed typed contract.

## Implementation rows once the capture contract is settled

| Row | Work | Acceptance |
| --- | --- | --- |
| D09.1 | Native capture owner and typed receipts | Correct seat/capture correlation; silent finish; refusal; disconnect; no automatic retry or duplicate delivery |
| D09.2 | Iridium provisional composition | Replacement partials, Unicode, continuing chunks, one undoable final insertion, cancellation preserves draft |
| D09.3 | TUI input and lifecycle wiring | Editable hold controls, repeat/release handling, focus changes, active-turn independence, visible cleanup outcomes |
| D09.4 | Documentation and delivery | Setup and shortcuts documented; actual capture and echo tests; strict Clippy/fmt; Fable review and exact-commit 205 battery |

Write the numbered implementation brief and exact file walls before editing
these seams. D09.2 can be designed while D09.1's external contract is settled;
capture admission must wait for that contract. Audio tests must cover both
normal finish and interruption on the actual service, not just wire fixtures.
