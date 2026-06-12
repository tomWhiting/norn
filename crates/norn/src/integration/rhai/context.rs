//! Shared Rhai context, handle types, helpers, and the entry-point
//! registration functions.

use std::sync::Arc;

use parking_lot::RwLock;
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use uuid::Uuid;

use crate::agent::child_policy::ChildPolicy;
use crate::agent::message_router::MessageRouter;
use crate::agent::registry::AgentRegistry;
use crate::provider::traits::Provider;
use crate::session::store::EventStore;
use crate::tool::registry::ToolRegistry;

/// Opaque Rhai handle wrapping a sub-agent UUID.
#[derive(Clone, Copy, Debug)]
pub struct AgentHandle(pub Uuid);

impl AgentHandle {
    /// Underlying agent id.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.0
    }

    /// String representation suitable for Rhai printing.
    #[must_use]
    pub fn to_string_repr(&self) -> String {
        self.0.to_string()
    }
}

/// Shared context passed to every Rhai host function.
///
/// Holds the agent registry, message router, provider, calling-agent id,
/// a Tokio runtime handle so synchronous Rhai code can bridge into async
/// work, the parent event store (used by `fork_agent`), and the shared
/// tool registry handed to spawned and forked sub-agents.
#[derive(Clone)]
pub struct NornRhaiContext {
    /// Agent registry (write-locked when spawning).
    pub registry: Arc<RwLock<AgentRegistry>>,
    /// Message router used for `send_message` — deliveries land on the
    /// recipient's inbound channel or fail typed; nothing is queued
    /// where no loop drains.
    pub router: Arc<MessageRouter>,
    /// Provider used when launching a sub-agent step.
    pub provider: Arc<dyn Provider>,
    /// Id of the agent invoking the script (sender for `send_message`).
    pub agent_id: Uuid,
    /// Tokio runtime handle to bridge sync Rhai into async work.
    pub runtime: tokio::runtime::Handle,
    /// Parent event store snapshot — read by `fork_agent` when applying its
    /// context filter.
    pub event_store: Arc<EventStore>,
    /// Optional shared tool registry handed to spawned and forked
    /// sub-agents. When `None`, both spawn and fork report a clear runtime
    /// error rather than silently launching a sub-agent with no tools.
    pub tool_registry: Option<Arc<ToolRegistry>>,
    /// Shared working directory used by `run_cmd` to set the child
    /// process's CWD. Cloning this field yields a handle that shares the
    /// same underlying value as [`crate::tool::context::ToolContext`] and
    /// [`crate::r#loop::loop_context::LoopContext`].
    pub working_dir: crate::tool::context::SharedWorkingDir,
    /// The host agent's **own** granted [`ChildPolicy`] — the budget its
    /// script-driven `spawn_agent` reservations are checked against, and
    /// the base each script-spawned child's grant is derived from
    /// (inherit-with-decrement, exactly like the spawn/fork tools). The
    /// embedder supplies it deliberately — typically the same
    /// `child_policy` as its builder envelope; Norn never assumes one
    /// (W3.4).
    pub child_policy: ChildPolicy,
}

// `NornRhaiContext` has all-public fields and is constructed via struct
// literal at each call site. No `new()` constructor exists because the
// eight-field signature would exceed clippy's `too_many_arguments` budget
// and the struct fields already form the canonical parameter list.

pub(super) fn rhai_error(s: impl Into<String>) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(Dynamic::from(s.into()), rhai::Position::NONE)
}

pub(super) fn dynamic_to_json(value: &Dynamic) -> Result<serde_json::Value, Box<EvalAltResult>> {
    rhai::serde::from_dynamic(value)
        .map_err(|e| Box::new(rhai_error(format!("dynamic→json failed: {e}"))))
}

pub(super) fn json_to_dynamic(value: serde_json::Value) -> Result<Dynamic, Box<EvalAltResult>> {
    rhai::serde::to_dynamic(value)
        .map_err(|e| Box::new(rhai_error(format!("json→dynamic failed: {e}"))))
}

/// Register every Norn Rhai builtin on `engine`.
///
/// Functions registered:
///
/// **Blocking**
/// - `read_file(path) -> String`
/// - `write_file(path, contents) -> ()`
/// - `run_cmd(cmd) -> Map { stdout, stderr, exit_code }`
/// - `read_json(path) -> Dynamic`
/// - `write_json(path, value) -> ()`
/// - `parse_json(s) -> Dynamic`
/// - `to_json(value) -> String`
///
/// **Handle-returning**
/// - `spawn_agent(config: Map) -> AgentHandle`
/// - `send_message(to: AgentHandle | String, content: Dynamic) -> ()`
/// - `fork_agent(config: Map) -> Dynamic`
pub fn register_norn_builtins(engine: &mut Engine, context: &NornRhaiContext) {
    super::blocking::register_blocking(engine, context.working_dir.clone());
    super::agent_ops::register_handle_returning(engine, context);
}

/// Build a Rhai engine with all Norn builtins registered. Convenience for
/// callers that don't want to manage `Engine::new()` themselves.
#[must_use]
pub fn build_norn_engine(context: &NornRhaiContext) -> Engine {
    let mut engine = Engine::new();
    register_norn_builtins(&mut engine, context);
    engine
}

/// Evaluate a script with `args` available as a Rhai scope variable.
///
/// # Errors
///
/// Returns the underlying [`Box<EvalAltResult>`] from Rhai when evaluation
/// fails.
pub fn eval_with_args(
    engine: &Engine,
    script: &str,
    args: serde_json::Value,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut scope = Scope::new();
    let args = rhai::serde::to_dynamic(args)?;
    scope.push("args", args);
    engine.eval_with_scope(&mut scope, script)
}
