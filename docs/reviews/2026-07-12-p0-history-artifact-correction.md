# P0 TUI-history artifact correction

**Date:** 2026-07-12
**Implementation commit:** `2c6b50c`
**Companion boundary commit:** `37c806a`
**Status:** implementation complete; whole-P0 Gate C and Gate D remain open

## Finding

The final repository-wide artifact sweep found that
`norn-tui/src/input/history.rs` persisted prior user prompts through ordinary
`create_dir_all` and `OpenOptions` calls. It bypassed the reviewed private
filesystem primitive, ignored absolute `NORN_HOME`, followed links under
ordinary platform semantics, inherited ambient modes, and derived `Debug` over
the complete history and current draft. This was a real omitted SEC-15 artifact
family, not a lexical false positive.

## Correction

- `norn::resource::PrivateLineLog` is a narrow public adapter over the
  crate-private `PrivateRoot`; the general descriptor capability remains
  unexported.
- The adapter accepts only an absolute path with a validated UTF-8 final
  component. It pins/creates the parent privately, opens the data and sibling
  lock files descriptor-relatively, rejects links/non-regular files, and heals
  directories/files to `0700`/`0600` on the supported Unix target class.
- A private advisory lock covers every read and append, including independent
  TUI processes. Append uses one encoded physical record and repairs an
  unterminated tail under that lock before adding the next line. Reads omit an
  unterminated final fragment.
- Invalid UTF-8 or another non-`NotFound` read failure disables disk backing
  after a warning. The TUI does not keep extending a file it cannot safely
  interpret.
- The production driver resolves history through
  `norn::config::paths::norn_dir()`, so an absolute `NORN_HOME` is honored and a
  relative override cannot make the working directory authoritative.
- `InputHistory` and `InputEditor` now implement structural `Debug` views that
  expose counts, cursor state, and presence only; historical prompts, drafts,
  and the current editor buffer are omitted.
- The touched history tests no longer use their inherited
  `#[allow(clippy::unwrap_used)]`; fallible tests propagate `Result` instead.

## Focused evidence

All commands ran with `CARGO_INCREMENTAL=0` after the host filesystem reached
capacity while Rust attempted to create an incremental query cache. No partial
or `ENOSPC`-terminated invocation is counted below.

| Command | Result |
|---|---|
| `cargo test -p norn resource::private_line_log --lib` | Pass: 8/8. Covers relative-path rejection, private creation, existing-mode healing, read/write final-symlink refusal, newline rejection, torn-tail repair, and 400-record concurrent writers. |
| `cargo test -p norn-tui input::history --lib` | Pass: 13/13. Covers persistence, missing/corrupt backing behavior, navigation, and history/draft Debug non-disclosure. |
| `cargo test -p norn-tui input::editor::tests::debug_omits_live_editor_text --lib` | Pass: 1/1. |
| `cargo check -p norn-cli` | Pass; proves the real TUI driver uses the borrowed hardened history path. |
| `cargo test -p norn --doc` | Pass: four runnable and four compile-fail doctests. |
| `cargo test -p norn --doc --features test-utils` | Pass: same four runnable and four compile-fail doctests, including both SEC-05 constructor-boundary fixtures. |
| `cargo clippy -p norn -p norn-tui -p norn-cli --all-targets -- -D warnings` | Pass with no warning. |

The independent design review recommended the narrow adapter and identified
the missing lock, torn-tail policy, corruption policy, final-name validation,
and Debug disclosure before packaging. Each was implemented before the commit.

## Evidence limits

- `PrivateLineLog` promises locked complete-record semantics among cooperating
  writers. It does not claim durability after every key submission or safety
  against a same-UID process that deliberately ignores the lock and mutates the
  pinned inode.
- Generic `PrivateRoot` tests remain the evidence for ancestor replacement
  confinement. The wrapper adds no alternate path traversal.
- The config-path suite is the authority for `NORN_HOME` absolute/relative
  behavior; the production history function calls that tested resolver
  directly rather than duplicating environment policy.
- This correction closes one P0 artifact omission. The complete coverage claim
  still depends on the retained raw writer enumeration, D1D resolution, final
  Gate C, and fresh whole-P0 review.
