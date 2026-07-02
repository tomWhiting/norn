# Brief NB-P1 — working_dir on ToolContext with relative path resolution

## Goal

Every tool and command site in Norn must resolve file paths against a per-agent working directory, not the process-global CWD. This is the load-bearing prerequisite for concurrent workflow steps operating in different worktrees — without it, two steps running simultaneously will read and write each other's files.

## Why

The current code calls `std::env::current_dir()` in tools (Bash, Read, Write, Edit, Search, Glob, Grep, ApplyPatch) and non-tool sites (LoopContext, prompt commands, Rhai run_cmd, shell hooks, rules engine, skill templates). Process CWD is a global mutable singleton — setting it for one agent changes it for all agents in the process. The Norn builder API requires `.working_dir(path)` which sets the directory per-agent on ToolContext. Tools resolve relative paths by joining against this field. Absolute paths pass through unchanged.

## User Stories

- S1: As a workflow author, I want two steps running in parallel worktrees to each see only their own files, so that concurrent execution is safe.
- S2: As a tool author, I want a single `ctx.resolve_path(path)` helper that handles both relative and absolute paths, so I don't need to think about CWD.
- S3: As an agent using Bash, I want `cd` to update my working directory for subsequent commands, so that relative paths in later tools reflect where I navigated to.

## Requirements

### R1 — ToolContext working_dir field and resolve_path helper

**File:** `crates/norn/src/tool/context.rs`

Add `working_dir: PathBuf` field to ToolContext. Default: `std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))`. Add `pub fn working_dir(&self) -> &Path` accessor. Add `pub fn resolve_path(&self, path: impl AsRef<Path>) -> PathBuf` helper: if path starts with `~`, expand tilde to `dirs::home_dir()`; if path is absolute, return as-is; if relative, join with working_dir.

**Acceptance:**
- ToolContext has a `working_dir: PathBuf` field
- `resolve_path("/absolute/path")` returns the path unchanged
- `resolve_path("relative/path")` returns `working_dir.join("relative/path")`
- `resolve_path("~/src/main.rs")` returns `home_dir/src/main.rs`
- Default working_dir is current_dir at construction time
- ToolContext remains Clone

### R2 — Filesystem tools updated

**Files:** `crates/norn/src/tools/read.rs`, `write.rs`, `edit.rs`, `glob_tool.rs`, `grep.rs`

Every file path argument is resolved via `ctx.resolve_path()` before any filesystem operation. No tool calls `std::env::current_dir()` directly.

**Acceptance:**
- read.rs: file_path resolved before fs::read
- write.rs: file_path resolved before fs::write and try_exists
- edit.rs: file_path resolved before read_to_string and write
- glob_tool.rs: path argument resolved before glob expansion
- grep.rs: path argument resolved before search
- `grep -rn 'current_dir' crates/norn/src/tools/{read,write,edit,glob_tool,grep}.rs` returns zero matches

### R3 — Bash tool CWD and cd parsing

**File:** `crates/norn/src/tools/bash.rs`

Bash spawns child processes with `cmd.current_dir(ctx.working_dir())`. After the command completes, the tool parses the command string for `cd` statements using a regex match (e.g., `cd\s+(.+?)(?:\s*[;&|]|$)`), resolves the target path (absolute as-is, relative joined with current working_dir), verifies the directory exists, and updates the session's tracked working_dir. No wrapping, no appending, no echoing, no subprocesses — just parsing what the model sent.

**Acceptance:**
- Child process CWD is ctx.working_dir(), not the process CWD
- After `cd /foo`, subsequent tools resolve relative paths against /foo
- After `cd foo/bar`, the path is resolved relative to current working_dir
- After `cd ..`, working_dir moves up one level
- After `ls && cd /foo`, cd is detected and working_dir updated
- After `cd /foo && ls`, cd is detected and working_dir updated
- `cd` to a non-existent directory does not change working_dir
- Handles `~` in cd target (tilde expanded to home directory)
- Does not attempt to handle pushd/popd, cd inside conditionals, or cd inside shell functions — these are exotic edge cases models don't use

