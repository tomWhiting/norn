//! System prompt builder.
//!
//! [`build_system_prompt`] takes runtime state (tool registry, configured
//! schemas, execution mode) and produces the composed Norn base system
//! prompt. The builder is called once at startup and again whenever the
//! tool registry changes mid-session.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::tool::traits::ToolCategory;

use super::sections;

/// Execution mode determines mode-dependent prompt sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Interactive REPL session with a human operator.
    Interactive,
    /// Headless print-mode execution (single step, no questions).
    Headless,
}

/// Collaboration mode determines how the agent interacts with the
/// operator and approaches its work. Changeable mid-session via
/// [`LoopContext`](crate::agent_loop::loop_context::LoopContext).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollaborationMode {
    /// Balanced: prefer assumptions over questions, execute readily.
    #[default]
    Default,
    /// Explore, research, and design — but do not mutate files.
    Plan,
    /// Complete the task end-to-end without stopping for questions.
    Autonomous,
}

impl CollaborationMode {
    /// Format the collaboration mode as a dynamic system prompt section.
    #[must_use]
    pub fn format_section(self) -> String {
        let content = match self {
            Self::Default => sections::COLLABORATION_DEFAULT,
            Self::Plan => sections::COLLABORATION_PLAN,
            Self::Autonomous => sections::COLLABORATION_AUTONOMOUS,
        };
        let mut section = String::with_capacity(content.len() + 24);
        section.push_str("# Collaboration Mode\n\n");
        section.push_str(content);
        section
    }
}

/// Metadata about a single tool, extracted from the registry for prompt
/// generation. Avoids coupling the builder to the full [`Tool`](crate::tool::traits::Tool) trait.
#[derive(Clone, Debug)]
pub struct ToolPromptEntry {
    /// Tool identifier.
    pub name: String,
    /// Grouping category.
    pub category: ToolCategory,
    /// Short description (the API-level one-liner).
    pub description: String,
    /// Extended usage guidance for the system prompt.
    pub usage_guidance: Option<String>,
}

/// All inputs the builder needs to assemble the system prompt.
#[derive(Clone, Debug)]
pub struct SystemPromptInputs {
    /// Interactive or headless execution.
    pub mode: ExecutionMode,
    /// Available tools with their prompt metadata.
    pub tools: Vec<ToolPromptEntry>,
    /// Whether an output schema is configured for the final response.
    pub has_output_schema: bool,
    /// Per-event schema descriptions: `(event_type_label, schema_json)`.
    pub event_schema_descriptions: Vec<(String, String)>,
    /// Whether a rules engine is present on the loop context.
    pub has_rules_engine: bool,
    /// Whether auto-compaction is enabled.
    pub has_auto_compact: bool,
}

/// Assemble the Norn base system prompt from runtime state.
///
/// The returned string is intended to become `system_sections[0]` in the
/// [`LoopContext`](crate::agent_loop::loop_context::LoopContext), with
/// profile instructions appended as subsequent sections.
pub fn build_system_prompt(inputs: &SystemPromptInputs) -> String {
    let mut out = String::with_capacity(4096);

    write_identity(&mut out, inputs.mode);
    write_harness_capabilities(&mut out, inputs);
    write_tools_section(&mut out, &inputs.tools);
    write_safety(&mut out);
    write_agent_coordination(&mut out, &inputs.tools);
    write_communication(&mut out, inputs.mode);

    out
}

fn write_identity(out: &mut String, mode: ExecutionMode) {
    out.push_str(sections::IDENTITY);
    out.push(' ');
    match mode {
        ExecutionMode::Interactive => out.push_str(sections::IDENTITY_INTERACTIVE),
        ExecutionMode::Headless => out.push_str(sections::IDENTITY_HEADLESS),
    }
}

