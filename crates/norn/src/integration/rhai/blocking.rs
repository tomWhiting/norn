//! Synchronous helper builtins: file I/O, JSON, and shell commands.

use std::path::PathBuf;
use std::process::Command;

use rhai::{Dynamic, Engine, EvalAltResult, ImmutableString, Map};

use super::context::{AgentHandle, dynamic_to_json, json_to_dynamic, rhai_error};

pub(super) fn register_blocking(
    engine: &mut Engine,
    working_dir: crate::tool::context::SharedWorkingDir,
) {
    engine.register_type_with_name::<AgentHandle>("AgentHandle");
    engine.register_fn("to_string", AgentHandle::to_string_repr);

    engine.register_fn(
        "read_file",
        |path: ImmutableString| -> Result<ImmutableString, Box<EvalAltResult>> {
            std::fs::read_to_string(path.as_str())
                .map(ImmutableString::from)
                .map_err(|e| Box::new(rhai_error(format!("read_file('{path}'): {e}"))))
        },
    );

    engine.register_fn(
        "write_file",
        |path: ImmutableString, contents: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            if let Some(parent) = PathBuf::from(path.as_str()).parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Box::new(rhai_error(format!("write_file('{path}'): mkdir: {e}")))
                })?;
            }
            std::fs::write(path.as_str(), contents.as_str())
                .map_err(|e| Box::new(rhai_error(format!("write_file('{path}'): {e}"))))
        },
    );

    let run_cmd_wd = working_dir;
    engine.register_fn(
        "run_cmd",
        move |command: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let output = Command::new("sh")
                .arg("-c")
                .arg(command.as_str())
                .current_dir(run_cmd_wd.get())
                .output()
                .map_err(|e| Box::new(rhai_error(format!("run_cmd: failed to spawn: {e}"))))?;
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let exit_code = i64::from(output.status.code().unwrap_or(-1));
            let mut map = Map::new();
            map.insert("stdout".into(), Dynamic::from(stdout));
            map.insert("stderr".into(), Dynamic::from(stderr));
            map.insert("exit_code".into(), Dynamic::from(exit_code));
            Ok(Dynamic::from_map(map))
        },
    );

    engine.register_fn(
        "read_json",
        |path: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let text = std::fs::read_to_string(path.as_str())
                .map_err(|e| Box::new(rhai_error(format!("read_json('{path}'): {e}"))))?;
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| Box::new(rhai_error(format!("read_json('{path}'): parse: {e}"))))?;
            json_to_dynamic(value)
        },
    );

    engine.register_fn(
        "write_json",
        |path: ImmutableString, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            let json = dynamic_to_json(&value)?;
            let pretty = serde_json::to_string_pretty(&json).map_err(|e| {
                Box::new(rhai_error(format!("write_json('{path}'): serialize: {e}")))
            })?;
            if let Some(parent) = PathBuf::from(path.as_str()).parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Box::new(rhai_error(format!("write_json('{path}'): mkdir: {e}")))
                })?;
            }
            std::fs::write(path.as_str(), pretty)
                .map_err(|e| Box::new(rhai_error(format!("write_json('{path}'): {e}"))))
        },
    );

    engine.register_fn(
        "parse_json",
        |input: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let value: serde_json::Value = serde_json::from_str(input.as_str())
                .map_err(|e| Box::new(rhai_error(format!("parse_json: {e}"))))?;
            json_to_dynamic(value)
        },
    );

    engine.register_fn(
        "to_json",
        |value: Dynamic| -> Result<ImmutableString, Box<EvalAltResult>> {
            let json = dynamic_to_json(&value)?;
            serde_json::to_string_pretty(&json)
                .map(ImmutableString::from)
                .map_err(|e| Box::new(rhai_error(format!("to_json: {e}"))))
        },
    );
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::super::context::{NornRhaiContext, build_norn_engine};
    use crate::agent::message_router::MessageRouter;
    use crate::agent::registry::AgentRegistry;
    use crate::provider::mock::MockProvider;
    use crate::provider::traits::Provider;
    use crate::session::store::EventStore;
    use crate::tool::registry::ToolRegistry;

    fn build_context() -> NornRhaiContext {
        let registry = AgentRegistry::shared();
        let router = Arc::new(MessageRouter::new());
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let agent_id = Uuid::new_v4();
        NornRhaiContext {
            registry,
            router,
            provider,
            agent_id,
            runtime: tokio::runtime::Handle::current(),
            event_store: Arc::new(EventStore::new()),
            tool_registry: Some(Arc::new(ToolRegistry::new())),
            working_dir: crate::tool::context::SharedWorkingDir::default(),
            child_policy: crate::agent::child_policy::ChildPolicy {
                messaging: crate::agent::child_policy::MessagingScope::SiblingsAndParent,
                delegation: crate::agent::child_policy::DelegationBudget {
                    remaining_depth: 2,
                    max_concurrent_children: 8,
                },
                inbound_capacity: 8,
                loop_config: None,
            },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_file_returns_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "from disk").unwrap();

        let ctx = build_context();
        let engine = build_norn_engine(&ctx);
        let script = format!(r#"read_file("{}")"#, path.to_string_lossy());
        let value: String = engine.eval(&script).unwrap();
        assert_eq!(value, "from disk");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn run_cmd_returns_stdout_exit_code() {
        let ctx = build_context();
        let engine = build_norn_engine(&ctx);
        let result: rhai::Dynamic = engine.eval(r#"run_cmd("echo hi")"#).unwrap();
        let map = result.try_cast::<rhai::Map>().unwrap();
        let stdout = map.get("stdout").unwrap().clone();
        assert_eq!(stdout.into_string().unwrap().trim(), "hi");
        let exit = map.get("exit_code").unwrap().clone();
        assert_eq!(exit.as_int().unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parse_to_json_roundtrip() {
        let ctx = build_context();
        let engine = build_norn_engine(&ctx);
        let script = r#"
            let v = parse_json("{\"k\": 7}");
            v.k
        "#;
        let v: i64 = engine.eval(script).unwrap();
        assert_eq!(v, 7);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_then_read_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("data.json");
        let ctx = build_context();
        let engine = build_norn_engine(&ctx);
        let script = format!(
            r#"
                let value = #{{ a: 1, b: "two" }};
                write_json("{p}", value);
                let r = read_json("{p}");
                r.a
            "#,
            p = path.to_string_lossy()
        );
        let v: i64 = engine.eval(&script).unwrap();
        assert_eq!(v, 1);
    }
}
