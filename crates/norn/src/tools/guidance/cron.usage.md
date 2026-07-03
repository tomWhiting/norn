# cron — in-session schedules

## The four schedule kinds

Pass `op: "schedule"`, a required `message` (the text delivered to you when
the schedule fires), and **exactly one** of:

- `in` — relative one-shot. Duration grammar: a positive integer directly
  followed by one unit letter — `s` (seconds), `m` (minutes), `h` (hours),
  `d` (days). Examples: `"90s"`, `"15m"`, `"2h"`, `"3d"`. No spaces, no
  spelled-out units, no signs, no zero.
- `at` — time-of-day one-shot. `"HH:MM"` (24-hour) resolves to the next
  occurrence in the **host's local timezone**; an RFC 3339 timestamp (e.g.
  `"2026-07-04T09:00:00Z"`) names an exact instant.
- `every` — looping interval, same duration grammar as `in`. Re-arms after
  each fire from the fire time.
- `cron` — full 5-field cron expression (`min hour day month dow`),
  evaluated in **UTC**. Example: `"0 9 * * 1-5"` fires weekdays at 09:00
  UTC. Re-arms after each fire.

There is no cap on the number of schedules and no bound on intervals —
`every: "1s"` and `every: "365d"` are both valid.

## Delivery

A fired schedule injects a `<agent_message from="norn:cron" kind="steer">`
frame whose content is JSON: `{ schedule_id, kind, fired_at, late, message }`.
A steer wakes you at a lingering would-stop boundary; if you are an idle
spawned agent it is queued durably and a wake is requested; otherwise it is
queued for injection at your next step. `late: true` marks a one-shot whose
fire time passed while no process was live (delivered immediately on resume).

## Resume semantics

Schedules persist as session events. On session resume: pending one-shots
whose time passed fire immediately with `late: true`; recurring schedules
re-arm at their next natural fire after resume — missed occurrences are
never backfilled. Live timers always die with the process.

## Operations

- `{"op": "schedule", "in": "15m", "message": "check the build"}` →
  `{ id, kind, next_fire }`
- `{"op": "list"}` → every pending schedule (`id`, `kind`, `message`,
  `next_fire`, `late`), soonest first, no pagination.
- `{"op": "cancel", "id": "<schedule id>"}` → cancels a pending schedule;
  unknown or already-completed ids report not-found.
