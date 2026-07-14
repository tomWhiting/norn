//! JSON wire protocol input passed to shell hook child processes.
//!
//! [`HookInput`] is the single flat struct serialised to a shell hook's
//! stdin. Five common fields are always present; per-event-type fields
//! ride along as [`Option`]s and are omitted from the JSON when absent
//! via `skip_serializing_if = "Option::is_none"`. NH-005
//! (`ShellCommandHook`) builds these values from the dispatch context;
//! this module only defines the schema.
//!
//! The five `NORN_*` constants are the environment variable names set on
//! every shell hook child process. They satisfy `DESIGN.md` D14 and are
//! re-exported from [`crate::integration::hooks`] alongside [`HookInput`].

use serde::Serialize;

/// Environment variable: working directory the shell hook child inherits.
///
/// Set verbatim to the value defined in `DESIGN.md` D14
/// (`NORN_PROJECT_DIR`). NH-005 sets this on every shell hook spawn so
/// scripts can locate project files without parsing stdin.
pub const NORN_PROJECT_DIR: &str = "NORN_PROJECT_DIR";

/// Environment variable: current session identifier.
///
/// Set verbatim to the value defined in `DESIGN.md` D14
/// (`NORN_SESSION_ID`).
pub const NORN_SESSION_ID: &str = "NORN_SESSION_ID";

/// Environment variable: current agent identifier.
///
/// Set verbatim to the value defined in `DESIGN.md` D14
/// (`NORN_AGENT_ID`).
pub const NORN_AGENT_ID: &str = "NORN_AGENT_ID";

/// Environment variable: active profile name, or the empty string when no
/// profile is active.
///
/// Set verbatim to the value defined in `DESIGN.md` D14 (`NORN_PROFILE`).
/// The empty-string sentinel mirrors [`HookInput::profile_name`] so shell
/// scripts can read either source.
pub const NORN_PROFILE: &str = "NORN_PROFILE";

/// Environment variable: the hook event type as a `snake_case` string.
///
/// Set verbatim to the value defined in `DESIGN.md` D14
/// (`NORN_HOOK_EVENT`). The string matches the corresponding
/// [`crate::integration::hooks::HookEventType`] serde rendering.
pub const NORN_HOOK_EVENT: &str = "NORN_HOOK_EVENT";

