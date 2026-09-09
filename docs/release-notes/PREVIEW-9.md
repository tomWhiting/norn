# Preview.9 candidate and verification

9 September 2026, Melbourne. This record describes source prepared for
`0.1.0-preview.9`. It does not declare a public release or installation.
The installed executable was checked during preparation and still reported
`norn 0.1.0-preview.8`.

## Included behaviour

- **Native read-aloud:** `/voice read`, `/voice stop`, `/voice replay`, saved
  preferences and editable Option+Shift+V / Option+Shift+X shortcuts. Norn
  connects to an already-running compatible Locutus control socket; it does
  not start an audio service or ask the model to call a speech tool. Disabled
  voice creates no task or connection. Automatic playback is separately
  opt-in. See [native voice](../NATIVE-VOICE.md).
- **Persistent driven execution:** a driver can opt in with
  `initialize.params.runLifecycle: "persistent"`, then execute sequential
  requests on the same runtime and conversation. Default one-shot clients
  retain their terminal response followed by stdout EOF. Idle channel input
  does not start a run. `/clear` returns the replacement session ID and closes
  with an explicit session-rotation receipt. See [driven mode](../DRIVEN-MODE-GUIDE.md).
- **Progressive terminal colour:** colour capability does not refuse
  interactive startup. RGB, indexed colour, ANSI16 and terminal defaults
  share the existing renderer. Reduced colour produces one in-interface
  notice. Composer selection and caret remain visible in monochrome.

Live detach/reattach, dictation, streamed spoken/written sections and
model-visible interruption receipts are separate deliveries. Persistent RPC
alone is not an attachable background session.

## Source and proof

The three branches are combined before final verification. Their individual
checks establish each lane's scope, not a passing combined or venue battery.

| Lane | Checked source | Evidence |
| --- | --- | --- |
| Native read-aloud | `1cd0b35a8e23b18e984b0b916a91f5106b2bec95` | [9 socket and 919 TUI tests, Clippy, fmt and review record](../design/norn-voice/PROOF.md) |
| Persistent RPC | `228b9fd5c2762be23a59775b556fb76569aa9108` | [566 CLI library, 12 native RPC, 3 signal tests, Clippy and fmt](../design/norn-cli/proofs/NDR-001-local.json); proof-only follow-on `deab26a0564d27d9a3bb6fc8ee4050ba23e0ee34` |
| Terminal colour | `7ce01292fad30643ac366737f085265c8fdbf3ae` | [915 TUI tests, strict Clippy and fmt](../design/terminal-colour/PROOF.md); includes real Iridium selection and caret regression tests |

Waffles approved the corrected read-aloud source at 14:52 Melbourne and
RPC source `228b9fd` at 15:05 on 9 September. The final capability-construction
cleanup and colour correction require their re-review. All build
output and test scratch files for this delivery stay under the Norn repository,
with the shared Cargo target at its `target/` directory.

## Combined local checks

Strict workspace/all-target Clippy including `norn/live-api-smoke`, formatting,
566 CLI library tests, 12 native RPC tests, 3 signal tests, 929 TUI tests and
9 native voice socket tests passed. The final CLI-only capability cleanup was
followed by the CLI suite and strict Clippy again; the TUI and core source
remained unchanged. [Exact commands, output and changed-source hashes](PREVIEW-9-local-checks.json)
record that boundary. No live provider or audible playback was exercised. The initial
combined record belongs to `b3547a4e82bec909cfcd0f2bf19ef7e528ee5e69`;
`run.rs` and `capabilities.rs` are re-hashed in the
[extraction record](PREVIEW-9-extraction-checks.json), including its final
wording-check section.

## Structural policy correction

The AST/tokei check on initial candidate `b3547a4` found `app/turn/run.rs`
at 501 production lines after the voice select arm was added. Waffles extended
NV-001's wall at 15:15 Melbourne on 9 September (Meridian
`300b32be-cd12-4208-a34d-a55a9b8a99e0`). The unchanged turn input enum and
initial-state reset now live in `app/turn/seed.rs`; `run.rs` measures 490
production lines and `seed.rs` 15. Existing turn behaviour and tests are unchanged. The 929-test TUI suite,
strict workspace/all-target Clippy, formatting and brief coverage passed after
extraction; [exact checks and source hashes](PREVIEW-9-extraction-checks.json)
record the final change.

That initial scan found no added forbidden constructs. Its other finding,
`crates/norn/src/tools/agent/mod.rs:47`, is a pre-existing constant in a
module entry point, byte-unchanged from `d9873d0`. Waffles assigned it a separate
row in the post-landing documentation/backfill brief alongside C35 and C124.
The [repeated AST policy report](PREVIEW-9-policy.json) on `ae13a81`
confirms no over-limit files, no added forbidden constructs and only that
unchanged module finding. The subsequent change updates release documentation
and one capability comment only. It remains a recorded baseline policy failure; this candidate does not claim a
clean whole-repository structural-policy verdict or alter that file.

## Remaining acceptance

1. Submit the final full commit ID for review and the exact-commit 205 battery.
   Earlier attempts refused before running a test: the runner checked the
   compiler outside the provisioned Norn tree. Waffles owns that runner repair;
   no passing venue receipt is implied by those attempts.
2. Land the accepted commit on `main`, push it and build/install that source
   locally, preserving a rollback binary and recording its hash and version.
3. With Tom in his own terminal, have Norn read a completed answer through
   Locutus/Dot on his Mac and exercise stop. Socket fixtures do not prove audible
   playback. Record this installation acceptance separately from code review.

The version change, lockfile, README and this release record belong to the
candidate submitted for the battery. Installation receipts can name that
commit without changing the code which was tested.
