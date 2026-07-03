# Norn-Process — Checklist

## Process Manager Core

- [x] **C1** — process/mod.rs contains only pub mod declarations and re-exports; logic lives in manager.rs, spool.rs, handle.rs.
- [x] **C2** — ProcessManager spawns background commands detached from any tool call, in their own process group, owning the stdout/stderr pipes for the process lifetime.
- [x] **C3** — The manager registry tracks processes under stable short ids with statuses (running, exited, killed) and imposes no cap on process count.
- [x] **C4** — A manager-owned background process is subject to no timeout, turn limit, or lifetime bound — it runs until it exits or is killed.
- [x] **C5** — ProcessHandle exposes status inspection, process-group kill, and exit notification.
- [x] **C6** — Runtime shutdown kills every still-running manager-owned process group and finalizes its spool; spool files persist on disk after shutdown.

## Spool

- [x] **C7** — Each process spools stdout and stderr to a single arrival-ordered on-disk file with stream-tagged lines and no size cap.
- [x] **C8** — Spools live under <norn_dir>/outputs/<session_id>/processes/ when a SessionId exists, and under a generated run-id in the same tree otherwise (NORN_HOME-aware via config::paths::norn_dir).
- [x] **C9** — Incremental spool reads return only content appended since the reader's cursor; multiple readers hold independent cursors.
- [x] **C10** — The spool publishes append notifications (committed-length watch) so a subscriber reacts to new output without polling — the watch attach seam.

## Bash Integration

- [x] **C11** — bash accepts run_in_background and, when true, returns immediately with the process id, spool path, and check instructions.
- [x] **C12** — A foreground bash command reaching its timeout is migrated to the background manager instead of killed; the result states it moved and how to check on it.
- [x] **C13** — Foreground drain-grace semantics for shell-backgrounded children (cmd &) are byte-for-byte unchanged; manager-owned processes never pass through drain-grace.
- [x] **C14** — Backgrounded and migrated bash results register follow-up actions for checking output and killing the process.

## Process Tool

- [x] **C15** — A process tool provides output, status, kill, and list operations over manager-owned processes.
- [x] **C16** — The output operation returns new-since-last-check content plus current status, advancing the model's cursor.

## Wake on Completion

- [x] **C17** — Process exit delivers a completion notice as an injected message via the durable path: live inbound steer when a channel is open, else the durable pending store with an agent_message.queued audit, plus an idle-child wake request.
- [x] **C18** — A completion notice wakes a lingering agent at its would-stop boundary via the steer wake set.

## Watches

- [x] **C19** — A watch (brief + agent-authored filter script) attaches to any manager-owned process at spawn or later, and detaches via unwatch; list surfaces active watches.
- [x] **C20** — Watch filters run deterministically against new spool regions on append notification — no model calls anywhere in the watch path.
- [x] **C21** — A filter match delivers an injected alert message carrying watch id, process id, brief, the matching excerpt, and the examined spool range.
- [x] **C22** — Filter execution failures are surfaced to the agent as alerts — never swallowed, never silently disabling the watch.
- [x] **C23** — On process exit the remaining unexamined spool region is filtered before the completion notice is delivered; watches then end.
- [x] **C24** — No caps on watch count per process or matches per watch.
- [x] **C25** — The watch layer consumes only the ProcessHandle subscription seam — it never reaches into manager or spool internals.

## Gates

- [x] **C26** — cargo clippy --workspace --all-targets -- -D warnings passes clean.
- [x] **C27** — cargo fmt --check passes clean.
