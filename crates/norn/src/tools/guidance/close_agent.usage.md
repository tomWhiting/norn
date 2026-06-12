Use to shut down a child agent — for example, a long-lived worker that should stop now rather than at its natural end. Address the target by hierarchical registry path or UUID. The close walks the target's subtree depth-first (children before parents) and reports a per-agent status list in `shut_down`. When in doubt, check the target's status with the agents tool first — a child that already finished needs no close.

For children whose handle you hold (agents you spawned or forked yourself), the close delivers your `reason` as a final Steer message, cancels the child's run, and waits for the child to record its own outcome. A run stopped mid-flight records `failed` with stop reason `cancelled` — an in-flight model call is interrupted immediately, while a tool that is already executing finishes first. A forced stop is never recorded as a completion, and an outcome the child already recorded is never rewritten.

Each `shut_down` entry carries one of these statuses:

- `reclaimed` — the child's run recorded a terminal outcome (its natural one, or the cancelled outcome after this close stopped it mid-run); the close removed the registry entry, preserving the recorded outcome in the agent's completion record.
- `already_completed` — the agent had already finished and been reclaimed before the close reached it; nothing was done.
- `force_failed` — the child's task ended without recording any outcome, so the close recorded `failed` (outcome unknown — never `completed`) and removed the entry.
- `unreachable` — a live agent whose handle you do not hold; it cannot be force-stopped from here and its registry entry is left untouched. Route the close through its parent instead.
- `failed` — the close could not record the forced shutdown in the registry.
- `missing` — the agent vanished without any completion record (an internal invariant violation, reported rather than retried).

Targeting an agent that already finished and was reclaimed is a soft success: the result carries `already_completed: true` with the recorded status and completion time, and there is nothing further to do — do not retry. A target that finished but was not yet reclaimed does not carry `already_completed`: it is reported `reclaimed` in `shut_down`, with its recorded outcome preserved.
