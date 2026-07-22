use super::super::*;
use super::cli_from;
use crate::cli::BuildError;
use norn::profile::Profile;
use norn::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};

#[test]
fn model_flag_overrides_profile_model() {
    let cli = cli_from(&["norn", "-m", "gpt-5.5"]);
    let mut profile = Profile {
        model: "gpt-old".to_owned(),
        ..Profile::default()
    };
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.model, "gpt-5.5");
}

#[test]
fn system_prompt_is_preserved_as_an_operator_override() {
    let cli = cli_from(&["norn", "-S", "Be concise"]);
    let mut profile = Profile {
        system_instructions: vec!["old".to_owned(), "more old".to_owned()],
        ..Profile::default()
    };
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(
        profile.system_instructions,
        vec!["old".to_owned(), "more old".to_owned()],
        "profile provenance must not be destroyed by an operator override",
    );
    assert_eq!(applied.system_prompt.as_deref(), Some("Be concise"));
}

#[test]
fn append_system_prompt_is_preserved_as_an_operator_fragment() {
    let cli = cli_from(&["norn", "--append-system-prompt", "Also be clear"]);
    let mut profile = Profile {
        system_instructions: vec!["Be concise".to_owned()],
        ..Profile::default()
    };
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.system_instructions, vec!["Be concise"]);
    assert_eq!(
        applied.append_system_prompt.as_deref(),
        Some("Also be clear")
    );
}

#[test]
fn system_prompt_and_append_remain_distinct_operator_fragments() {
    let cli = cli_from(&[
        "norn",
        "-S",
        "Be concise",
        "--append-system-prompt",
        "Also be clear",
    ]);
    let mut profile = Profile::default();
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert!(profile.system_instructions.is_empty());
    assert_eq!(applied.system_prompt.as_deref(), Some("Be concise"));
    assert_eq!(
        applied.append_system_prompt.as_deref(),
        Some("Also be clear")
    );
}

#[test]
fn allowed_tools_replaces_profile_tools_with_csv() {
    let cli = cli_from(&["norn", "--allowed-tools", "read,edit"]);
    let mut profile = Profile {
        tools: Some(vec!["bash".to_owned()]),
        ..Profile::default()
    };
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(
        profile.tools,
        Some(vec!["read".to_owned(), "edit".to_owned()]),
    );
}

#[test]
fn allowed_tools_trims_whitespace_and_skips_empty() {
    let cli = cli_from(&["norn", "--allowed-tools", " read , , edit "]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(
        profile.tools,
        Some(vec!["read".to_owned(), "edit".to_owned()]),
    );
}

#[test]
fn allowed_tools_glob_pattern_is_hard_error() {
    let cli = cli_from(&["norn", "--allowed-tools", "bash*"]);
    let mut profile = Profile::default();
    let err = apply_cli_profile_overrides(&cli, &mut profile).unwrap_err();
    match err {
        BuildError::Argument(reason) => {
            assert!(reason.contains("--allowed-tools"), "reason: {reason}");
            assert!(reason.contains("bash*"), "reason: {reason}");
            assert!(reason.contains("exact"), "reason: {reason}");
        }
        other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
    }
    // The profile must not have been partially gated.
    assert!(profile.tools.is_none());
}

#[test]
fn disallowed_tools_glob_pattern_is_hard_error() {
    for pattern in ["bash*", "to?l", "tool[ab]", "tool{ab}"] {
        let cli = cli_from(&["norn", "--disallowed-tools", pattern]);
        let mut profile = Profile::default();
        let err = apply_cli_profile_overrides(&cli, &mut profile).unwrap_err();
        match err {
            BuildError::Argument(reason) => {
                assert!(
                    reason.contains("--disallowed-tools"),
                    "pattern {pattern}: reason: {reason}",
                );
                assert!(
                    reason.contains(pattern),
                    "pattern {pattern}: reason: {reason}",
                );
            }
            other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
        }
    }
}

#[test]
fn exact_tool_names_pass_pattern_rejection() {
    let cli = cli_from(&[
        "norn",
        "--allowed-tools",
        "read,write_file,lsp-bridge",
        "--disallowed-tools",
        "bash",
    ]);
    let mut profile = Profile::default();
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(
        applied.allowed_tools,
        vec!["read", "write_file", "lsp-bridge"],
    );
    assert_eq!(applied.disallowed_tools, vec!["bash"]);
}

#[test]
fn allowed_tools_flag_absent_leaves_applied_allowed_empty() {
    let cli = cli_from(&["norn"]);
    let mut profile = Profile {
        tools: Some(vec!["from-profile".to_owned()]),
        ..Profile::default()
    };
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert!(applied.allowed_tools.is_empty());
    // Profile-declared tools stay untouched by the flag plumbing.
    assert_eq!(profile.tools, Some(vec!["from-profile".to_owned()]));
}

#[test]
fn disallowed_tools_returned_in_applied_overrides() {
    let cli = cli_from(&["norn", "--disallowed-tools", "write,edit"]);
    let mut profile = Profile::default();
    let applied = apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(
        applied.disallowed_tools,
        vec!["write".to_owned(), "edit".to_owned()],
    );
    // Profile.tools must NOT have been touched.
    assert!(profile.tools.is_none());
}

#[test]
fn reasoning_effort_high_maps_to_runtime_high() {
    let cli = cli_from(&["norn", "--reasoning-effort", "high"]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn reasoning_effort_low_and_medium_map_correctly() {
    let cli_low = cli_from(&["norn", "--reasoning-effort", "low"]);
    let cli_medium = cli_from(&["norn", "--reasoning-effort", "medium"]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli_low, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Low));
    apply_cli_profile_overrides(&cli_medium, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Medium));
}

#[test]
fn reasoning_effort_xhigh_and_max_map_correctly() {
    let cli_xhigh = cli_from(&["norn", "--reasoning-effort", "xhigh"]);
    let cli_max = cli_from(&["norn", "--reasoning-effort", "max"]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli_xhigh, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::XHigh));
    apply_cli_profile_overrides(&cli_max, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Max));
}

