# Native read-aloud

This describes the D08 implementation branch. Installation and live audio
verification are pending; these commands are not yet in installed preview.8.

Norn's terminal connects directly to Locutus's Unix control socket. It does
not ask the model to call `say`, load audio models, or start a voice service.
With a compatible Locutus service already running, supply its actual socket:

```text
/voice configure {"enabled":true,"control_socket":"/absolute/path/locutus.sock","automatic":false,"voice":null}
/voice read
/voice stop
/voice replay
```

`read` selects the latest completed textual answer from this live session.
`replay` repeats the last selected answer as a new speech request. Neither
reads raw structured JSON or reasoning. Source changes invalidate earlier
selections; loading history never automatically speaks it.

`stop` targets this terminal's request, including speech that has not started.
The UI distinguishes requesting a stop from receiving confirmation. Agent
generation and tools continue. `off` stops speech and disables new requests.

```text
/voice on
/voice off
/voice auto on
/voice auto off
/voice status
/voice help
```

Automatic read-aloud is off by default. It reads newly completed textual
answers when enabled. Only one speech operation is active. If another answer
finishes while speech is busy, the UI reports that automatic read-aloud did
not start; the latest answer remains available with `read`. There is no hidden
playback queue or automatic retry after a connection failure.

Locutus's admission policy still applies. Speech may wait until you select
Play in Dot. Accepted speech is not reported as playing until the service
sends its `playing` event.

## Preferences and shortcuts

Option+Shift+V reads; Option+Shift+X stops. Both use editable bindings:

```text
/view keys set voice_read alt+shift+v
/view keys set voice_stop alt+shift+x
```

Settings live beside the other frontend preferences:

```json
{
  "tui": {
    "voice": {
      "enabled": false,
      "automatic": false,
      "control_socket": "/absolute/path/locutus.sock",
      "voice": null
    }
  }
}
```

`voice: null` uses the registered seat's voice. Supply a Locutus voice name
to override it. Enabling requires an absolute socket path. Unknown keys and
malformed values are rejected. `configure` replaces the whole voice object;
omitted boolean choices return to disabled/manual defaults.

Use `/view preferences run` for temporary changes, `user` or `local` for the
existing save scopes, and `save` for an explicit save. Higher settings layers
still win on restart. Disabled native voice opens no connection and creates
no task. Separately configured MCP servers keep their own launch policy.

## Outcomes and verification

Service refusals and socket failures appear in the conversation. Losing the
connection after submission leaves an unknown playback outcome; it does not
prove the sound stopped. Stopped receipts preserve the output callback's
position and device latency, rather than claiming perfect knowledge of which
words reached your ears.

Dictation, streamed spoken/written sections and model-visible interruption
receipts are subsequent deliveries. The pinned contract is `locutus-contract`
at `56b6722d43380e6a5e59fe3f759bb907cbb17c87`. Live service testing, review and
the exact-commit 205 battery remain required before landing and installation.
