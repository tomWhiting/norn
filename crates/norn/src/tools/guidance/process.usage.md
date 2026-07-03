Use this tool to work alongside long-running background processes — a dev server, a long test run, a watched build.

A process id (e.g. "p1") is returned when you start a command with bash `run_in_background: true`, or when a foreground bash command exceeds its timeout and is automatically moved to the background (the result carries `migrated: true` and the id).

- `output` (id): returns only the output produced since your last `output` call for that process, plus its current status. Call it repeatedly to follow a process incrementally. If there is no new output it says so — that is not an error. A very large new region is written to the process's spool file instead of being inlined; read or grep that file.
- `status` (id): the process's status (running / exited / killed), pid, start time, exit code when it has exited, and its spool path and size.
- `kill` (id): stop the process (and its process group, so a `server &` grandchild dies too). Killing an already-finished process just reports its final status — it is safe to call.
- `list`: every background process this session owns, with ids, commands, and statuses.

Background processes have no timeout and no turn limit — they live until they exit or you kill them. When one finishes you are notified automatically with its exit status. Kill processes you no longer need so nothing keeps running past its usefulness.
