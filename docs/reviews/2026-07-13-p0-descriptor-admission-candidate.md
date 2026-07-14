# P0 D1E weighted descriptor-admission candidate

## Status

**Ready for independent review; not accepted.** This record covers the
weighted-admission half of D1E at the current candidate. D1E and whole-phase P0
remain open until the inventory boundary, retained evidence, and implementation
pass independent review. D1D and the final P0 gates are separate open work.

The external D1C/D1E review covered only the earlier range through `09b9d49`.
Its F-1 through F-3 actions are implemented through `d488c1a` and recorded in
the [correction record](2026-07-13-p0-d1c-d1e-correction.md). Review of the
integrated candidate should therefore use `ca43c1b..d488c1a`; the older
`ACCEPTED` verdict is not attributed to the later weighted-admission changes.

The candidate does not claim to prevent system-wide `ENFILE`, exhaustion caused
by unrelated embedder code, or every short one-shot filesystem syscall. The
enumerated active/scalable families below use the authority. Ordinary serialized
configuration reads and user-directed write/edit/patch operations retain the
explicit eight-descriptor emergency reserve; that residual boundary is called
out for reviewer ruling rather than described as complete coverage.

## Authority

`DescriptorGovernor` is one process-wide, success-only lazy authority. A failed
first observation is retryable rather than cached forever. Finite capacity is
derived from the live soft limit minus observed use and emergency headroom; an
explicit OS infinity sentinel is distinguished from unavailable limits. Every
production admission is fail-fast. The only waiting API is compiled under
`cfg(test)`.

Each acquisition snapshots the live limits and open count before reserving its
weight. The reservation decision then accounts for that snapshot and the
already-reserved governed weight. This deliberately double-counts
already-realized governed descriptors:
it trades capacity for safety when the limit is lowered or foreign descriptors
appear after initialization. Errors retain the requested weight, capacity,
snapshot, and `norn doctor` guidance through provider, process, tool, session,
diagnostic, and LSP boundaries.

Source-derived weights:

| Family | Peak | Retained |
|---|---:|---:|
| One output pipe, null stdin, inherited stderr | 5 | operation lifetime |
| Two output pipes, null stdin | 7 | stdout + stderr + active spool = 3 |
| Three piped standard streams | 8 | stdin + stdout + stderr = 3 |
| Active HTTP request | 3 | response/body/stream lifetime |
| Private filesystem transaction | 5 | operation or returned file/lock lifetime |
| Serial `.gitignore`-aware recursive walk | 11 | walk lifetime |
| OAuth callback listener plus accepted socket | 2 | accepted socket through final browser response |
| Diagnostic Unix socket | 1 | query lifetime |
| Debug append | 3 | append transaction |

The spawn peaks count both ends of every stdio pipe before fork and both ends of
Rust's close-on-exec status/error pipe. The private-filesystem peak counts root,
two simultaneously held traversal directories, source, and no-replace
destination. The recursive-walk peak counts `walkdir`'s source-defined maximum
of ten simultaneously open directories plus one ignore-file handle.

## Governed inventory

- Managed and foreground shell launch, foreground-to-background adoption,
  process-spool creation/seeding/appender ownership, watch filters, shell hooks,
  prompt/rule/variable/skill/Rhai commands, Claude subprocesses, and diagnostic
  subprocesses reserve their full launch peaks before spawn.
- Cancellation-sensitive spool operations move the complete composite permit
  into each `spawn_blocking` closure and return it before splitting steady
  stdout/stderr/spool ownership. `ProcessHandoff` aborts drains and kills an
  unadopted process on every cancellation/failure edge.
- MCP stdio transports reserve the three-pipe launch peak and retain stdin,
  stdout, and stderr (3); the stderr reader owns its split permit until drain
  completion. Extension stdio transports reserve the two-pipe launch peak and
  retain stdin and stdout (2) while attaching stderr to null. HTTP
  MCP/extensions, OpenAI/compatible provider requests, OAuth
  exchange/refresh/revoke, web fetch, and web search retain an HTTP permit
  through body or stream completion. Relevant clients disable idle pooling.
- The OAuth callback server admits its listener and one accepted connection
  before bind, accepts only one socket at a time, bounds headers and read time,
  drops the listener before token exchange, and delays browser success until
  exchange and credential persistence succeed.
- Session files, index locks/transactions, event readers, session artifacts,
  action-log and mutation-ledger observations, branch/delete operations, task
  spools, process-spool lazy reads/writes, model read streaming, search walks,
  Rhai file I/O, and debug appends admit before opening. Detached blocking work
  owns its permit until the closure ends.
- Chiron serializes same-key starts and uses a host lease for every physical LSP
  spawn. Norn reserves W8, Chiron synchronously settles it to W3 only after a
  successful `Command::spawn`, and deterministic teardown releases W3 before
  restart or shutdown. Production Norn rejects an unmanaged raw LSP workspace.

## Upstream Chiron evidence

The published branch `fix/lsp-descriptor-admission` ends at
`25161bc8f93484b34291184e49dc3dfdda957760`. Norn pins the `diagnostics`, `lsp`,
and `syntax` workspace dependencies to that exact revision.

At that revision:

- strict LSP all-target Clippy passes;
- the full LSP suite passes (435 unit tests, two crash E2E tests, seven
  admission/start E2E tests, and two doctests; ten pre-existing binary-dependent
  tests remain ignored);
- same-key 32-caller convergence, distinct-key exact-W8 contention, and
  exact-W8 crash/restart each pass 20/20.

The separate `libyggd` dependency still brings its own unpinned `syntax` source;
this record claims only Norn's three workspace-root Chiron dependencies.

## Norn evidence

- `cargo clippy -p norn -p norn-cli --all-targets --all-features -- -D warnings`:
  pass.
- `cargo test -p norn --lib`: 3176 passed, zero failed, zero ignored.
- `cargo test -p norn-cli --lib`: 444 passed, zero failed, zero ignored.
- OAuth callback tests: 9 passed.
- `docs/reviews/evidence/run_descriptor_retention_evidence.sh 20`: each of
  low-limit retention/saturation, foreground cancellation, and timeout migration
  passed 20/20; raw counts are retained in
  `evidence/2026-07-13-descriptor-admission.json`.
- The retained syntax-aware policy audit for committed range
  `ca43c1b..6aa7185` covers 65 changed Rust files: zero production files over
  500 lines (largest 487), zero thin-entrypoint violations, and zero added
  unwrap/expect/panic/todo/unimplemented/suppression/ignored-test/marker
  matches. Raw rows are in `evidence/2026-07-13-d1e-policy.json`.
- The review-correction runner passes 20/20 for both configured lock-deadline
  reporting and first-request full replay after interrupted-tool repair. Its
  final-head policy report for `aa6a653..d488c1a` has zero prohibited matches
  and zero production files over 500 lines; see the correction record above.

## Review questions

1. Does live revalidation plus conservative double-counting close limit-lowering
   and delayed-cleanup races without introducing an unacceptable false-refusal
   profile?
2. Are every cancellation-sensitive blocking open and every persistent owner
   paired with the permit lifetime claimed above?
3. Is the explicitly excluded one-shot filesystem boundary acceptable under
   D1E, or must write/edit/patch and startup/configuration reads also enter the
   weighted authority before D1E can close?
4. Does the W8 to W3 Chiron contract cover initial start, duplicate start,
   initialization failure, crash, restart, and shutdown without an unmanaged
   production construction path?