/// Context passed to a shell hook on stdin as a single flat JSON object.
///
/// The five common fields (`session_id`, `cwd`, `hook_event_name`,
/// `agent_id`, `profile_name`) are always serialised. Per-event-type
/// fields are [`Option`]s with `skip_serializing_if = "Option::is_none"`
/// so an empty pre-LLM payload renders as just the common fields plus
/// `model` and `message_count`, without empty `tool_name`/`final_text`
/// keys cluttering the wire form.
///
/// `profile_name` is a non-optional [`String`] because `DESIGN.md` D14
/// fixes the empty string as the absent-profile sentinel — matching
/// [`NORN_PROFILE`].
///
/// Constructed by NH-005 (`ShellCommandHook`). This module does not
/// populate or interpret the fields — it only defines the schema.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub struct HookInput {
    // ---- Common fields (always present) -----------------------------------
    /// Current session identifier.
    pub session_id: String,

    /// Working directory the child inherits.
    pub cwd: String,

    /// Hook event type as the `snake_case` string (`pre_tool`, `post_tool`, …).
    pub hook_event_name: String,

    /// Current agent identifier.
    pub agent_id: String,

    /// Active profile name. The empty string when no profile is active.
    pub profile_name: String,

    // ---- Tool-event fields (pre_tool / post_tool / post_tool_failure) -----
    /// Name of the tool being dispatched. Present for tool events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    /// Tool arguments as serialised by the provider. Present for tool events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,

    /// Provider-assigned tool call identifier. Present for tool events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    // ---- Post-tool fields (post_tool / post_tool_failure) -----------------
    /// Tool output value. Present for `post_tool` / `post_tool_failure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<serde_json::Value>,

    /// Tool execution duration in milliseconds.
    /// Present for `post_tool` / `post_tool_failure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_duration_ms: Option<u64>,

    /// Whether the tool output represents an error.
    /// Present for `post_tool` / `post_tool_failure`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_is_error: Option<bool>,

    // ---- LLM fields (pre_llm / post_llm) ----------------------------------
    /// Model identifier for the provider call. Present for LLM events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Number of messages in the provider request. Present for LLM events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,

    // ---- Stop fields ------------------------------------------------------
    /// The model's final text output. Present for `stop` events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,

    // ---- Subagent fields (subagent_start / subagent_stop) -----------------
    /// Child agent identifier. Present for subagent events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,

    /// Child agent profile name or agent type. Present for subagent events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
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
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    fn common_only() -> HookInput {
        HookInput {
            session_id: "sess-1".to_string(),
            cwd: "/tmp/work".to_string(),
            hook_event_name: "pre_tool".to_string(),
            agent_id: "agent-1".to_string(),
            profile_name: String::new(),
            tool_name: None,
            tool_input: None,
            tool_call_id: None,
            tool_output: None,
            tool_duration_ms: None,
            tool_is_error: None,
            model: None,
            message_count: None,
            final_text: None,
            subagent_id: None,
            subagent_type: None,
        }
    }

    fn keys_of(value: &serde_json::Value) -> Vec<String> {
        value
            .as_object()
            .expect("expected JSON object")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn env_var_constants_have_expected_values() {
        assert_eq!(NORN_PROJECT_DIR, "NORN_PROJECT_DIR");
        assert_eq!(NORN_SESSION_ID, "NORN_SESSION_ID");
        assert_eq!(NORN_AGENT_ID, "NORN_AGENT_ID");
        assert_eq!(NORN_PROFILE, "NORN_PROFILE");
        assert_eq!(NORN_HOOK_EVENT, "NORN_HOOK_EVENT");
        for name in [
            NORN_PROJECT_DIR,
            NORN_SESSION_ID,
            NORN_AGENT_ID,
            NORN_PROFILE,
            NORN_HOOK_EVENT,
        ] {
            assert!(!name.is_empty(), "env var name must not be empty");
        }
    }

    #[test]
    fn common_fields_serialize_to_flat_snake_case_object() {
        let value = serde_json::to_value(common_only()).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(obj.get("session_id"), Some(&serde_json::json!("sess-1")));
        assert_eq!(obj.get("cwd"), Some(&serde_json::json!("/tmp/work")));
        assert_eq!(
            obj.get("hook_event_name"),
            Some(&serde_json::json!("pre_tool"))
        );
        assert_eq!(obj.get("agent_id"), Some(&serde_json::json!("agent-1")));
        assert_eq!(obj.get("profile_name"), Some(&serde_json::json!("")));
    }

    #[test]
    fn options_absent_when_none() {
        let value = serde_json::to_value(common_only()).unwrap();
        let keys = keys_of(&value);
        for key in [
            "tool_name",
            "tool_input",
            "tool_call_id",
            "tool_output",
            "tool_duration_ms",
            "tool_is_error",
            "model",
            "message_count",
            "final_text",
            "subagent_id",
            "subagent_type",
        ] {
            assert!(
                !keys.contains(&key.to_string()),
                "unexpected key {key} present when option was None"
            );
        }
    }

    #[test]
    fn tool_event_fields_serialize_alongside_common_fields() {
        let mut input = common_only();
        input.tool_name = Some("Write".to_string());
        input.tool_input = Some(serde_json::json!({"path": "foo.txt"}));
        input.tool_call_id = Some("call-42".to_string());

        let value = serde_json::to_value(&input).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(obj.get("tool_name"), Some(&serde_json::json!("Write")));
        assert_eq!(
            obj.get("tool_input"),
            Some(&serde_json::json!({"path": "foo.txt"}))
        );
        assert_eq!(obj.get("tool_call_id"), Some(&serde_json::json!("call-42")));
        // Common fields still present at the top level (flat, not nested).
        assert!(obj.contains_key("session_id"));
        assert!(obj.contains_key("hook_event_name"));
        // Post-tool / LLM / stop / subagent fields absent.
        for absent in [
            "tool_output",
            "tool_duration_ms",
            "tool_is_error",
            "model",
            "message_count",
            "final_text",
            "subagent_id",
            "subagent_type",
        ] {
            assert!(
                !obj.contains_key(absent),
                "unexpected key {absent} for tool-event input"
            );
        }
    }

    #[test]
    fn post_tool_event_adds_output_fields() {
        let mut input = common_only();
        input.hook_event_name = "post_tool".to_string();
        input.tool_name = Some("Write".to_string());
        input.tool_input = Some(serde_json::json!({"path": "foo.txt"}));
        input.tool_call_id = Some("call-42".to_string());
        input.tool_output = Some(serde_json::json!({"bytes_written": 17}));
        input.tool_duration_ms = Some(125);
        input.tool_is_error = Some(false);

        let value = serde_json::to_value(&input).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(
            obj.get("tool_output"),
            Some(&serde_json::json!({"bytes_written": 17}))
        );
        assert_eq!(obj.get("tool_duration_ms"), Some(&serde_json::json!(125)));
        assert_eq!(obj.get("tool_is_error"), Some(&serde_json::json!(false)));
        // Tool input fields still present.
        assert!(obj.contains_key("tool_name"));
        assert!(obj.contains_key("tool_input"));
        assert!(obj.contains_key("tool_call_id"));
    }

    #[test]
    fn llm_event_includes_model_and_message_count() {
        let mut input = common_only();
        input.hook_event_name = "pre_llm".to_string();
        input.model = Some("claude-opus-4-7".to_string());
        input.message_count = Some(7);

        let value = serde_json::to_value(&input).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(
            obj.get("model"),
            Some(&serde_json::json!("claude-opus-4-7"))
        );
        assert_eq!(obj.get("message_count"), Some(&serde_json::json!(7)));
        for absent in ["tool_name", "final_text", "subagent_id"] {
            assert!(!obj.contains_key(absent));
        }
    }

    #[test]
    fn stop_event_includes_final_text() {
        let mut input = common_only();
        input.hook_event_name = "stop".to_string();
        input.final_text = Some("done.".to_string());

        let value = serde_json::to_value(&input).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(obj.get("final_text"), Some(&serde_json::json!("done.")));
        for absent in ["tool_name", "model", "subagent_id"] {
            assert!(!obj.contains_key(absent));
        }
    }

    #[test]
    fn subagent_event_includes_id_and_type() {
        let mut input = common_only();
        input.hook_event_name = "subagent_start".to_string();
        input.subagent_id = Some("child-1".to_string());
        input.subagent_type = Some("planner".to_string());

        let value = serde_json::to_value(&input).unwrap();
        let obj = value.as_object().expect("flat JSON object");
        assert_eq!(obj.get("subagent_id"), Some(&serde_json::json!("child-1")));
        assert_eq!(
            obj.get("subagent_type"),
            Some(&serde_json::json!("planner"))
        );
        for absent in ["tool_name", "model", "final_text"] {
            assert!(!obj.contains_key(absent));
        }
    }
}
