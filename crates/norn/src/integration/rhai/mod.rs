//! Rhai builtins for agent operations.
//!
//! Two host-function categories per the grilling refinement:
//!
//! 1. **Blocking helpers** — `read_file`, `run_cmd`, `read_json`,
//!    `parse_json`, `to_json`, `write_json`, `write_file`. Mirror the
//!    workspace `cmd_builtins` set and operate synchronously inside Rhai.
//!
//! 2. **Agent operations** — `spawn_agent` (returns an opaque
//!    [`AgentHandle`] wrapping a [`uuid::Uuid`]) and `signal_agent`
//!    (returns the delivery sequence). The underlying work runs on the
//!    Tokio runtime via a stored runtime handle.

mod agent_ops;
mod blocking;
mod context;

pub use context::{
    AgentHandle, NornRhaiContext, build_norn_engine, eval_with_args, register_norn_builtins,
};
