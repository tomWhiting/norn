# TUI preferences

Saved frontend preferences are installed locally in **Norn 0.1.0-preview.4**. Start a fresh session to use them. This version includes the earlier layout, flicker and tool-description fixes. Main-branch venue verification and independent review remain pending; exact local evidence is recorded in [NFP-001](design/norn-frontend-preferences/briefs/NFP-001.md).

## Choose where changes are saved

Each CLI launch starts with **personal automatic saving**. This is the implementation's working assumption, chosen from the request that settings be remembered; it is not a quoted or confirmed user ruling. Opening Norn alone does not write preferences. Changing a saved view, display or input preference starts a save.

| Command | Effect |
| --- | --- |
| `/view preferences status` | Show active values, target, pending/failed/saved outcome and the captured winning settings layer. `/view preferences` also shows status. |
| `/view preferences run` | Keep subsequent preference changes temporary for this process. |
| `/view preferences user` | Save the current values to personal settings, then save further preference edits there automatically. |
| `/view preferences local` | Explicitly save the current values to this launch root's local settings, then save further edits there automatically. |
| `/view preferences save` | Request a save to the selected persistent scope. In `run` scope it asks you to choose `user` or `local`; while a save is pending it does not start a duplicate. |

The scope choice itself is **not saved**. On the next CLI launch, saving returns to `user`, even if workspace-local settings supply the initial displayed values. A write already accepted before switching to `run` is still observed to completion; later temporary edits are not saved by that transaction.

For example, `/view preferences local` followed by `/view pane open` and `/view split 2 1` saves those choices for the current launch root. Use `/view preferences status` to inspect the actual result. These are local frontend commands: they are not sent as model input and do not start/restart MCP servers or make provider requests.

## Files and precedence

Personal values live in `$NORN_HOME/settings.json`, normally `~/.norn/settings.json`. An explicit `NORN_HOME` must be absolute. Local values live in `<launch-root>/.norn/settings.local.json`; the target remains bound to the canonical directory from which this invocation was launched. The shared project file is `<launch-root>/.norn/settings.json`. These controls do not write the shared project file.

Precedence is **workspace-local → shared project → personal**. The highest present `tui` object wins as a whole; individual fields are not combined across layers. Missing fields inside that winning object use the declared defaults, not values from a lower layer. Even an empty higher-layer `tui` object shadows the lower one.

A successful personal save can therefore be **saved but shadowed on restart**. The current run keeps its selected values; on restart the higher layer wins again. Status reports the layer captured at launch or the last own publication, not a live watch of edits by other processes. Choose `local` explicitly when you want to update that launch root's higher-priority local object.

## JSON fields

This is the current default projection. Add or edit the `tui` member in the chosen settings document while preserving its other settings; do not replace the whole file with this example if it already contains other configuration.

```json
{
  "tui": {
    "view": {
      "changes_open": false,
      "split": { "conversation": 1, "changes": 1 },
      "upper_pane": "conversation",
      "expanded_tools": false,
      "history_events": 20,
      "body_bytes": 65536,
      "clipboard": "unspecified"
    },
    "display": {
      "thinking_visible": true,
      "secondary_fields_visible": false
    },
    "input": { "submit_mode": "steer" }
  }
}
```

- `split` stores positive integer weights from 1 to 65535, not a measured terminal width. `upper_pane` is `conversation` or `changes`.
- `history_events` and `body_bytes` are positive machine-sized integers controlling requested history/body loads. They are not retention or model limits.
- `clipboard` is `unspecified`, `disabled` or `osc52`. This records transport intent, not proof that the terminal accepts clipboard writes.
- `submit_mode` is `steer` or `queue` for input submitted during agent work. It does not configure Enter versus Alt-Enter.
- Boolean fields require JSON booleans. Fields may be omitted to use the declared defaults within the winning object.

The frontend owns only `tui.view`, `tui.display` and `tui.input`. Saves preserve unrelated document keys and unowned `tui` siblings such as `composer`. They do not save drafts, selections, viewport positions, transcript IDs, queued messages or terminal capability replies.

Malformed values and unknown fields inside an owned section are refused with the document and dotted field name, rather than silently replaced. Each loaded layer is validated, including a shadowed layer. Correct the named field and restart. Unknown top-level `tui` siblings remain available to their separate owners.

## Pending writes, conflicts and failures

Only one save runs at a time. Later edits remain active in the current view and unsaved until their own values are persisted. After a successful completion, the same owner saves the latest eligible state; an older completion is not reported as saving newer edits. An ordinary exit waits for accepted preference writes and reports failures.

The shared settings writer compares the three owned sections against the captured snapshot under the same document lock used by MCP settings writes. Unrelated changes are preserved. A concurrent change to an owned section is a named conflict and is not overwritten.

A failure before publication leaves the run values intact and stops automatic retries. Inspect `/view preferences status` and the reported file/error. After correcting a transient write problem, `/view preferences save` can retry; for an owned-section conflict, inspect the file and restart to capture its current values before reapplying desired changes.

“Published; durability uncertain” means the settings reached the document but durable directory sync was not confirmed. It is not a rollback. A save task ending without a known outcome also cannot be treated as a failed write: further saves are blocked until you inspect the settings and restart. Do not assume either case requires repeating an already-published write.

The [authoritative brief](design/norn-frontend-preferences/briefs/NFP-001.md) records acceptance and verification. The installed local preview does not constitute venue approval or a public release.
