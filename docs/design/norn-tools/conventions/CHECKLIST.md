# Norn-Tools/Conventions — Checklist

## CONVENTIONS.toml Parsing and ConventionsConfig Type

- [ ] **C1** — ConventionsConfig struct defined in a conventions module under crates/norn/src/tools/
- [ ] **C2** — ConventionsConfig parsed from TOML using toml crate with #[derive(Deserialize)]
- [ ] **C3** — ConventionGroup struct has fields: tools (Vec<String>), paths (Vec<String>), loc (Option<LocCheck>), advise_on (Vec<String>), block_on (Vec<String>), bypass_detection (Option<bool>), adapters_dir (Option<PathBuf>), priority (i32 default 0)
- [ ] **C4** — LocCheck struct has fields: limit (u64), handling (Handling enum: Advise or Block)
- [ ] **C5** — ConventionsConfig::load(path) reads and parses CONVENTIONS.toml, returning Result<ConventionsConfig, ConventionsError>
- [ ] **C6** — ConventionsConfig::load returns a typed error (ConventionsError) for missing file, parse failure, and invalid glob patterns
- [ ] **C7** — DiagnosticInfra gains conventions: Option<ConventionsConfig> field
- [ ] **C8** — ConventionsConfig is parsed at session startup and stored on DiagnosticInfra before any tool execution

## Convention Matching

- [ ] **C9** — Convention groups are matched by tool name (current tool in tools list) AND file path (modified file matches any paths glob)
- [ ] **C10** — Glob patterns in paths are compiled at load time using the globset crate, same as AdapterRegistry
- [ ] **C11** — Multiple convention groups can match the same file; all matching groups run their checks
- [ ] **C12** — When multiple groups specify conflicting LOC limits, the group with the highest priority field wins
- [ ] **C13** — Groups at the same priority with conflicting LOC limits: first-match wins, warning logged at startup
- [ ] **C14** — Path matching is relative to the workspace root stored on DiagnosticInfra
- [ ] **C15** — If no convention group matches a file, no post-mutation quality checks run for that file

## Advisory and Blocking Pathway

- [ ] **C16** — PostCheckResult struct defined with fields: outcome (PostValidateOutcome), advisories (Vec<Advisory>)
- [ ] **C17** — Advisory struct defined with fields: message (String), source (String)
- [ ] **C18** — RuntimePostValidateCheck::check() return type changed from PostValidateOutcome to PostCheckResult
- [ ] **C19** — ToolRegistry::execute_tool post-check loop updated to handle PostCheckResult: outcome processed as before, advisories collected separately
- [ ] **C20** — Advisories injected into tool output under an advisories key by the registry
- [ ] **C21** — Advisories do not set is_error on the tool output
- [ ] **C22** — Advisories do not cause PostValidateOutcome::Fail
- [ ] **C23** — Adapter results from advise_on list produce Advisory items (informational)
- [ ] **C24** — Adapter results from block_on list produce errors that trigger PostValidateOutcome::Fail
- [ ] **C25** — LOC check with handling = advise produces Advisory items
- [ ] **C26** — LOC check with handling = block produces errors that trigger PostValidateOutcome::Fail
- [ ] **C27** — Tool's own AST validation (tree-sitter parse check) remains unconditional and always blocks, unaffected by conventions

## DiagnosticsPostCheck Convention Integration

- [ ] **C28** — When conventions is Some, DiagnosticsPostCheck uses convention groups exclusively for post-mutation checks
- [ ] **C29** — When conventions is None, DiagnosticsPostCheck runs no post-mutation quality checks (only structural AST validation from tool itself)
- [ ] **C30** — Hardcoded 500 LOC limit removed from diagnostics_check.rs
- [ ] **C31** — Hardcoded 200 LOC limit for mod.rs/lib.rs/main.rs removed from diagnostics_check.rs
- [ ] **C32** — Hardcoded clippy check removed from diagnostics_check.rs
- [ ] **C33** — Hardcoded bypass detection removed from diagnostics_check.rs
- [ ] **C34** — DiagnosticsPostCheck iterates matching convention groups and dispatches adapter checks per advise_on/block_on lists
- [ ] **C35** — bypass_detection defaults to true for Rust and TypeScript, false for other languages

## Adapter Resolution

- [ ] **C36** — Adapter names in advise_on/block_on resolve against the AdapterRegistry on DiagnosticInfra
- [ ] **C37** — Unresolvable adapter names log a warning at startup but do not prevent other conventions from loading
- [ ] **C38** — Declarative TOML adapters from adapters_dir are loaded and registered on AdapterRegistry alongside compiled adapters
- [ ] **C39** — Custom adapters from adapters_dir are sandboxed to subprocess execution only — no arbitrary code loading into the norn process

## Init Command

- [ ] **C40** — norn init conventions command exists as a CLI subcommand
- [ ] **C41** — Init scans the project file tree, collects file extensions and their counts
- [ ] **C42** — Init checks which compiled adapters are available for each detected file type
- [ ] **C43** — Init checks which external tools are installed via which/where lookup (mypy, ruff, eslint, shellcheck, etc.)
- [ ] **C44** — Init generates convention groups with language-ecosystem-appropriate LOC limits
- [ ] **C45** — Init populates advise_on with available adapters, block_on empty by default
- [ ] **C46** — Init writes CONVENTIONS.toml with comments explaining each field
- [ ] **C47** — Init never overwrites an existing CONVENTIONS.toml — appends missing groups or writes to a new path

## Prerequisites

- [ ] **C48** — ToolError::PostValidationFailed carries optional committed_output so blocking convention checks preserve the tool's structured output (PR0)
- [ ] **C49** — PostCheckResult type exists so advisory convention results flow through output without triggering Fail (PR1)

## Constraints

- [ ] **C50** — CONVENTIONS.toml is optional — projects without one get no post-mutation quality checks
- [ ] **C51** — LOC counting uses tokei (language-aware, not raw wc -l)
- [ ] **C52** — Glob matching uses the globset crate, same as search tool and AdapterRegistry
- [ ] **C53** — No file exceeds 500 lines of code (excluding tests, comments, whitespace)
