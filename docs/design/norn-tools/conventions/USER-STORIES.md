# Norn-Tools/Conventions — User Stories

## Human Developer — Tool Author Configuring Conventions

**S1.** As a tool author, I want to define post-mutation quality checks in a single CONVENTIONS.toml file so that I do not have to modify Rust source code to change which checks run or what thresholds apply.

**S2.** As a tool author, I want to specify different LOC limits for different file types so that each language in my project has thresholds appropriate to its ecosystem norms.

**S3.** As a tool author, I want to control which mutation tools trigger which checks so that read-only tools do not waste time running linters and write tools only run checks relevant to the modified file type.

**S4.** As a tool author, I want to add custom linters without writing Rust code so that I can integrate project-specific or language-specific tools via declarative TOML adapter definitions.

## AI Agent — Experiencing Advisory vs Blocking Checks

**S5.** As an AI agent, I want advisory diagnostics to appear in the tool output as informational items so that I can see quality feedback without my edit being rolled back.

**S6.** As an AI agent, I want blocking diagnostics to trigger a clear error with the specific violations so that I know exactly what to fix before re-attempting the edit.

**S7.** As an AI agent, I want to see no post-mutation quality checks when no CONVENTIONS.toml exists so that I am not blocked by hardcoded defaults that do not match the project's standards.

**S8.** As an AI agent, I want the tool's own AST validation to remain unconditional so that structurally invalid edits are always caught regardless of convention configuration.

## Human Developer — Project Maintainer Running Init

**S9.** As a project maintainer, I want to run norn init conventions to generate a starter configuration so that I do not have to write the TOML file from scratch or memorize the schema.

**S10.** As a project maintainer, I want the init command to detect my project's languages and available tools so that the generated configuration includes only checks that will actually work in my environment.

**S11.** As a project maintainer, I want the init command to never overwrite my existing CONVENTIONS.toml so that I do not lose manual customizations when re-running init.

**S12.** As a project maintainer, I want the generated file to include comments explaining each field so that I can understand and customize the configuration without consulting documentation.

## Human Developer — Diagnostics Adapter Author

**S13.** As a diagnostics adapter author, I want convention groups to resolve adapter names against the existing AdapterRegistry so that my compiled adapters work with CONVENTIONS.toml without extra registration code.

**S14.** As a diagnostics adapter author, I want unresolvable adapter names to log a warning rather than crash so that projects referencing CI-only tools do not fail at startup in local development.

**S15.** As a diagnostics adapter author, I want declarative TOML adapters loaded from adapters_dir to be sandboxed to subprocess execution so that custom tools cannot compromise the norn process.