### R4 — ApplyPatch and Search tools

**Files:** `crates/norn/src/tools/patch.rs`, `crates/norn/src/tools/search/mod.rs`

ApplyPatch resolves file paths in patch headers against ctx.working_dir(). Search uses ctx.working_dir() as the default root directory instead of `PathBuf::from(".")`.

**Acceptance:**
- patch.rs: resolve_path uses ctx.working_dir() as base
- search/mod.rs: default search root is ctx.working_dir()
- No `PathBuf::from(".")` as default root in search

### R5 — Skill, spawn, and fork tools

**Files:** `crates/norn/src/tools/skill.rs`, `crates/norn/src/tools/agent/spawn.rs`, `crates/norn/src/tools/agent/fork_tool.rs`

Skill tool uses ctx.working_dir() instead of `std::env::current_dir()`. Spawn tool uses ctx.working_dir() for scan directories. Fork tool inherits parent's working_dir to child ToolContext.

**Acceptance:**
- skill.rs: no current_dir() calls
- spawn.rs: scan dirs use ctx.working_dir()
- fork_tool.rs: child ToolContext.working_dir = parent ToolContext.working_dir
- `grep -rn 'current_dir' crates/norn/src/tools/skill.rs crates/norn/src/tools/agent/` returns zero matches

### R6 — Non-tool command sites

**Files:** `crates/norn/src/loop/loop_context.rs`, prompt command sites, Rhai run_cmd, shell hook execution, rules engine, skill template rendering

LoopContext gains `working_dir: Option<PathBuf>` field. All command execution sites that spawn processes or resolve paths read from LoopContext.working_dir, falling back to std::env::current_dir() only when None.

**Acceptance:**
- LoopContext has working_dir field
- Prompt command execution uses LoopContext.working_dir for child process CWD
- Shell hook execution uses LoopContext.working_dir
- Rules engine path resolution uses LoopContext.working_dir
- Skill template working_dir variable reflects LoopContext.working_dir
- All sites fall back gracefully when working_dir is None (backward compat)

## Checklist

- [ ] C1: ToolContext.working_dir field exists with accessor
- [ ] C2: resolve_path() handles absolute and relative paths
- [ ] C3: read.rs uses resolve_path
- [ ] C4: write.rs uses resolve_path
- [ ] C5: edit.rs uses resolve_path
- [ ] C6: glob_tool.rs uses resolve_path
- [ ] C7: grep.rs uses resolve_path
- [ ] C8: bash.rs sets child CWD from ctx.working_dir()
- [ ] C9: bash.rs parses command for cd, resolves target, updates working_dir (no wrapping/appending)
- [ ] C10: patch.rs resolves paths against ctx.working_dir()
- [ ] C11: search/mod.rs uses ctx.working_dir() as default root
- [ ] C12: skill.rs uses ctx.working_dir()
- [ ] C13: spawn.rs uses ctx.working_dir()
- [ ] C14: fork_tool.rs inherits working_dir to child
- [ ] C15: LoopContext.working_dir field exists
- [ ] C16: Prompt commands use LoopContext.working_dir
- [ ] C17: Shell hooks use LoopContext.working_dir
- [ ] C18: Rules engine uses LoopContext.working_dir
- [ ] C19: Zero 'current_dir' grep matches in tools/ and loop/ (excluding task.rs, web, coord)
- [ ] C20: All existing tests pass
- [ ] C21: Clippy clean

## Boundaries

- SHALL NOT change tool argument types or schemas — path resolution is internal
- SHALL NOT remove std::env::current_dir() from the default ToolContext constructor
- SHALL NOT modify task.rs, web tools, or coordination tools
- SHALL NOT add working_dir to the Tool trait — it lives on ToolContext
