//! Task management tool: CRUD plus hierarchical tasks and named groups.
//!
//! The [`task`](self) tool exposes create / get / list / update / complete
//! operations alongside hierarchy (`create_subtask`, `children`,
//! `ancestors`, `claim`) and group (`create_group`, `list_groups`)
//! operations over a [`TaskStore`] trait. The trait abstracts storage so an
//! orchestrator can plug a persistent backend; this module ships an
//! [`InMemoryTaskStore`] for tests and ephemeral sessions.

pub mod disk;
pub mod memory;
pub mod rollup;
pub mod tool;
pub mod types;

pub use self::disk::DiskTaskStore;
pub use self::memory::{InMemoryTaskStore, SharedTaskStore};
pub use self::rollup::effective_status;
pub use self::tool::TaskTool;
pub use self::types::{TaskEntry, TaskStatus, TaskStore};
