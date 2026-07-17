# D2 F1 correction handoff

**Status:** Narrow correction candidate for the same Gate D coordinator. The
original D2 review returned `READY` contingent on F1. F1 is fixed and
re-evidenced, but D2 is not accepted until that reviewer confirms this
correction.

**Original reviewed range:** `2c0350d..3ebc468`; candidate tree
`b95ab9d411271769c7c0e6a305d0d4a21152b2b2`.

**Gate D review:** `59dc244`,
[`2026-07-17-d2-gate-d-review.md`](2026-07-17-d2-gate-d-review.md).

**Correction commit:**
`e9755fe2533979410f53eda88966349437a54517`; tree
`9663e625dc57c7e23e87e3caf4f11b3adee8e231`.

**Narrow correction diff:** `59dc244..e9755fe`; one production file,
`session/migration/transaction.rs`, with seven inserted lines.

## Correction

F1 identified one missing durability edge in the backup-publication
transaction. `copy_tree` made the copied files and the `sessions/` directory
durable, but the containing `.session-backup-stage/` directory was not synced
after its `sessions` child was created.

The correction adds exactly one operation in
`session/migration/transaction.rs`, immediately after `copy_tree` and before
staged verification or any publication checkpoint:

```rust
root.sync_dir(&backup_stage).map_err(|error| {
    SessionMigrationError::mutation(
        "synchronizing backup migration stage",
        &backup_stage,
        error,
    )
})?;
```

The resulting order is:

1. sync copied files and their descendant directories;
2. sync the copied `sessions/` root;
3. sync `.session-backup-stage/` so its `sessions` dirent is durable;
4. verify and classify the staged backup, construct and validate the strict
   stage, and re-digest the live source;
5. record `BackupPrepared` and publish the backup with no replacement;
6. record `BackupPublished`, sync the backup namespace, and record
   `BackupDurable`.

The new failure remains typed as a migration mutation failure and occurs before
publication. No other production source changed between the reviewed candidate
and the correction.

An ordinary process test cannot prove a power-loss `fsync` guarantee. The
mechanism is therefore evidenced by the exact ordered production call, the
existing abrupt-process `BackupPrepared` recovery case, and a complete
exact-head gate. No test-only filesystem abstraction or synthetic sync hook was
added solely to observe the call.

## Exact-head evidence

The correction gate ran from a detached checkout of `e9755fe`, using the main
repository's normal Cargo target through `target/shared`. Its source checkout
was clean at both recorded evidence snapshots, before the runner published its
own output JSON. The implementer reports that loopback-only tests ran outside
the managed network sandbox after an in-sandbox `EPERM`; no external network
was used.

- 10/10 gate legs passed.
- Workspace all-target tests: 5,281/5,281.
- Doctests: 8/8.
- Focused persistence, migration, manager, CLI session, and config-path suites:
  165/165, 29/29, 37/37, 51/51, 14/14, and 11/11.
- All 14 isolated concurrency/recovery cases ran 20 times: 280/280, including
  `backup_prepared`, `backup_published`, and `backup_durable` at 20/20 each.
- The complete base diff contains 164 paths and 7,654 raw NUL-delimited bytes,
  SHA-256
  `68462c3e7d5359f49a819becf4a44ccf1e8c84044292ab09fa497e708a2556dc`.
- The runner SHA-256 is
  `756000cf20fe7f6392d4434a369ae765e5bb07b970c311cf2472d45563d23601`.
- The policy scan covers 145 changed Rust files, including 33 test-only files,
  with zero production files at or above 500 lines and zero added bypass, panic,
  unwrap, expect, ignored-test, debt-marker, thin-entrypoint, or module-shape
  violations.

**Correction gate artifact:**
[`evidence/d2/2026-07-17-d2-f1-correction-gate-e9755fe.json`](evidence/d2/2026-07-17-d2-f1-correction-gate-e9755fe.json),
SHA-256
`8cb4caf0b5724bf49de4fe53e9b7a4c4296751d2fb09a73fdb1e809aacd9e077`.

**Correction policy artifact:**
[`evidence/d2/2026-07-17-d2-f1-correction-policy-e9755fe.json`](evidence/d2/2026-07-17-d2-f1-correction-policy-e9755fe.json),
SHA-256
`c8565b5cf75f7ae88ae94b862ab6e0a6572e988eb333b7c74d27191e9cc6a024`.

## Narrow review request

The same Gate D coordinator should:

- confirm that `59dc244..e9755fe` changes only the intended production file;
- verify that the new stage-root sync is after population and before
  verification, checkpoints, or publication;
- verify that `PrivateRoot::sync_dir` synchronizes the target directory
  descriptor;
- verify the typed error mapping and that no other production source changed;
- reproduce the correction artifact hashes and exact commit/tree binding;
- independently rerun the focused migration suite, strict Clippy, policy scan,
  and the retained `BackupPrepared`, `BackupPublished`, and `BackupDurable`
  cases 20 times each; and
- return `F1 CLOSED; prior contingency satisfied; corrected D2 Gate D READY`,
  or identify a remaining defect.

The correction verdict must be committed as a durable review record containing
the reviewer and date, exact correction commit and tree, commands and result
counts, reproduced artifact hashes, policy result, and final verdict.

This is not a request to repeat the four-seat whole-D2 review. The eight
non-blocking observations in the original Gate D review remain the P3/P4 ledger,
especially global `index.lock` occupancy and quadratic full-timeline rescanning
under repeated appends.

This handoff does not claim D2 acceptance, response-scoped audio, the exhaustive
lifecycle matrix, final P3/P4 Gate C, or P3/P4 acceptance.