fn write_harness_capabilities(out: &mut String, inputs: &SystemPromptInputs) {
    out.push_str("\n\n# Norn Runtime");

    out.push_str("\n\n");
    out.push_str(sections::HARNESS_TOOL_LIFECYCLE);

    out.push_str("\n\n");
    out.push_str(sections::HARNESS_SESSION_CONTEXT);

    let has_schemas = inputs.has_output_schema || !inputs.event_schema_descriptions.is_empty();
    if has_schemas {
        out.push_str("\n\n");
        out.push_str(sections::HARNESS_SCHEMA_ENFORCEMENT);
    }

    for (event_type, schema_json) in &inputs.event_schema_descriptions {
        let _ = write!(
            out,
            "\n\nWhen producing a {event_type}, your output must conform to this schema:\n```json\n{schema_json}\n```",
        );
    }

    if inputs.has_rules_engine {
        out.push_str("\n\n");
        out.push_str(sections::HARNESS_RULES);
    }

    if inputs.has_auto_compact {
        out.push_str("\n\n");
        out.push_str(sections::HARNESS_AUTO_COMPACT);
    }
}

fn write_tools_section(out: &mut String, tools: &[ToolPromptEntry]) {
    if tools.is_empty() {
        return;
    }

    let _ = write!(
        out,
        "\n\n# Tools\n\nYou have access to {} tools. \
        Call tools to accomplish tasks. You can call multiple tools in \
        parallel when they are independent.",
        tools.len()
    );

    out.push_str("\n\n");
    out.push_str(sections::TOOL_ENVELOPE_GUIDANCE);

    let grouped = group_by_category(tools);
    for (category, entries) in &grouped {
        let _ = write!(out, "\n\n## {}", category_heading(*category));
        for entry in entries {
            out.push_str("\n\n**");
            out.push_str(&entry.name);
            out.push_str("**: ");
            out.push_str(&entry.description);
            if let Some(guidance) = &entry.usage_guidance {
                out.push(' ');
                out.push_str(guidance);
            }
        }
    }
}

fn write_safety(out: &mut String) {
    out.push_str("\n\n# Safety\n\n");
    out.push_str(sections::SAFETY);
}

fn write_agent_coordination(out: &mut String, tools: &[ToolPromptEntry]) {
    let has_agent_tools = tools.iter().any(|t| t.category == ToolCategory::Agent);
    if !has_agent_tools {
        return;
    }
    out.push_str("\n\n# Agent Coordination\n\n");
    out.push_str(sections::AGENT_COORDINATION);
}

fn write_communication(out: &mut String, mode: ExecutionMode) {
    out.push_str("\n\n# Communication\n\n");
    match mode {
        ExecutionMode::Interactive => out.push_str(sections::COMMUNICATION_INTERACTIVE),
        ExecutionMode::Headless => out.push_str(sections::COMMUNICATION_HEADLESS),
    }
}

/// Groups tools by category in rendering order. Distinct
/// [`ToolCategory::Custom`] labels form distinct groups: the grouping key
/// is the ordinal *plus* the label, so two custom categories never merge
/// even though they share an ordinal slot (labels order alphabetically
/// within it).
fn group_by_category(tools: &[ToolPromptEntry]) -> Vec<(ToolCategory, Vec<&ToolPromptEntry>)> {
    let mut map: BTreeMap<(u8, &str), (ToolCategory, Vec<&ToolPromptEntry>)> = BTreeMap::new();
    for entry in tools {
        let key = (
            category_sort_key(entry.category),
            custom_label(entry.category),
        );
        map.entry(key)
            .or_insert_with(|| (entry.category, Vec::new()))
            .1
            .push(entry);
    }
    map.into_values().collect()
}

/// The label of a [`ToolCategory::Custom`] category; empty for built-ins,
/// whose ordinal alone determines their position.
fn custom_label(cat: ToolCategory) -> &'static str {
    match cat {
        ToolCategory::Custom(label) => label,
        _ => "",
    }
}

