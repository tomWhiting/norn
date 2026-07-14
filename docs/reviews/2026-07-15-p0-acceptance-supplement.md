# P0 acceptance evidence supplement

- **Date (Australia/Melbourne):** 2026-07-15
- **Accepted source head:** `e1bf7f2`
- **Packaging parent:** `7ce29d7`
- **Source range:** `41ea210..e1bf7f2`
- **Purpose:** make the exhaustive manual-review claims explicit after the
  accepted Gate D report recorded them only indirectly
- **Result:** no production finding; the `READY` verdict remains supported

Three fresh read-only seats independently covered the manual Rust-policy,
writer-family, and sensitive fixture/evidence requirements. They ran no Cargo
command and changed no file. Disposable policy output stayed under the
repository's ignored `target/` tree. A separate read-only closure auditor then
checked this reconciliation and the exact Cargo-TOML denominator.

## Rust LOC, shape, and bypass review

The accepted policy JSON and Git each enumerate the same 359 unique changed
Rust paths: 128 added and 231 modified. The sorted path-set SHA-256 is
`836ada0eeb59f09da70887f23e5be5813506e591a3db0038682410c511695771`,
with no set difference and no Rust drift after `e1bf7f2`.

The seat inspected all 359 records and independently checked physical-line
counts, removed-range ordering/bounds/UTF-8 boundaries, whole-file versus
partial test stripping, test-only reachability, production LOC, module shape,
entrypoint shape, and every prohibited added-line category:

- 175,080 physical lines and 52,312 production lines;
- 65/65 test-only records independently reconstructed, with no missing or
  extra path;
- maximum production LOC 497 and zero production files over 500;
- 29 changed production `mod.rs` files, zero shape violations;
- three changed entrypoints at 42, 25, and 8 production LOC, all below 200;
- all 37,261 added Rust lines inspected by the Rust-policy seat;
- all 12 added nonblank Cargo-TOML lines inspected by the closure auditor, with
  no prohibited bypass; and
- zero added unwrap/expect/panic/todo/unimplemented calls, lint suppressions,
  ignored tests, empty cfgs, or unresolved debt markers.

The three raw `.unwrap(` matches were embedded JXA/test strings. One added line
overlapped a pre-existing multiline `#[allow]`, but the semantic diff removed
three allowances and only removed punctuation from the retained entry. The one
`TODO` match was an intentional convention-test fixture.

A fresh policy generation was byte-identical to the retained artifact,
SHA-256
`e61acb565377989891250329f6e10f9187fe46f36294f2db65d715d2680e7abe`.

## Writer-family reconciliation

All 97 policy-selected rows are unique, still match current source, and are
classified exactly once across 27 files:

| Classification | Rows |
|---|---:|
| Implicit private artifacts | 67 |
| Shared Codex OAuth store, owned by P2 | 6 |
| Explicit user/operator-directed writers | 19 |
| Build and diagnostic writers | 3 |
| Read-only or semantic false positives | 2 |
| **Total** | **97** |

The canonical row-list SHA-256 is
`3d9bbeea6193e2dbc08007f64f1810406d997b7f652d6d0d113e71f3082c76ba`.
The `bfa0b8e` and accepted `e1bf7f2` row sets are identical; none of their 27
source files changed in that correction range, and no Rust source changed after
`e1bf7f2`. A broader production AST sweep found no unowned writer family.

The P0 lexical method is deliberately narrowed in the inventory record: its 97
rows are roots/opener/family seeds, not every downstream mutation statement.
Additional `set_permissions`, handle `write_all`/`sync_all`, and Rustix
filesystem operations all resolve to already-owned families. P1 owns turning
that broader method into one checked-in reproducible policy. The inventory's
`doctor` prose is also corrected: scratch creation failure is reported, while
cleanup failure is intentionally ignored.

## Sensitive fixture and evidence review

The live candidate contains 84 files beneath `docs/reviews/evidence`: 43 JSON,
26 Python, 6 shell, 8 Rust fixtures, and 1 YAML. The seat traversed every scalar
and key in all 43 JSON files: 11,301 string occurrences, 1,152 unique string
values, 36,609 key occurrences, and 177 unique keys.

The JSON corpus contains zero known credential prefixes or JWTs, emails, UUIDs,
real account/organization identifiers, prompt/turn/conversation/cache values,
URLs, captured secret-variable values, and actual local paths. Its
1,892 full-hex values are 37 commit IDs and 1,855 explicit SHA-256/artifact
fields, not raw cache keys. The only environment inventory is the fixed 15-name
sterile allowlist repeated across the three schema-v3 artifacts; removed
ambient variables are retained only as numeric counts.

The seat also enumerated 6,042 added Rust string literals across the 359 changed
files. Every credential-, identity-, prompt-, cache-, path-, email-, and
high-entropy candidate is a structural label, error string, or explicit
synthetic sentinel. Intentional evidence-tool path fixtures cover Unix,
Windows, UNC, and file-URI rejection; fixed runner executable names are
checked-in inputs rather than captured machine paths.

The six owner-removed schema-v2 artifacts remain only in approved historical
Git commits. They contain 92 local-path occurrences plus six occurrences each
of the names `CODEX_THREAD_ID` and `PERPLEXITY_API_KEY`, but no corresponding
values or credential-value candidate. Current evidence artifacts retain none of
those historical path strings, variable names, or values. Current narrative
records retain the artifact filenames, package commits, SHA-256 values, and the
two variable names solely to disclose what was removed; no value is retained.

The audit found generic native-interpreter paths in two reviewer batteries and
the accepted review. The live candidate replaces those paths with a path-free
toolchain description. It also corrects two editorial review labels: the
failure/TUI cluster is GD-12, the `claude_runner` pin is GD-14, GD-13 is the
admission-record ordering correction, and GD-16 remains redirect refusal.

## Reproduction record

The seats used complete Git path/diff inventories with external diff disabled,
the checked-in policy generator, canonical JSON row hashing, independent
per-record invariants, rebuilt module reachability, AST module/literal scans,
and raw plus token-aware added-line scans. The writer seat additionally swept
standard/Tokio filesystem operations, handle mutations, and Rustix filesystem
calls. The sensitive seat recursively parsed every retained JSON scalar and
verified the six removed paths are absent from current `HEAD` but retrievable
from their recorded commits.

This supplement makes the manual coverage explicit; it does not replace or
expand the final Gate D verdict in
[`2026-07-15-p0-correction-gate-d-review.md`](2026-07-15-p0-correction-gate-d-review.md).
