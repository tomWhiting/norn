---
type: design
cluster: norn-tools/conventions
title: "CONVENTIONS.toml: Project-Scoped Post-Mutation Validation"
---

# CONVENTIONS.toml: Project-Scoped Post-Mutation Validation

## Intention

CONVENTIONS.toml exists so that each project defines what quality checks run after file mutations (edit, write, apply_patch), which files they apply to, and whether they advise or block. When this is done, a Rust project gets clippy checking after edits while a TypeScript project gets biome checking, each with project-specific LOC limits and handling modes — all from a single, human-readable config file. No hardcoded limits, no one-size-fits-all validation.

## Problem

Post-mutation validation in Norn is currently hardcoded in `diagnostics_check.rs`:

- 500 LOC limit (200 for mod.rs/lib.rs/main.rs) — hardcoded, not configurable
- Clippy scoped to the owning crate — hardcoded, Rust-only
- Bypass detection — hardcoded, always runs
- All checks produce errors that flow through Gate/Report mode — no distinction between advisory and blocking

This creates three problems:

1. **No project customization.** A project that uses 600-line files legitimately cannot adjust the limit. A TypeScript project gets no validation at all (no adapters registered). A Python project cannot add mypy or ruff checks.

2. **All-or-nothing severity.** Every diagnostic is treated the same — either it blocks (Gate tools) or it's reported as an error (Report tools). There is no way to say "warn me about LOC but block on clippy violations."

3. **Cross-crate noise.** Clippy runs on the owning crate, which can report diagnostics in dependencies. The agent cannot distinguish "my edit caused this" from "this was already broken." Scoping checks to the modified file's conventions reduces this noise.

## Solution

### D1: CONVENTIONS.toml format

A TOML file in the project root (or a configurable path) defining named convention groups. Each group specifies which tools trigger it, which file paths it applies to, and what checks to run with what handling:

```toml
[rust-general]
tools = ["write", "edit", "apply_patch"]
paths = ["*.rs"]
loc = { limit = 500, handling = "advise" }
advise_on = ["clippy"]
block_on = []

[rust-entrypoints]
tools = ["write", "edit"]
paths = ["**/mod.rs", "**/lib.rs", "**/main.rs"]
loc = { limit = 200, handling = "advise" }

[typescript]
tools = ["write", "edit", "apply_patch"]
paths = ["*.ts", "*.tsx"]
loc = { limit = 400, handling = "advise" }
advise_on = ["biome"]
block_on = ["tsc"]

[python]
tools = ["write", "edit", "apply_patch"]
paths = ["*.py"]
loc = { limit = 300, handling = "advise" }
advise_on = ["ruff"]
block_on = ["mypy"]

[shell]
tools = ["write", "edit"]
paths = ["*.sh", "*.bash"]
advise_on = ["shellcheck"]
block_on = []
```

Fields:

