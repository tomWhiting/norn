//! Unified Search tool.
//!
//! Supports four operating modes selected by the `mode` argument:
//!
//! * **Content search** (`mode: "content"`, default when `pattern` is set) --
//!   walks the target tree and returns regex-matching lines as
//!   `file:line:content` tuples.
//! * **File finding** (`mode: "files"`, default when only `glob` is set) --
//!   expands the glob against the filesystem and returns matching paths.
//! * **Fuzzy file matching** (`mode: "fuzzy"`) -- scores file paths against
//!   `pattern` using the `nucleo-matcher` algorithm and returns them ranked
//!   best-first as `{path, score}` pairs.
//! * **AST structural search** (`mode: "ast"`) -- parses each candidate file
//!   with tree-sitter and evaluates an S-expression query (`ast_query`)
//!   against it, returning matched node locations and captured text.
//!
//! All four modes are read-only: `effect()` is
//! [`ToolEffect::ReadOnly`](crate::tool::scheduling::ToolEffect::ReadOnly) so
//! the scheduler may dispatch multiple Search calls concurrently with other
//! read-only tools. Walks honour gitignore/hidden-file rules by default
//! (`include_ignored: true` disables that), respect the agent's workspace
//! confinement, and report unreadable entries in a `skipped` array.

mod ast_search;
mod content;
mod file_find;
mod fuzzy;
mod helpers;
mod tool;

pub use self::tool::SearchTool;