fn category_sort_key(cat: ToolCategory) -> u8 {
    match cat {
        ToolCategory::FileSystem => 0,
        ToolCategory::Search => 1,
        ToolCategory::Shell => 2,
        ToolCategory::Development => 3,
        ToolCategory::Custom(_) => 4,
        ToolCategory::Web => 5,
        ToolCategory::Agent => 6,
        ToolCategory::Scripting => 7,
        ToolCategory::TaskManagement => 8,
        ToolCategory::Discovery => 9,
        ToolCategory::Skills => 10,
        ToolCategory::General => 11,
    }
}

fn category_heading(cat: ToolCategory) -> &'static str {
    match cat {
        ToolCategory::FileSystem => "File Operations",
        ToolCategory::Search => "Search",
        ToolCategory::Shell => "Shell",
        ToolCategory::Development => "Development",
        ToolCategory::Custom(label) => label,
        ToolCategory::Web => "Web",
        ToolCategory::Agent => "Agent Tools",
        ToolCategory::Scripting => "Scripting",
        ToolCategory::TaskManagement => "Task Management",
        ToolCategory::Discovery => "Tool Discovery",
        ToolCategory::Skills => "Skills",
        ToolCategory::General => "Other Tools",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn entry(name: &str, cat: ToolCategory, desc: &str, guidance: Option<&str>) -> ToolPromptEntry {
        ToolPromptEntry {
            name: name.to_owned(),
            category: cat,
            description: desc.to_owned(),
            usage_guidance: guidance.map(str::to_owned),
        }
    }

    fn minimal_inputs() -> SystemPromptInputs {
        SystemPromptInputs {
            mode: ExecutionMode::Interactive,
            tools: vec![],
            has_output_schema: false,
            event_schema_descriptions: vec![],
            has_rules_engine: false,
            has_auto_compact: false,
        }
    }

    #[test]
    fn identity_varies_by_mode() {
        let interactive = build_system_prompt(&minimal_inputs());
        assert!(interactive.contains(sections::IDENTITY_INTERACTIVE));
        assert!(!interactive.contains(sections::IDENTITY_HEADLESS));

        let mut headless_inputs = minimal_inputs();
        headless_inputs.mode = ExecutionMode::Headless;
        let headless = build_system_prompt(&headless_inputs);
        assert!(headless.contains(sections::IDENTITY_HEADLESS));
        assert!(!headless.contains(sections::IDENTITY_INTERACTIVE));
    }

    #[test]
    fn schema_section_omitted_when_no_schemas() {
        let prompt = build_system_prompt(&minimal_inputs());
        assert!(!prompt.contains("schema validation"));
    }

    #[test]
    fn schema_section_included_when_output_schema_set() {
        let mut inputs = minimal_inputs();
        inputs.has_output_schema = true;
        let prompt = build_system_prompt(&inputs);
        assert!(prompt.contains("conform to the provided schema"));
    }

    #[test]
    fn event_schemas_rendered_inline() {
        let mut inputs = minimal_inputs();
        inputs.event_schema_descriptions = vec![(
            "spoken response".to_owned(),
            r#"{"type":"object","properties":{"text":{"type":"string"}}}"#.to_owned(),
        )];
        let prompt = build_system_prompt(&inputs);
        assert!(prompt.contains("spoken response"));
        assert!(prompt.contains(r#""type":"object""#));
    }

    #[test]
    fn rules_section_conditional() {
        let without = build_system_prompt(&minimal_inputs());
        assert!(!without.contains("inject contextual guidance"));

        let mut with = minimal_inputs();
        with.has_rules_engine = true;
        let prompt = build_system_prompt(&with);
        assert!(prompt.contains("inject contextual guidance"));
    }

    #[test]
    fn auto_compact_section_conditional() {
        let without = build_system_prompt(&minimal_inputs());
        assert!(!without.contains("automatically summarised"));

        let mut with = minimal_inputs();
        with.has_auto_compact = true;
        let prompt = build_system_prompt(&with);
        assert!(prompt.contains("automatically summarised"));
    }

    #[test]
    fn tools_section_groups_by_category() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![
            entry("read", ToolCategory::FileSystem, "Read a file.", None),
            entry("search", ToolCategory::Search, "Search files.", None),
            entry("bash", ToolCategory::Shell, "Run a command.", None),
            entry(
                "write",
                ToolCategory::FileSystem,
                "Write a file.",
                Some("Use for new files."),
            ),
        ];
        let prompt = build_system_prompt(&inputs);

        assert!(prompt.contains("# Tools"));
        assert!(prompt.contains("4 tools"));
        assert!(prompt.contains("## File Operations"));
        assert!(prompt.contains("## Search"));
        assert!(prompt.contains("## Shell"));
        assert!(prompt.contains("**read**: Read a file."));
        assert!(prompt.contains("**write**: Write a file. Use for new files."));
    }

    #[test]
    fn empty_tools_omits_section() {
        let prompt = build_system_prompt(&minimal_inputs());
        assert!(!prompt.contains("# Tools"));
    }

    #[test]
    fn custom_category_tools_group_under_their_label_heading() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![
            entry("read", ToolCategory::FileSystem, "Read a file.", None),
            entry(
                "meridian_messaging",
                ToolCategory::Custom("Meridian"),
                "Send and read Meridian DMs and channels.",
                None,
            ),
        ];
        let prompt = build_system_prompt(&inputs);

        assert!(prompt.contains("## Meridian"));
        assert!(
            prompt.contains("**meridian_messaging**: Send and read Meridian DMs and channels.")
        );
    }

    #[test]
    fn distinct_custom_labels_form_distinct_ordered_groups() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![
            entry("zeta_tool", ToolCategory::Custom("Zeta"), "Zeta op.", None),
            entry("acme_tool", ToolCategory::Custom("Acme"), "Acme op.", None),
        ];
        let prompt = build_system_prompt(&inputs);

        let acme_pos = prompt.find("## Acme").expect("Acme heading present");
        let zeta_pos = prompt.find("## Zeta").expect("Zeta heading present");
        assert!(
            acme_pos < zeta_pos,
            "custom labels must order alphabetically within their slot",
        );
        assert!(prompt.contains("**acme_tool**: Acme op."));
        assert!(prompt.contains("**zeta_tool**: Zeta op."));
    }

    #[test]
    fn custom_category_slots_between_development_and_web() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![
            entry("web_fetch", ToolCategory::Web, "Fetch.", None),
            entry("prod_tool", ToolCategory::Custom("Product"), "Op.", None),
            entry("lsp", ToolCategory::Development, "LSP.", None),
        ];
        let prompt = build_system_prompt(&inputs);

        let dev_pos = prompt.find("## Development").expect("dev heading");
        let custom_pos = prompt.find("## Product").expect("custom heading");
        let web_pos = prompt.find("## Web").expect("web heading");
        assert!(dev_pos < custom_pos && custom_pos < web_pos);
    }

    #[test]
    fn custom_heading_absent_without_custom_tools() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![entry(
            "read",
            ToolCategory::FileSystem,
            "Read a file.",
            None,
        )];
        let prompt = build_system_prompt(&inputs);

        assert!(!prompt.contains("## Meridian"));
    }

    #[test]
    fn communication_varies_by_mode() {
        let interactive = build_system_prompt(&minimal_inputs());
        assert!(interactive.contains("Keep responses concise"));

        let mut headless = minimal_inputs();
        headless.mode = ExecutionMode::Headless;
        let prompt = build_system_prompt(&headless);
        assert!(prompt.contains("Do not ask clarifying questions"));
    }

    #[test]
    fn safety_always_present() {
        let prompt = build_system_prompt(&minimal_inputs());
        assert!(prompt.contains("# Safety"));
        assert!(prompt.contains("reversibility"));
    }

    #[test]
    fn full_prompt_has_expected_section_order() {
        let mut inputs = minimal_inputs();
        inputs.has_output_schema = true;
        inputs.has_rules_engine = true;
        inputs.tools = vec![entry("read", ToolCategory::FileSystem, "Read.", None)];
        let prompt = build_system_prompt(&inputs);

        let identity_pos = prompt.find(sections::IDENTITY).unwrap();
        let runtime_pos = prompt.find("# Norn Runtime").unwrap();
        let tools_pos = prompt.find("# Tools").unwrap();
        let safety_pos = prompt.find("# Safety").unwrap();
        let comm_pos = prompt.find("# Communication").unwrap();

        assert!(identity_pos < runtime_pos);
        assert!(runtime_pos < tools_pos);
        assert!(tools_pos < safety_pos);
        assert!(safety_pos < comm_pos);
    }

    #[test]
    fn full_prompt_with_agent_tools_has_six_section_order() {
        let mut inputs = minimal_inputs();
        inputs.has_output_schema = true;
        inputs.has_rules_engine = true;
        inputs.tools = vec![
            entry("read", ToolCategory::FileSystem, "Read.", None),
            entry("fork", ToolCategory::Agent, "Fork.", None),
        ];
        let prompt = build_system_prompt(&inputs);

        let identity_pos = prompt.find(sections::IDENTITY).unwrap();
        let runtime_pos = prompt.find("# Norn Runtime").unwrap();
        let tools_pos = prompt.find("# Tools").unwrap();
        let safety_pos = prompt.find("# Safety").unwrap();
        let coord_pos = prompt.find("# Agent Coordination").unwrap();
        let comm_pos = prompt.find("# Communication").unwrap();

        assert!(identity_pos < runtime_pos);
        assert!(runtime_pos < tools_pos);
        assert!(tools_pos < safety_pos);
        assert!(safety_pos < coord_pos);
        assert!(coord_pos < comm_pos);
    }

    #[test]
    fn agent_coordination_present_when_agent_tools_registered() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![entry("fork", ToolCategory::Agent, "Fork.", None)];
        let prompt = build_system_prompt(&inputs);
        assert!(prompt.contains("# Agent Coordination"));
        assert!(prompt.contains("fork yourself or spawn sub-agents"));
    }

    #[test]
    fn agent_coordination_absent_when_no_agent_tools() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![entry("read", ToolCategory::FileSystem, "Read.", None)];
        let prompt = build_system_prompt(&inputs);
        assert!(!prompt.contains("# Agent Coordination"));
    }

    #[test]
    fn agent_coordination_between_safety_and_communication() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![
            entry("read", ToolCategory::FileSystem, "Read.", None),
            entry("fork", ToolCategory::Agent, "Fork.", None),
        ];
        let prompt = build_system_prompt(&inputs);
        let safety_pos = prompt.find("# Safety").unwrap();
        let coord_pos = prompt.find("# Agent Coordination").unwrap();
        let comm_pos = prompt.find("# Communication").unwrap();
        assert!(safety_pos < coord_pos);
        assert!(coord_pos < comm_pos);
    }

    #[test]
    fn envelope_guidance_present_when_tools_exist() {
        let mut inputs = minimal_inputs();
        inputs.tools = vec![entry("read", ToolCategory::FileSystem, "Read.", None)];
        let prompt = build_system_prompt(&inputs);
        assert!(prompt.contains("tool_use_description"));
        assert!(prompt.contains("tool_use_metadata"));
    }

    #[test]
    fn collaboration_mode_default_section() {
        let section = CollaborationMode::Default.format_section();
        assert!(section.contains("# Collaboration Mode"));
        assert!(section.contains("reasonable assumptions"));
    }

    #[test]
    fn collaboration_mode_plan_section() {
        let section = CollaborationMode::Plan.format_section();
        assert!(section.contains("# Collaboration Mode"));
        assert!(section.contains("plan mode"));
        assert!(section.contains("Ground in the environment"));
        assert!(section.contains("decision complete"));
    }

    #[test]
    fn collaboration_mode_autonomous_section() {
        let section = CollaborationMode::Autonomous.format_section();
        assert!(section.contains("# Collaboration Mode"));
        assert!(section.contains("autonomous execution mode"));
        assert!(section.contains("Persist until"));
    }

    #[test]
    fn collaboration_mode_defaults_to_default() {
        assert_eq!(CollaborationMode::default(), CollaborationMode::Default);
    }
}