- `tools`: which mutation tools trigger this convention group. Only these tools run these checks after modifying matching files.
- `paths`: glob patterns matched against the modified file's path. Multiple groups can match the same file; all matching groups run (most-specific-wins for conflicting LOC limits).
- `loc`: file length check. `limit` is the line count threshold. `handling` is "advise" (informational) or "block" (error).
- `advise_on`: list of adapter names whose results are advisory (appear in tool output but do not set is_error).
- `block_on`: list of adapter names whose results are blocking (appear in tool output and set is_error, triggering Gate rollback for edit/apply_patch).
- `bypass_detection`: optional boolean (default true for supported languages). Whether to scan for lint-silencing constructs (#[allow], eslint-disable, etc.).
- `adapters_dir`: optional path to a directory containing custom declarative adapter TOML files.

### D2: Convention matching

When a mutation tool modifies a file, DiagnosticsPostCheck:

1. Reads the ConventionsConfig from DiagnosticInfra
2. Finds all convention groups where the current tool name is in `tools` AND the modified file path matches any `paths` glob
3. For each matching group, runs the specified checks

Multiple groups can match. When they conflict on LOC limit, the group with the highest `priority` field wins (default 0). Groups at the same priority with conflicting values: warn at startup, first-match wins. When they agree (or cover non-overlapping fields), all checks from all matching groups run.

```toml
[rust-entrypoints]
priority = 10
paths = ["**/mod.rs", "**/lib.rs", "**/main.rs"]
loc = { limit = 200, handling = "advise" }
```

If no convention group matches a file, no post-mutation checks run for that file. This is intentional — unconfigured file types get no validation rather than hardcoded defaults.

### D3: Advise vs block handling

The convention's handling field determines how check results flow through the tool lifecycle. This layers ON TOP of the tool's structural PostValidateMode (Gate for edit/apply_patch, Report for write), it does not override it.

**advise**: Diagnostic results are returned as advisories via a new `PostCheckResult` struct. The `RuntimePostValidateCheck` trait's `check()` method returns `PostCheckResult { outcome: PostValidateOutcome, advisories: Vec<Advisory> }` instead of bare `PostValidateOutcome`. Advisory items are injected into the tool output under an `advisories` key by the registry. They do not set `is_error`. They do not cause PostValidateOutcome::Fail. The model sees them as informational. DiagnosticCollector records them for CLI/TUI rendering. The trait change is low-impact: only one production implementor (DiagnosticsPostCheck) and two test stubs exist.

**block**: Diagnostic results are included in the tool output under `diagnostics` with severity "error". They set `is_error = true`. They cause PostValidateOutcome::Fail. For Gate tools (edit, apply_patch), this is the path where the committed/failed transparency fix (see Prerequisites) matters — the model must know the file was committed even though diagnostics failed.

The tool's own AST validation (tree-sitter parse check) remains unconditional and always blocks — that is structural validity, not a convention. CONVENTIONS.toml controls quality checks only.

### D4: Adapter resolution

Adapter names in `advise_on` and `block_on` resolve against the AdapterRegistry on DiagnosticInfra. The resolution order:

1. Compiled adapters (clippy, nextest, biome, gleam_check, gleam_test, file_length) — always available
2. Declarative TOML adapters loaded from the diagnostics crate's built-in adapters directory
3. Project-local declarative adapters loaded from `adapters_dir` if specified in the convention group

If an adapter name does not resolve, the convention entry is skipped with a warning logged at startup. This is not an error — a project may reference adapters that are only available in certain environments (e.g., a CI-only linter).

### D5: Custom adapters via declarative TOML

Users can define custom check adapters without writing Rust code. A TOML file in the project's adapters directory:

```toml
[adapter]
name = "mypy"
language = "python"
file_patterns = ["*.py"]
output_format = "json-lines"

[adapter.command]
binary = "mypy"
args = ["--output=json", "{file}"]

[adapter.mapping]
severity = "severity"
message = "message"
file = "file"
line = "line"
column = "column"
code = "code"
```

The declarative adapter engine in the diagnostics crate already supports this format. CONVENTIONS.toml adds the `adapters_dir` field to point at a project-local directory. Adapters are loaded at startup and registered on the AdapterRegistry alongside the compiled adapters.

For tools with non-JSON output, a compiled adapter (Rust code) is needed. Most modern linters support JSON output modes.

### D6: ConventionsConfig on DiagnosticInfra

DiagnosticInfra gains a new field:

```rust
pub struct DiagnosticInfra {
    pub adapters: Arc<AdapterRegistry>,
    pub policies: Arc<PolicyRegistry>,
    pub bypass_detector: Arc<BypassDetector>,
    pub workspace_root: PathBuf,
    pub conventions: Option<ConventionsConfig>,
}
```

ConventionsConfig is parsed from CONVENTIONS.toml at session startup. If no file exists, conventions is None and DiagnosticsPostCheck runs no checks (replacing the current hardcoded behavior).

The PolicyRegistry is unchanged. It holds static expert knowledge about lint codes ("clippy::unwrap_used is a safety issue, propagate with ?"). CONVENTIONS.toml configures which adapters run and how their results are handled. These are separate concerns:

- PolicyRegistry: WHAT GUIDANCE to give for a specific finding
- ConventionsConfig: WHICH CHECKS TO RUN and whether results advise or block

### D7: Init command

`norn init conventions` scans the project and generates a starter CONVENTIONS.toml:

1. Walks the file tree, collects file extensions and their counts
2. For each file type, checks which compiled adapters are available (clippy for .rs, biome for .ts, etc.)
3. Checks which external tools are installed (mypy, ruff, eslint, shellcheck, etc. via `which`)
4. Generates convention groups with recommended settings:
   - LOC limits based on the language ecosystem norms
   - advise_on populated with available adapters
   - block_on empty by default (user opts in to blocking)
5. Writes to CONVENTIONS.toml with comments explaining each field

The generated file is a starting point. Users edit it to match their project's standards. The init command never overwrites an existing file — it appends missing groups or writes to a new path.

### D8: Interaction with existing validation

CONVENTIONS.toml replaces the hardcoded checks in diagnostics_check.rs. The migration path:

1. When `conventions` is Some, use it exclusively for post-mutation checks
2. When `conventions` is None (no file), run no post-mutation quality checks. The tool's own AST gate (tree-sitter syntax validation inside execute()) still runs — that is structural, not quality.
3. The hardcoded 500/200 LOC limits, hardcoded clippy check, and hardcoded bypass detection in diagnostics_check.rs are removed. They become the default values in `norn init conventions` output.

This is a clean break, not a fallback chain. Projects that want validation must have a CONVENTIONS.toml. The init command makes creating one easy.

### D9: Search tool .gitignore integration (standalone)

This is a separate change from CONVENTIONS.toml, extracted here as a reference. The search tool gains default ignore patterns: `.git/` directory (always ignored) and `.gitignore` patterns (parsed and applied to search traversal). No other directories are hardcoded. Implementation is a standalone fix to the search tool's directory walker, with no dependency on the conventions system. See the search tool implementation for details.

## Prerequisites

**PR0: Lifecycle transparency fix.** `ToolError::PostValidationFailed` must carry an optional `committed_output`. Blocking convention checks cause PostValidateOutcome::Fail, which currently discards the structured output. Without PR0, the model won't know whether its edit landed when a blocking convention fires.

**PR1: PostCheckResult type.** `RuntimePostValidateCheck::check()` must return `PostCheckResult { outcome, advisories }` so that advisory convention results flow through the output without triggering Fail. This is the mechanism that enables the advise/block distinction in D3.

## Goals

G1. A project's post-mutation quality checks are defined in a single CONVENTIONS.toml file, not hardcoded in the runtime.

G2. Each convention group specifies which tools, file paths, checks, and handling modes apply — advisory vs blocking.

G3. Custom linters can be added without writing Rust code via declarative TOML adapter definitions.

G4. `norn init conventions` generates a starter file by scanning the project for file types and available tools.

G5. The advisory/blocking distinction layers on top of the tool's structural validation (AST gate), not replacing it.

G6. Search tool respects .gitignore to avoid returning results from build artifacts and vendored dependencies.

## Non-Goals

NG1. Runtime convention changes. CONVENTIONS.toml is read at session startup. Changes during a session require restart. Live reload is not in scope.

NG2. Convention inheritance. No cascading configs, no per-directory overrides, no include directives. One file, flat groups, glob matching. Complexity can be added later if needed.

NG3. Convention-driven tool availability. CONVENTIONS.toml controls post-mutation checks, not which tools are available. Tool availability is controlled by profiles and permissions, not conventions.

NG4. Replacing the PolicyRegistry. Conventions control which checks run and how results are handled. Policies control what guidance to give for specific lint codes. These are separate systems.

NG5. Automatic convention discovery from CI configs. Reading .github/workflows, Makefile, or package.json to infer conventions is interesting but out of scope. The init command detects available tools; CI integration is future work.

## Constraints

CO1. CONVENTIONS.toml is optional. Projects without one get no post-mutation quality checks (only structural AST validation from the tool itself). This is intentional — no assumed defaults.

CO2. Adapter names must resolve against the AdapterRegistry at startup. Unresolvable names log a warning but do not prevent other conventions from loading.

CO3. LOC counting uses tokei (the same library used today). Language-aware line counting, not raw wc -l. The file_length adapter handles this.

CO4. The init command never overwrites an existing CONVENTIONS.toml. It writes to a new path or appends missing groups.

CO5. Glob matching uses the same glob crate as the search tool. Path matching is relative to the workspace root.

CO6. bypass_detection defaults to true for languages with known silencing constructs (Rust, TypeScript, Gleam). For other languages, it defaults to false.

CO7. Custom adapters from `adapters_dir` are sandboxed to subprocess execution only. They cannot load arbitrary code into the norn process.
