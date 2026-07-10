# norn-process — Background-process manager and watches

**Scope:** the `crates/norn/src/process/` module family from
`docs/design/norn/INTERNAL-AGENTS.md` §3 (background-process manager,
owner-ruled 2026-07-03 to the Claude Code Bash-tool standard) and §5
(deterministic watches over managed processes). The cheap-model watcher
*agent* layer (§5 step 2) and the digest processor (§4.2) are future
internal-agents work and are explicitly out of this cluster.

## Why this exists

Norn's bash tool is strictly synchronous by design: the drain-grace
mechanism (`crates/norn/src/tools/bash/process.rs:36-48`) exists precisely
to stop shell-backgrounded children (`cmd &`) from holding the tool's pipes
past process exit. Everything long-running — dev servers, long test runs,
watched builds — therefore either blocks a turn or loses its output.
INTERNAL-AGENTS §2 records this as the one deliberately absent primitive.

This cluster adds the primitive:

- **NP-001** — the manager (`process/manager.rs`, `process/spool.rs`,
  `process/handle.rs` per INTERNAL-AGENTS §9.1): explicit backgrounding
  via `run_in_background` on bash, automatic migration of long foreground
  commands, file-backed spools with incremental cursor reads, a `process`
  tool (output/status/kill/list), wake-on-completion over the durable
  injected-message path, follow-up integration, and the subscription seam
  a watch attaches to.
- **NP-002** — watches (`process/watch.rs` + `process/watch_exec.rs`):
  agent-authored deterministic filter scripts run incrementally against a
  process spool; matches wake the agent with the matching excerpt as an
  injected message. Zero model cost.

## Owner rulings binding this cluster

- The Claude Code Bash tool is the standard to meet (INTERNAL-AGENTS §3).
- **No timeouts, no turn limits, no caps** on process count or spool size:
  a background process lives until it exits or is killed.
- Boundary signals ride the durable injected-message path
  (`MessageRouter` + pending store), never the tool envelope
  (INTERNAL-AGENTS §2, message-injection ruling).
- The watcher *agent* (cheap-model layer) is out of scope here, but alert
  payload shapes must not preclude it.

## Ledgers

- `checklist.json` — C1..C27, rendered to `CHECKLIST.md`.
- `stories.json` — S1..S8 (the owner's own use cases: long test runs, dev
  servers, watching CI-style logs, background hygiene).
- Briefs under `briefs/` — NP-001, NP-002 (NP-002 depends on NP-001).

## Private spool boundary (2026-07-11)

Background-process spools retain arbitrary stdout/stderr and can therefore hold
credentials, private source, provider responses, or terminal control bytes. They
share the same confidentiality class as session JSONL and the uncapped
full-output session spool; a display-friendly `~/...` path is not evidence of
safe permissions.

The P0 policy requires private Unix directory/file modes (`0700`/`0600`), a
regular final target opened without following symlinks, and failure on a link or
non-regular target. The same rules apply on initial create and every reopen or
resume path, including parents created under a permissive umask. Session index,
lock, atomic-temporary, and full-output spool artifacts follow the same private
artifact policy in the session persistence subsystem.

The owner-ratified absence of process-count/spool-size caps remains unchanged.
Confidentiality hardening must not invent a size cap or timeout; resource policy
is a separate owner decision.
