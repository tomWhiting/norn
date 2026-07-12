# P0 artifact-writer inventory

**Date:** 2026-07-12  
**Phase base:** `41ea210`  
**Snapshot code head:** `37c806a`  
**Status:** complete candidate inventory; regenerate after D1D before final Gate C  
**Scope:** every production or build-script filesystem-mutation candidate in
`crates/**/*.rs`; test-only AST items and integration-test crates are excluded

## Evidence method

`docs/reviews/evidence/run_p0_policy_evidence.py` performs a repository-wide,
syntax-aware test exclusion followed by a deliberately conservative lexical
sweep for filesystem roots, opens, creates, directory creation, publication,
rename, truncation, and removal. The exact raw rows, including file, line, and
source text, are retained as `artifact_writer_candidates` in
`docs/reviews/evidence/2026-07-12-p0-policy.json` at the final P0 head.

The candidate list is not treated as type inference. It deliberately retains
read-only opens, cleanup operations, and a semantic `SessionManager::rename`
false positive so each can be classified visibly. The sweep includes aliased
`fs::write` calls and descriptor-relative `create_dir_all` calls. A Cargo build
script remains a production build target even when an integration test imports
that file under `#[cfg(test)]`; the checker has an explicit regression-safe
classification for that case.

Run from the repository root:

```sh
python3 docs/reviews/evidence/run_p0_policy_evidence.py \
  --base 41ea210 --head HEAD \
  --output docs/reviews/evidence/2026-07-12-p0-policy.json
```

## Implicit private artifacts

Repeated candidate rows in one implementation family are grouped below. Every
raw row belongs to exactly one table row or to the non-artifact classification
later in this document.

| Family and implementation | Owner, lifetime, and root | Filesystem contract | Model read surface |
|---|---|---|---|
| Provider debug JSONL: `provider/debug.rs` | Explicit trusted operator configuration; retained until the operator removes it; absolute selected parent | Parent pinned through `PrivateRoot`; missing ancestors private; regular no-follow final file; `0700`/`0600`; append-only. Individual append calls are not claimed to be a cross-process transaction. | None automatically. The path and raw dump are not placed in model context. |
| Session event files, child/fork timelines, index, lock, and atomic temporaries: `session/persistence/{io,index,lock}.rs`, `session/{branch,manager}.rs` | One persisted session tree beneath the trusted session data root; retained with the session | Descriptor-relative private tree; regular-file and link rejection; index mutation lock; exclusive temporary creation; fsync according to durability policy; no-replace publication for new sessions and atomic rename where replacement is intended. Removal candidates are confined cleanup, not additional writers. | No generic filesystem authority. Resume code reads validated events and supplies the derived conversation state. |
| Immutable fetched documents: `session/artifacts.rs` | One root session; retained with the session under `<session>/artifacts/fetched/` | Descriptor-relative `0700`/`0600`; UUID exclusive creation; no overwrite; configured durability syncs the file and complete directory chain. | Only the active session artifact subtree is added as a read/search exemption. Other sessions, transcripts, indexes, and credentials remain unavailable. |
| Oversized persisted tool results: `session/spool.rs` | One event in one persisted session under `<session>/spool/`; retained with the session | Descriptor-relative private directory and exclusive file; verbatim serialized bytes; write completes before the referencing event; configured durability syncs the file and directory chain. | The model receives the bounded projection, not automatic access to the full spool. The validated read side exists for resume/forensics. |
| Foreground Bash threshold output: `tools/bash/output.rs` | One session/tool call under `$NORN_HOME/outputs/<session>/<call>.log`; retained until user cleanup | Trusted absolute Norn root; validated components; descriptor-relative private directories and exclusive regular file. No overwrite or link following. | The tool result returns the path and read/search follow-ups can address that specific output. |
| Managed process output: `process/{manager,spool}.rs` and `process/manager/launch.rs` | One process-manager run under `$NORN_HOME/outputs/<session-or-run>/processes/<run>/<id>.log`; retained after process exit | One pinned `PrivateRoot` shared by the manager; validated path components; private directories and exclusive files; serialized async append/flush and committed-length cursor. | The process tool exposes the spool path and bounded cursor reads; output is not injected wholesale. |
| Persistent tasks and claim locks: `tools/task/disk.rs` | Explicit cross-session task group beneath `$NORN_HOME/tasks/<group>/`; lives until task/group deletion | Descriptor-relative private tree; validated group/task IDs; exclusive lock creation; sibling temp plus fsync and atomic rename/no-replace publication; link/non-regular rejection. Removal rows are scoped task/lock cleanup. | Only through the task tool's typed operations; the task root is not a generic read exemption. |
| TUI input history and sibling lock: `resource/private_line_log.rs`, `norn-tui/input/history.rs` | Current user across TUI runs at `$NORN_HOME/history.txt`; retained until user removal | Trusted `norn_dir`; pinned private parent; validated UTF-8 final component; regular no-follow data and lock files; `0700`/`0600`; advisory inter-process lock across reads/appends; unterminated tails ignored on read and truncated before append; corrupt/unreadable backing is disabled rather than extended. | No direct model file authority. Recalled entries become user input only when the user selects/submits them. `Debug` reveals counts/presence, not prompts or drafts. |

