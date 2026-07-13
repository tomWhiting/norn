# P0 D1C file-descriptor capacity correction

**Date:** 2026-07-12
**Status:** implementation complete; whole-P0 Gate C and Gate D remain open

## Scope and owner ruling

D1C addresses descriptor pressure introduced by descriptor-pinned private
storage and persistent process/session sinks. It does not claim that raising a
limit eliminates descriptor exhaustion, and it does not mutate a library
embedder's process-global limits.

The official `norn` binary now makes one initialization attempt before Clap or
runtime construction. It raises only the soft `RLIMIT_NOFILE` value, never
lowers an inherited value, never changes the hard value, and selects a finite
target as follows:

- macOS: `min(kern.maxfilesperproc, finite hard limit when present)`;
- Linux: `min(/proc/sys/fs/nr_open, finite hard limit when present)`;
- other Unix: the finite hard limit;
- unsupported or ceiling-discovery failure: no mutation and an explicit
  warning/doctor failure.

An inherited unlimited soft limit is preserved. Failure does not brick an
unrelated CLI command, but it is printed at startup and makes the descriptor
portion of `norn doctor` fail.

`RLIMIT_CORE=0` is not part of D1C. It would be inherited by user commands that
Norn launches and therefore remains a separate owner decision.

## Typed diagnostic boundary

`EMFILE` and `ENFILE` are classified by errno, never message text:

- `EMFILE` becomes `DescriptorExhaustionKind::Process`;
- `ENFILE` becomes `DescriptorExhaustionKind::System`;
- every other I/O error retains its existing error class.

The diagnostic carries a locally authored operation label, an optional path,
the observed soft/hard values, and a labelled `/dev/fd` or `/proc/self/fd`
count. Missing observations remain `None` with an explicit observation error;
they are never rendered as zero. Common error enums box this diagnostic so the
typed result does not inflate every `Result<_, ToolError/SessionError>` ABI or
trip `result_large_err`.

The typed value is preserved through the P0 storage and process boundaries:

- private session/index/event persistence to `SessionPersistError` and
  `SessionError`;
- process-manager root, spool, child spawn, and output-pipe construction to
  `ProcessError` and `ToolError`;
- foreground Bash spawn, redirect, read, flush, and spool-seed failures;
- task-store private-root, file, temporary publication, and lock operations;
- fetched-artifact publication;
- tool failure payloads as `kind = "resource_exhausted"` with structured
  `detail.descriptor`;
- integration diagnostics as `tool-resource-exhausted`.

Rendered errors direct the operator to `norn doctor`; no diagnostic tells a
user to run `ulimit`.

## Retained descriptor inventory

This inventory covers Norn-owned retained or operation-scoped descriptors in
the P0 surfaces. It does not claim an exact process total: TLS, DNS, Tokio,
loaded libraries, subprocess internals, and embedder code also allocate
descriptors.

| Boundary | Retained descriptors | Lifetime and notes |
|---|---:|---|
| `PrivateRoot` | 1 | One pinned root directory descriptor per live instance; descriptor-relative traversal opens temporary ancestor descriptors. |
| `JsonlSink` | 1 | One append file for the life of the persistent session sink. Index updates acquire their own operation-scoped lock transaction. |
| `IndexLock` | 2 | One pinned root plus one lock file for one index transaction; both close on success, timeout, or error. |
| `SessionArtifactStore` | 1 | One pinned session-data root for the active artifact capability. Each immutable artifact file is operation-scoped. |
| `SpoolWriter` | 0 | Stores path/capability metadata only; each tool-result spool publication is operation-scoped. |
| `ProcessManager` | 1 | One shared pinned Norn root per manager. Spools share this `Arc`; they do not open another root descriptor. |
| Managed background process | 3 plus child/runtime internals | One retained spool file and two captured output pipes while running. Supervisor/runtime descriptors are platform/runtime-owned and not counted here. |
| Foreground Bash | 2 normally | Captured stdout/stderr pipes while running. Crossing the redirect threshold additionally retains one output file and one pinned root until capture finalization. |
| Task claim guard | 1 | One pinned task root while the claim guard exists. The `O_EXCL` lock file handle closes immediately after creation; the name remains until release/drop. |
| Private reads/writes | operation-scoped | Root, ancestor, final file, temp file, and directory-sync descriptors close on return; exact simultaneous count depends on the operation. |

This correction raises capacity and makes exhaustion self-diagnosing. It does
not claim structural descriptor sharing or lazy reopen; those remain explicit
future optimization choices if measured pressure warrants them.

## File-size evidence

The two touched production modules that exceeded the 500-line gate were split
rather than grandfathered:

- `process/manager.rs`: production body ends before its test module at line
  435; launch/spool construction is in `process/manager/launch.rs` (232 total
  lines at this snapshot).
- `tools/web/fetch.rs`: production body ends before its test module at line
  417; parsing/conversion is in `tools/web/fetch/format.rs` (136 total lines at
  this snapshot).

The final whole-P0 syntax-aware inventory supersedes these snapshot line
numbers and covers every touched production file with one method.

## Focused verification

Commands and observed distributions:

```text
cargo test -p norn resource::descriptor::tests --lib
6 passed; 0 failed

cargo test -p norn session::persistence --lib
83 passed; 0 failed

cargo test -p norn process::manager::tests --lib
30 passed; 0 failed

cargo test -p norn tools::task::disk::tests --lib
16 passed; 0 failed

cargo test -p norn tools::bash::tests --lib
55 passed; 0 failed

cargo test -p norn tools::web::fetch::tests --lib
27 passed; 0 failed

cargo test -p norn-cli nofile::tests --lib
5 passed; 0 failed

cargo test -p norn-cli commands::doctor::descriptors::tests --lib
2 passed; 0 failed

cargo clippy -p norn -p norn-cli --all-targets -- -D warnings
pass
```

The whole-workspace Gate C rerun is deliberately not claimed here. It occurs
only after every remaining P0 correction is present and records the required
concurrency distributions rather than one lucky sample.
