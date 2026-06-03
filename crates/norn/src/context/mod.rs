//! Always-on project and user context for the Norn agent runtime.
//!
//! `NORN.md` mirrors the role Claude Code's `CLAUDE.md` plays:
//! always-loaded conventions that travel with the agent's system
//! prompt. Two files are read at session start — user-level
//! (`~/.norn/NORN.md`) and project-root (`{cwd}/NORN.md`) — and their
//! content is concatenated user-first / project-second so the
//! project-specific guidance appears later in the prompt and the model
//! reads it most recently (DESIGN.md §D1).
//!
//! - [`types`] — the passive [`ContextFile`] record (path, content,
//!   mtime).
//! - [`loader`] — file discovery, reading, mtime-based staleness
//!   detection, and the [`ContextLoader::formatted_context`] string
//!   ready for appending to `system_sections[0]`.
//! - [`scanner`] — rule-file directory scanning *and* nested
//!   `NORN.md` synthetic-rule registration. [`scanner::scan_rule_dirs`]
//!   reads every `.md` file under the caller-supplied search
//!   directories, derives the rule ID from the file stem, and resolves
//!   first-found-wins on collision. [`scanner::NestedScanner`] reacts
//!   to `RuntimeEvent::PathChanged` by walking the changed file's
//!   directory ancestry inside the project root and registering a
//!   synthetic rule for every previously-unseen ancestor that contains
//!   a `NORN.md` (DESIGN.md §D4).
//!
//! Out of scope for the context cluster: Claude Code rule-file format
//! compatibility (NX-003 extends [`crate::rules::parser`] for that) and
//! `build_runtime` wiring (NX-005 wires
//! [`scanner::NestedScanner::scan_on_path_change`] into the agent loop
//! alongside the existing rules-engine `process_event` call).

pub mod loader;
pub mod scanner;
pub mod types;

pub use loader::ContextLoader;
pub use scanner::{NestedScanner, scan_rule_dirs};
pub use types::ContextFile;