### Layout boundary

The foreground Bash and managed-process artifacts are private, but they remain
under `$NORN_HOME/outputs/...`, not the owner's desired eventual
`$NORN_HOME/sessions/<session>/outputs/...` layout. P0 closes privacy,
workspace-placement, and link-confinement defects; it does **not** claim the
broader session-storage redesign. That design remains part of the explicit
pre-P3 transcript/storage discussion so references, forks, retention, and
migration are decided together.

## Shared foreign credential artifact

| Family and implementation | Classification |
|---|---|
| Codex-compatible `$CODEX_HOME/auth.json`: `provider/openai_oauth/storage.rs` | This is a shared foreign compatibility store, not a Norn `PrivateRoot` artifact. Writes use a unique `0600` sibling temporary, file fsync, and atomic rename; load/delete share the Codex CLI location. Cross-process reload-lock-refresh-save correctness, foreign-writer detection, directory/link hardening, and durable rename semantics are explicitly owned by P2 `AUTH-02`/`AUTH-06`. P0 does not fold this store into its private-session coverage claim. It is never model-readable. |

## Explicit user-directed writers

These paths perform requested workspace or operator-selected output mutation.
Moving them into the private Norn root would change the requested operation,
so they are not implicit private artifacts.

| Candidate family | Authority and semantics | Model visibility |
|---|---|---|
| Rhai `write`/`write_json`: `integration/rhai/blocking.rs` | A script explicitly requests the supplied path. Ordinary directory creation and write semantics apply. | The invoking extension owns the value/path. |
| File write/patch commit paths: `tools/{file_commit,patch_commit,write}.rs` | A model tool explicitly targets a workspace path already constrained by tool/workspace policy. Sibling staging, cleanup, and rename implement the requested edit. | The tool result reports the requested mutation. |
| CLI init and upgrade output: `norn-cli/commands/init/{conventions,upgrade}.rs` | The operator invokes generation and may select an alternate output path. | No model surface. |
| CLI structured/step output: `norn-cli/print/step_output.rs` | The operator explicitly chooses an output file. Ordinary create/write behavior is intentional. | No model surface unless the operator later supplies the file. |

## Build and diagnostic writers

| Candidate family | Classification |
|---|---|
| Generated model catalog: `crates/norn/build.rs` | Cargo build-time output under Cargo-provided `OUT_DIR`. It is neither runtime user data nor model-readable. The aliased `fs::write` is retained in the raw inventory to prove build scripts were swept. |
| Doctor permission scratch: `norn-cli/commands/doctor.rs` | Empty, short-lived file deliberately created in the current directory to diagnose workspace writability and immediately removed. It contains no prompt, credential, response, or tool output and is not a retained artifact. Failure/cleanup is reported by doctor. |

## Non-writer and cleanup candidates

- `PrivateRoot::open` rows in session manager, persistence, spool, and task code
  are read/reopen roots. Their nearby remove rows delete confined session/task
  entries or failed temporaries; they create no new artifact family.
- `system_prompt/environment.rs` constructs `OpenOptions` for a read-only
  `.git` metadata probe. It does not request create, write, append, or truncate.
- `norn-cli/commands/slash/registry.rs` calls the semantic
  `SessionManager::rename` operation. The raw lexical matcher retains it, but
  the actual filesystem mutation is already classified under session index
  persistence.
- Cleanup `remove_file`/`remove_dir_all` candidates in OAuth, session, task,
  file-commit, and patch-commit paths remove their family's temporary, lock, or
  requested destination. They do not imply another writer or read surface.

## Coverage conclusion

The raw JSON enumeration plus the classifications above support the narrow P0
claim: every implicit runtime artifact writer currently identified is either
on reviewed private storage, explicitly classified as the shared foreign OAuth
store owned by P2, or not an implicit artifact at all. This conclusion does
not claim that all storage has the final session-centric layout, that the
foreign OAuth transaction is correct, or that user-requested workspace/output
writes should be private.