#[test]
fn service_tier_flags_map_to_runtime_fast() {
    let cli = cli_from(&["norn", "--service-tier", "fast"]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.service_tier, Some(ServiceTier::Fast));

    let cli = cli_from(&["norn", "--fast"]);
    let mut profile = Profile::default();
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.service_tier, Some(ServiceTier::Fast));
}

#[test]
fn no_flags_leaves_profile_untouched() {
    let cli = cli_from(&["norn"]);
    let mut profile = Profile {
        model: "kept".to_owned(),
        system_instructions: vec!["kept".to_owned()],
        tools: Some(vec!["kept".to_owned()]),
        ..Profile::default()
    };
    let snapshot = profile.clone();
    apply_cli_profile_overrides(&cli, &mut profile).unwrap();
    assert_eq!(profile.model, snapshot.model);
    assert_eq!(profile.system_instructions, snapshot.system_instructions);
    assert_eq!(profile.tools, snapshot.tools);
}

#[test]
fn settings_reasoning_fills_profile_when_unset() {
    use norn::config::{AgentSettings, NornSettings};
    let mut profile = Profile::default();
    let settings = NornSettings {
        agent: Some(AgentSettings {
            reasoning_effort: Some("low".to_owned()),
            reasoning_summary: Some("detailed".to_owned()),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::Low));
    assert_eq!(profile.reasoning_summary, Some(ReasoningSummary::Detailed));
}

#[test]
fn settings_service_tier_fills_profile_when_unset() {
    use norn::config::{AgentSettings, NornSettings};
    let mut profile = Profile::default();
    let settings = NornSettings {
        agent: Some(AgentSettings {
            service_tier: Some("fast".to_owned()),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
    assert_eq!(profile.service_tier, Some(ServiceTier::Fast));
}

#[test]
fn settings_reasoning_does_not_overwrite_profile() {
    use norn::config::{AgentSettings, NornSettings};
    let mut profile = Profile {
        reasoning_effort: Some(ReasoningEffort::High),
        ..Profile::default()
    };
    let settings = NornSettings {
        agent: Some(AgentSettings {
            reasoning_effort: Some("low".to_owned()),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap();
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn settings_reasoning_rejects_bad_value() {
    use norn::config::{AgentSettings, NornSettings};
    let mut profile = Profile::default();
    let settings = NornSettings {
        agent: Some(AgentSettings {
            reasoning_effort: Some("turbo".to_owned()),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    };
    let err = apply_settings_reasoning_to_profile(&settings, &mut profile).unwrap_err();
    match err {
        BuildError::Argument(reason) => {
            assert!(
                reason.contains("agent.reasoning_effort"),
                "reason: {reason}"
            );
            assert!(reason.contains("turbo"), "reason: {reason}");
        }
        other @ BuildError::Auth(_) => panic!("expected Argument, got {other:?}"),
    }
}
