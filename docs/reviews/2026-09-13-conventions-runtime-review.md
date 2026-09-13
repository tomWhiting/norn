# CONVENTIONS runtime review — 13 September 2026, Melbourne

Requested by Tom after repeated `WARN · norn::tools::diagnostics_check::post_check` notices. This is a source review and repair plan, not an implementation or installed fix. Existing preview29 source changes are left intact. No builds, checks, service restarts, or configuration edits were run for this review.

## Conclusion

The declarative pattern/LOC engine, mutation integration, task-complete checks, stop hook, child-context forwarding, and committed-output reporting already exist. The most urgent work is making configuration admission and check outcomes truthful. Simply suppressing warnings or turning all rules into blockers would obscure missing enforcement or stop legitimate work.

The exact cause of the reported live warnings is not yet proven: their bodies were not available. Source inspection found multiple warning sites in `post_check.rs`, including unresolved languages, missing activated tools, workspace-path failures, and a poisoned modified-file accumulator. Earlier conversation narrowed too quickly to workspace paths.

## Confirmed findings

### 1. Removed definitions leave active rules behind

`crates/norn/src/tools/diagnostics_infra.rs:140` removes `lsp`, `diagnostics`, `remediation`, and `reports` tables before parsing workspace configuration. It does not remove or explicitly classify their rule activations. Its `is_non_executing` predicate permits a missing tool lookup.

`crates/norn/src/tools/diagnostics_check/post_check.rs:269` warns and skips when an activated tool has no definition. Unresolved rule languages likewise warn and skip at line 261. Neither becomes a failed/incomplete check. Matching files and overlapping rules can repeat these warnings after each edit and at configured stop/task boundaries.

This is a concrete explanation for this class of warning flood, not a confirmed identification of Tom's current configuration. Norn's own checked-in CONVENTIONS file avoids the stripped declarations; that does not establish which file the running agent loaded.

### 2. Invalid configuration becomes no configuration

`crates/norn/src/tools/diagnostics_infra.rs:80` catches read/parse/validation failures, logs them, and sets conventions to `None`. That is the same state as an absent optional file. Post-check and stop paths then pass/proceed without conventions. A broken declared policy must not appear equivalent to an intentionally unconfigured workspace.

### 3. Generator and runtime disagree

`crates/norn-cli/src/commands/init/conventions.rs:199` generates bundled language definitions and rules containing subprocess tools. Its tests around lines 632, 649, 758 and 840 explicitly expect Clippy/rustfmt activations. These definitions are removed by the normal workspace loader.

The earlier repository review `docs/reviews/2026-08-08-conventions-wip-review.md` had already recorded this gap. Current source still contains it.

### 4. Path failures do not fail validation

`crates/norn/src/tools/diagnostics_check/post_check.rs:130` logs a `check_convention_file` error but does not add it to `all_errors`. The final outcome can therefore be Pass. The current fallible path in that helper is workspace-relative conversion.

The accumulator at line 68 also omits paths when conversion fails. These files are then absent from the stop/task-complete snapshot. `workspace_relative_path` at line 370 uses lexical prefix stripping; it does not establish file identity across symlinks or normalize parent components. Launch root canonicalization alone does not establish that mutation paths share its spelling.

`crates/norn/src/tools/agent/spawn_context.rs:288` deliberately shares the parent's DiagnosticInfra, including root and modified-file set, with children. The repair must define which policy applies when a child works in another worktree; rebasing the string or silently loading a different policy is not sufficient.

### 5. Warning presentation obscures the problem

`crates/norn-tui/src/app/diagnostics.rs:24` labels every event with only level and module. Actual details are retained in the local expandable body. `render/transcript_items.rs` gives each notice its own transcript row. Consequently different failures appear identical and repeated diagnostics consume the conversation.

A summary should name the check, reason and affected file/rule when available. Aggregate repeated identical configuration failures without merging different causes, losing their counts/details, or treating them as agent messages. Expansion should be tested through actual interaction and body loading, not solely a manually loaded renderer fixture.

### 6. Pattern compilation is repeated in the mutation path

`crates/norn/src/tools/diagnostics_check/trigger.rs:77` recompiles each activated pattern for each file/check. The pinned Chiron config already exposes compiled patterns (`conventions/config.rs:248`). Reuse immutable compiled matchers while preserving each activation's handling override. Do not cache clean results without binding them to actual content and policy versions.

### 7. Existing limits must remain explicit

`stop_hook.rs:49` consumes the failure outcome and discards advisories; Norn's checked-in configuration correctly documents this and limits advisory triggers to tool time. The pattern engine used by this tree scans whole files and the existing configuration intentionally keeps six patterns advisory because of inline test-code exceptions. Do not upgrade them to block without a separately verified scope-aware matcher.

`diagnostics_check/loc.rs:16` returns without a finding when the shared line counter returns None. Audit that dependency contract before assigning a failure category; do not assume every None is an unreadable file.

## Preserve these working contracts

- Post-validation runs after the mutation. A failed check does not mean the edit was rolled back.
- `crates/norn/src/tool/registry.rs:352` attaches diagnostics/advisories to output; gate-mode failures retain committed output at line 364. Repairs must preserve that distinction in model and operator views.
- Declarative workspace data must not acquire subprocess execution authority as a side effect of fixing dangling activations. If command-backed conventions are desired, admit them through an explicit trusted runtime/profile policy.
- File/rule/trigger/handling identity must survive from loader through result, stop logic, tool output, and UI.
- Children and changed working directories must retain explicit policy provenance.

## Repair order and acceptance

1. **Admission and truthful outcomes.** Distinguish absent, invalid, restricted and active configuration. Resolve activations once; report missing names and rejected capabilities at admission. Ensure a check that cannot run cannot produce a clean pass. Preserve committed mutation receipts. Test malformed files, unknown tools/languages, stripped definitions, and gate/report outcomes.
2. **Workspace and child scope.** Bind mutation paths to canonical workspace identity using the existing file-access rules. Track unresolved checks through completion. Test relative/absolute paths, symlink spellings, parent components, deleted files, child worktrees and inherited policy.
3. **Readable diagnostics.** Show a useful collapsed explanation, expand complete details reliably, and coalesce repeated identical notices with a count. Test distinct causes stay distinct; flood does not starve input or exit.
4. **Generator parity and documentation.** Generated configurations must round-trip through the exact production loader, with explicit reporting of every active/restricted declaration. Explain pattern/LOC enforcement versus command-backed tools and external repository gates.
5. **Performance.** Reuse compiled patterns, bound/supervise expensive work outside the UI owner, and avoid repeated reads when multiple matching rules examine the same file. Measure warm per-edit latency before claiming improvement.

First deliverable should cover admission, truthful failures, and operator visibility together. It should not silently remove requested enforcement just to eliminate warnings. Full compiler/remediation activation and test-scope-aware blocking are separate policy/engine work.

## Verification boundary

Inspected the current integration-candidate tree and its pinned Chiron source at revision 25161bc8f93484b34291184e49dc3dfdda957760. Read the earlier conventions review and compared its claims to current code. No new runtime reproduction, tests, build, installation, or performance measurement was performed. The precise live warning still requires its body or a reproducer using that session's admitted configuration.
