# P0 macOS descriptor-relative create correction

**Date:** 2026-07-12

**Correction commit:** `c25e8411cf3e55c3a31fce61bf7f9b2480a07159`

**Platform:** macOS 26.3.1, Darwin 25.3, arm64, APFS

**Disposition:** The Gate D F1 implementation and targeted repeatability
evidence are complete pending independent re-review. Integrated P0 Gate C
remains open until the other corrective items land and the complete suite runs.

## Corrected claim

The original handoff recorded one successful workspace suite as an unqualified
pass. Gate D subsequently reproduced
`open_or_resume_concurrent_same_id_converges_on_one_session` failing 6/10 times;
a separate independent run failed 19/20 times. The original command result was a
truthful observation but insufficient stability evidence.

The retained standalone reproducer independently opens one parent directory
descriptor per worker before a barrier, then performs the same-name
`openat(O_WRONLY | O_CREAT | O_NOFOLLOW | O_NONBLOCK)` topology used by Norn.
The affected platform returned `ENOENT` for 174/400 thread attempts. Controls
using an absolute path, distinct names, or a pre-existing target each succeeded
400/400 times.

One retry after only macOS `ENOENT + O_CREAT` succeeded 400/400 times and used
189 retries. This is the smallest bound supported by retained evidence. The
production correction is macOS-only; non-create operations, other errors, and
other platforms preserve their prior single-call behavior. Extending the bound
requires new evidence.

The `O_CREAT | O_EXCL` control produced exactly one success and three expected
`EEXIST` outcomes in each of 100 trials: 100 winners, 300 losers, zero
`ENOENT`. The Rust regression independently pins exactly-one-winner behavior.

## Raw reproduction evidence

All files contain the full denominator, platform, Python version, flags,
outcome distribution, errno distribution, retry count, and unexpected worker
errors:

- [`2026-07-12-openat-baseline.json`](evidence/2026-07-12-openat-baseline.json)
- [`2026-07-12-openat-one-retry.json`](evidence/2026-07-12-openat-one-retry.json)
- [`2026-07-12-openat-absolute-control.json`](evidence/2026-07-12-openat-absolute-control.json)
- [`2026-07-12-openat-different-names-control.json`](evidence/2026-07-12-openat-different-names-control.json)
- [`2026-07-12-openat-existing-control.json`](evidence/2026-07-12-openat-existing-control.json)
- [`2026-07-12-openat-exclusive-control.json`](evidence/2026-07-12-openat-exclusive-control.json)

The checked-in generator is
[`openat_same_name_create_repro.py`](evidence/openat_same_name_create_repro.py).
Representative commands:

```text
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100 --enoent-retries 1
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100 --absolute
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100 --different-names
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100 --precreate
python3 docs/reviews/evidence/openat_same_name_create_repro.py --trials 100 --exclusive --enoent-retries 1
```

## Rust repeatability evidence

The checked-in runner invoked each regression in a fresh test process 50 times:

| Regression | Pass | Fail |
|---|---:|---:|
| Session same-ID convergence | 50 | 0 |
| Independently opened roots sharing one lock name | 50 | 0 |
| Concurrent `create_new` exactly-one-winner | 50 | 0 |

Raw per-invocation exit codes and durations are in
[`2026-07-12-p0-concurrency-c25e841.json`](evidence/2026-07-12-p0-concurrency-c25e841.json).
The record identifies the exact commit and truthfully reports the unrelated
pre-existing `.claude/skills/norn/SKILL.md` worktree modification. The runner is
[`run_p0_concurrency_evidence.py`](evidence/run_p0_concurrency_evidence.py):

```text
python3 docs/reviews/evidence/run_p0_concurrency_evidence.py --runs 50
```

## Targeted verification

```text
cargo test -p norn util::private_fs -- --nocapture
# 11 passed; 0 failed

cargo clippy -p norn --all-targets -- -D warnings
# pass; no warnings or allowances

tokei --output json crates/norn/src/util/private_fs.rs \
  crates/norn/src/util/private_fs_tests.rs
# private_fs.rs: 412 code lines
# private_fs_tests.rs: 418 code lines
```

No lint suppression, ignored test, panic-style helper, absolute-path fallback,
or cross-platform retry was added.
