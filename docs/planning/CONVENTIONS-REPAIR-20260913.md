# Conventions repair — 13 September 2026, Melbourne

Owner: Tom. Scope: repair configuration admission, post-check failure reporting, generated configuration parity, and readable diagnostic headings. Preserve non-executing workspace authority and committed mutation output.

R1: Missing configuration stays optional; invalid declared configuration must be retained as failure and refused during ordinary agent assembly.
R2: Activated names must resolve after workspace capability filtering. A missing language/tool and an out-of-root check must produce findings, not a clean pass.
R3: Generated configuration must pass the same non-executing loader and contain no activations of removed tools.
R4: Collapsed diagnostics must include their explanatory first line; full original detail remains expandable.
R5: Regression tests, fmt, strict Clippy and AST scan must pass; installed state must be reported separately.

File wall: tools/diagnostics_infra.rs and new conventions_admission.rs; tools/diagnostics_check/{infra,post_check,stop_hook,tests,admission_regression_tests}.rs; agent/assembly/runtime/base.rs; agent/builder/build.rs; norn-cli commands/init/conventions.rs; norn-tui app/{diagnostics,diagnostics_tests}.rs; this brief; conventions review; release notes. Paths under crates/norn/src unless another crate is specified.

Out of this patch: changing inherited workspace policy, enabling subprocess definitions, test-scope matching, repeated-notice aggregation and matcher optimization. Those require separate semantics and proof.
