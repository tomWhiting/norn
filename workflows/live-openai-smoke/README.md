# Explicit live OpenAI acceptance

The normal Norn release battery exercises local behavior and compiles/lints this
live test. It does **not** prove a live OpenAI request succeeded. This optional
lane preserves the preexisting `gpt-4.1-mini` hello smoke and produces its own
exact-commit receipt when explicitly dispatched. No live execution receipt exists
merely because the release battery is green.

The Rust target `live_openai_smoke` requires the `live-api-smoke` Cargo feature.
The release declaration includes strict Clippy and `cargo test --no-run` legs for
that exact target. Missing Python or permission prerequisites in the normal tests
now return `NORN_TEST_PREREQUISITE_UNMET` errors through the Rust test harness;
they cannot become passing tests because a tracing subscriber was absent.

The live worker runs on venue205, with its existing `/home/aion` home. Its host
process must receive `OPENAI_TEST_KEY` and a positive, operator-assigned
`REPO_BATTERY_LEG_JOBS`. No value belongs in an AWL file, workflow input, command
argument or log. Start it through the operator's existing credential provisioning
mechanism; this repository does not invent a credential file or value.

The runner uses the same named environment carriage as Aion's
`workflows/repo-battery/bin/lib.sh::worker_env_value`: its own environment first,
then the immediate worker parent's `/proc/<pid>/environ`. That explicit Linux
seam is necessary because AWL command children inherit only PATH and literal
exports. Only the two declared prerequisite names are admitted. A missing or
unreadable value refuses before Cargo or network execution. The venue cap becomes
the existing Cargo/nextest/libtest thread variables; there is no new default.

Check both documents with `aion awl check` and inspect the worker advertisement
with `aion worker awl workflows/live-openai-smoke/worker.awl --check` before serving
it. Deploy `workflow.awl` through the existing Aion operator surface. Serve
`worker.awl` with the venue's explicitly chosen identity, concurrency and reconnect
settings and `HOSTNAME=venue205`. The AWL-worker CLI currently registers in the
default namespace; a global namespace flag is not an isolation mechanism here.

Start workflow `norn_live_openai_smoke` with exactly these non-secret input fields:

```json
{"repo_path":"<absolute reviewed clean checkout on205>","expected_head":"<full reviewed commit>"}
```

The runner refuses a mismatched HEAD. The existing source guard verifies tracked
blobs, modes, untracked cleanliness and the same HEAD before and after the exact
live test command. The receipt requires both matching source witnesses and
exactly one passing test. TextDelta and Done assertions remain in that test.
Raw test/provider output is captured and never copied into the workflow receipt;
failures name the target and phase without exposing credentials. Keep the receipt
with the workflow/worker document hashes and compare its source hashes against the
reviewed commit. A refused or red live receipt is never a live acceptance pass.

This is a source-guarded live smoke in an operator-provided checkout, not a clone
provisioner. The checkout must have no other writer during measurement. As with
the release guard, changes restored within a command are outside its before/after
guarantee. No Aion scripts, shared workers, credentials or global toolchain settings
are changed by authoring this lane.
