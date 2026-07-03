Use for commands that do not have a dedicated tool. Prefer dedicated tools over shell equivalents — use read instead of cat/bat/sed/head/tail, search instead of grep or find. Shell commands that print file contents are read-equivalent and their output is still budgeted before it reaches model context. Commands are risk-classified: destructive operations may be blocked or require confirmation. Use working_dir to execute in a specific directory without changing global state.

## Long-running commands: background and migration

Set `run_in_background: true` for anything you start and check on later — a dev server, a long build, a watched test run, a data import. The tool returns immediately with a process id (e.g. `p1`) and a spool path; the command keeps running detached, with **no timeout**, until it exits or you kill it. Do not sit in a foreground bash call waiting for that kind of work — background it and keep going.

`timeout` bounds only the foreground wait, and reaching it **no longer kills the command**. When a foreground command outruns its `timeout`, it is automatically **moved to the background** as a managed process instead — the result comes back with `migrated: true` and a process id, and nothing is lost: slow-but-healthy work keeps running. `timeout: 0` waits forever and never migrates. Raise `timeout` when you genuinely want to keep waiting; background it up front when you already know it is long.

Either way you are handed a process id. Use the `process` tool to work with it: `op=output` pulls the new output since your last check (call it repeatedly to follow along), `op=status` reports whether it is still running, and `op=kill` stops it (and its process group). When a background process finishes you are notified automatically with its exit status. Kill processes you no longer need.

`run_in_background: true` cannot be combined with `timeout` — a background process has no timeout, so setting both is rejected. Pick one: background it (no timeout), or run it in the foreground with a timeout (and let migration catch it if it runs long).
