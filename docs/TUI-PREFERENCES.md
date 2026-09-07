# TUI preferences

Updated 8 September 2026, Melbourne time, for **Norn 0.1.0-preview.8**. The retained TUI uses an Iridium composer, saved send-key choices, and editable view shortcuts. See [NUI-005](design/norn-retained-tui/briefs/NUI-005.md) for the current installation and verification limits; [NFP-001](design/norn-frontend-preferences/briefs/NFP-001.md) records the earlier preference implementation.

## Choose where changes are saved

Each CLI launch starts with **personal automatic saving**. Opening Norn alone does not write preferences. Changing a saved view, display, input or composer preference starts a save.

| Command | Effect |
| --- | --- |
| `/view composer send-key enter` | Enter sends; Alt+Enter inserts a newline. This is the default. |
| `/view composer send-key shift-enter` | Shift+Enter sends; Enter inserts a newline. Requires distinguishable modifier reporting. |
| `/view composer send-key alt-enter` | Alt+Enter sends; Enter inserts a newline. |
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

This example uses the declared defaults and omits optional shortcut overrides. Add or edit the `tui` member in the chosen settings document while preserving its other settings; do not replace the whole file with this example if it already contains other configuration.

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
    "input": { "submit_mode": "steer" },
    "composer": { "send_key": "enter" }
  }
}
```

- `split` stores positive integer weights from 1 to 65535, not a measured terminal width. `upper_pane` is `conversation` or `changes`.
- `history_events` and `body_bytes` are positive machine-sized integers controlling requested history/body loads. They are not retention or model limits.
- `clipboard` is `unspecified`, `disabled` or `osc52`. This records transport intent, not proof that the terminal accepts clipboard writes.
- `input.submit_mode` is `steer` or `queue` for input submitted during agent work. Ctrl+T changes this delivery choice.
- `composer.send_key` is `enter` (default), `shift-enter`, or `alt-enter`; it selects the physical send key independently of steer/queue. Change it with `/view composer send-key enter|shift-enter|alt-enter`, Option/Alt+S, or the last-row send-key control. A visible completion popup takes bare Enter/Tab first. In Shift+Enter or Alt+Enter mode, bare Enter inserts a newline. The terminal must distinguish the chosen modifier; the control reports unconfirmed modifier support where applicable. A setting cannot enable unsupported terminal reporting.
- Boolean fields require JSON booleans. Fields may be omitted to use the declared defaults within the winning object.

The frontend owns `tui.view`, `tui.display`, `tui.input` and `tui.composer`. `composer` is a strict object containing only `send_key`; unknown fields such as `composer.future` are refused. Saves preserve unrelated document keys and unowned `tui` siblings such as `extension_data`. They do not save drafts, selections, viewport positions, transcript IDs, queued messages or terminal capability replies.

Malformed values and unknown fields inside an owned section are refused with the document and dotted field name, rather than silently replaced. Each loaded layer is validated, including a shadowed layer. Correct the named field and restart. Unknown top-level `tui` siblings remain available to their separate owners.

## Editable view shortcuts

`/view keys` shows the active bindings. These frontend actions do not send a message to the model:

| Action | Default keys |
| --- | --- |
| `pane_toggle` | Option/Alt+P, F7 |
| `pane_diff` | Option/Alt+D, F8 |
| `pane_agents` | Option/Alt+A, F9 |
| `send_key_cycle` | Option/Alt+S, F10 |
| `upper_switch` | F2 |
| `search` | F3 |
| `copy` | F4 |
| `export` | F5 |
| `focus_next` / `focus_previous` | F6 / Shift+F6 |

For example, `/view keys set pane_toggle alt+q` replaces the toggle bindings with Option/Alt+Q. `/view keys set pane_toggle alt+p alt+q` assigns both; `/view keys clear pane_toggle` removes its shortcuts. The slash commands and clickable controls remain available. These edits use the same selected save scope as other frontend preferences.

The equivalent settings field is `tui.input.bindings`. This partial example overrides three actions; omitted actions retain their declared bindings. Add the member to the existing winning `tui.input` object:

```json
{
  "tui": {
    "input": {
      "bindings": {
        "pane_toggle": ["alt+q"],
        "pane_diff": ["alt+d"],
        "pane_agents": []
      }
    }
  }
}
```

Each action maps to an array of key strokes. An empty array explicitly unbinds it. Invalid, reserved, or conflicting shortcuts are refused before replacing the active bindings. Option-based bindings require the terminal to send Alt. Use `/view keys` to check the effective choices after loading settings.

## Pending writes, conflicts and failures

Only one save runs at a time. Later edits remain active in the current view and unsaved until their own values are persisted. After a successful completion, the same owner saves the latest eligible state; an older completion is not reported as saving newer edits. An ordinary exit waits for accepted preference writes and reports failures.

The shared settings writer compares the four owned sections against the captured snapshot under the same document lock used by MCP settings writes. Unrelated changes are preserved. A concurrent change to an owned section is a named conflict and is not overwritten.

A failure before publication leaves the run values intact and stops automatic retries. Inspect `/view preferences status` and the reported file/error. After correcting a transient write problem, `/view preferences save` can retry; for an owned-section conflict, inspect the file and restart to capture its current values before reapplying desired changes.

“Published; durability uncertain” means the settings reached the document but durable directory sync was not confirmed. It is not a rollback. A save task ending without a known outcome also cannot be treated as a failed write: further saves are blocked until you inspect the settings and restart. Do not assume either case requires repeating an already-published write.

The [preference brief](design/norn-frontend-preferences/briefs/NFP-001.md) records the existing save owner and verification; [NCP-001](design/norn-iridium-composer/briefs/NCP-001.md) adds the composer send-key preference. Composer integration and the three send-key policies are included in the current preview. The installed local preview does not constitute venue approval or a stable release.
