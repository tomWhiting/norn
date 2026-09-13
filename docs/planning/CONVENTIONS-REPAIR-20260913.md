# Conventions repair — 13 September 2026, Melbourne

Owner: Tom. Scope: repair configuration admission, post-check failure reporting, generated configuration parity, and readable diagnostic headings. Preserve non-executing workspace authority and committed mutation output.

R1: Missing configuration stays optional; invalid declared configuration must be retained as failure and refused during ordinary agent assembly.
R2: Activated names must resolve against the declared definitions before workspace capability filtering. Declared subprocess/LSP checks do not prevent startup: retain their path, tool, trigger and handling as explicit unavailable-check findings. Advise remains advisory; block remains blocking when the check is due. Process definitions never enter executable infrastructure. A missing language/tool and an out-of-root check must produce findings, not a clean pass.
R3: Generated configuration must pass the same non-executing loader and contain no activations of removed tools.
R4: Collapsed diagnostics must include their explanatory first line; full original detail remains expandable.
R5: Regression tests, fmt, strict Clippy and AST scan must pass; installed state must be reported separately.

File wall: tools/diagnostics_infra.rs and new conventions_admission.rs; tools/diagnostics_check/{mod,infra,post_check,stop_hook,tests,admission_regression_tests,unavailable}.rs and admission_liminal_fixture.toml; agent/assembly/runtime/base.rs; agent/builder/build.rs; norn-cli commands/init/conventions.rs; norn-tui app/{diagnostics,diagnostics_tests}.rs; this brief; conventions review; release notes. Paths under crates/norn/src unless another crate is specified.

Out of this patch: changing inherited workspace policy, enabling subprocess definitions, test-scope matching, repeated-notice aggregation and matcher optimization. Those require separate semantics and proof.

Correction, 13 September 2026: preview.29 incorrectly rejected the existing Norn-generated Liminal configuration merely because it declared restricted checks. Preview.30 repairs that admission regression without modifying the user configuration. Truly malformed declarations and undeclared names remain configuration errors.

Preview.30 source review (Ripley, owner-assigned): checked original-name validation before sanitization, absence of executable definitions after filtering, retained path/tool/lifecycle matching, advisory/block routing, LSP-only rules and stop-time failure propagation. Formatting, strict release workspace/all-target Clippy with live-api-smoke, and changed-file AST/LOC scans passed locally. Full workspace tests and actual CLI/PTY probes pending. This is a self-review, not an independent Fable verdict.
