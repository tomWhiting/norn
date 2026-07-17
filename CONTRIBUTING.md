# Contributing to Norn

## Prerequisites

- **Rust 1.94.0** — pinned via `rust-toolchain.toml` (edition 2024). Install with [rustup](https://rustup.rs/); the toolchain file will be picked up automatically.
- **Git** — for cloning and submitting changes.
- **clippy and rustfmt** — included in the toolchain components (`rust-toolchain.toml` declares both).

## Building

```sh
cargo build --workspace
```

Run the CLI:

```sh
cargo run --bin norn -- --help
```

Run Norn as an MCP server over stdio:

```sh
cargo run --bin norn -- mcp serve
```

## Testing

```sh
cargo test --workspace
```

## Linting

Both gates must pass clean before any commit:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

If clippy fires, fix the code. `#[allow(...)]`, `#[expect(...)]`, `#[ignore]` on tests, underscore-prefixed variable renames, and `#[cfg(any())]` dead-code hiding are all considered bypasses, not fixes.

**Exception:** `#[allow(...)]` is permitted inside `#[cfg(test)]` modules (e.g., `clippy::unwrap_used` in test assertions).

Auto-format:

```sh
cargo fmt
```

## Repository layout

```
crates/
  norn/              Core runtime (~150K lines, ~2,600 tests)
    src/
      agent/         AgentBuilder, registry, forking, child policy, resume
      loop/          Agent step state machine, compaction, schema
      provider/      Provider trait, OpenAI Responses API, OAuth
      session/       Event store, action logs, context editing, session trees
      rules/         Rules engine (triggers, delivery modes, lifecycle)
      tools/         Tool implementations (bash, search, lsp, agent, web, ...)
      tool/          Tool trait, lifecycle, registry, context, envelope
      integration/   Claude adapter, MCP client/server/proxy, Rhai, hooks
      config/        Settings loading, merge, permissions, validation
      skill/         Skill system (loadable SKILL.md prompt templates)
      system_prompt/ System prompt construction
    examples/        chat.rs, login.rs, smoke.rs
  norn-cli/          The norn binary — print mode, JSON-RPC driven mode (~16K lines)
  norn-tui/          Terminal user interface (~26K lines)
  norn-macros/       Derive macros for tool argument schemas (~2K lines)

docs/                Design documents, decision logs, integration plans
  design/            Per-subsystem design clusters with briefs
  reviews/           Code review records

assets/              Model catalog (models.json)
resources/           Reference data (codex models, request shapes, tool schemas)
```

## Error handling

- `thiserror` for library errors with domain-specific types.
- `anyhow` only in the binary crate for top-level reporting.
- Never `.unwrap()` or `.expect()` in library code. Handle mutex/lock poisoning explicitly and map to a typed error.

## Code style

- **No god files.** Nothing over 500 lines of code (excluding tests, comments, whitespace). `mod.rs` files contain only `pub mod` declarations and re-exports.
- **No silent failures.** Every error is handled, logged, or propagated.
- **No TODO/FIXME markers** in shipped code — resolve them before committing.

For the full coding standards, see `CLAUDE.md`.

## Conventions system

Norn uses `CONVENTIONS.toml` for automated post-mutation checks. When Norn (or an agent running inside Norn) edits a `.rs` file, the conventions system runs pattern checks (AST and regex) and diagnostic tools (clippy, rustfmt) automatically. See the file for details.

## Design documents

Implementation is driven by per-subsystem briefs under `docs/design/<cluster>/briefs/`. Each brief has numbered requirements, acceptance criteria, and file paths. Read the relevant design docs before working on a subsystem.

## License

By contributing, you agree that your contributions will be licensed under the AGPL-3.0 license (see `LICENSE`). A commercial license is also available — contact Ablative for details.
