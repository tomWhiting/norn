//! Unit tests for [`crate::config::merge`], exercising the public
//! [`merge_settings`] entry point across all per-section merge semantics.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::uninlined_format_args
)]

use std::collections::BTreeMap;

use super::merge_settings;
use crate::config::types::{
    AgentSettings, HookEntry, HookSettings, LengthOverrideEntry, McpServerSettings,
    ModelAliasSettings, NornSettings, PermissionSettings, ProviderAuthMode,
    ProviderProfileSettings, ProviderSettings, SkillToolSettings, ToolSettings, VariantSettings,
    WriteToolSettings,
};

fn ns_model(model: &str) -> NornSettings {
    NornSettings {
        model: Some(model.to_owned()),
        ..NornSettings::default()
    }
}

fn ns_index_lock_deadline(ms: u64) -> NornSettings {
    NornSettings {
        agent: Some(AgentSettings {
            index_lock_deadline_ms: Some(ms),
            ..AgentSettings::default()
        }),
        ..NornSettings::default()
    }
}

fn ns_provider(auth: Option<ProviderAuthMode>, api_key_env: Option<&str>) -> NornSettings {
    NornSettings {
        provider: Some(ProviderSettings {
            auth,
            api_key_env: api_key_env.map(str::to_owned),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    }
}

#[test]
fn scalar_local_wins_over_project_wins_over_user() {
    let mut user = ns_model("z");
    let mut project = ns_model("y");
    let mut local = ns_model("x");
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(merged.model.as_deref(), Some("x"));
}

#[test]
fn scalar_none_at_higher_layer_preserves_lower() {
    let mut user = ns_model("x");
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(merged.model.as_deref(), Some("x"));
}

#[test]
fn scalar_project_wins_when_local_is_none() {
    let mut user = ns_model("x");
    let mut project = ns_model("y");
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(merged.model.as_deref(), Some("y"));
}

/// Review A3 (2026-07-06): `agent.index_lock_deadline_ms` rides the
/// scalar-section merge like every other `[agent]` scalar — local beats
/// project beats user. Deleting its `pick_scalar` arm in
/// `merge_agent` fails this fence.
#[test]
fn agent_index_lock_deadline_local_beats_project_beats_user() {
    let mut user = ns_index_lock_deadline(1_000);
    let mut project = ns_index_lock_deadline(2_000);
    let mut local = ns_index_lock_deadline(3_000);
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(
        merged.agent.unwrap().index_lock_deadline_ms,
        Some(3_000),
        "local tier wins",
    );
}

#[test]
fn agent_index_lock_deadline_project_beats_user_when_local_unset() {
    let mut user = ns_index_lock_deadline(1_000);
    let mut project = ns_index_lock_deadline(2_000);
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(
        merged.agent.unwrap().index_lock_deadline_ms,
        Some(2_000),
        "project tier wins over user when local has no value",
    );
}

#[test]
fn agent_index_lock_deadline_user_tier_only_value_survives_merge() {
    let mut user = ns_index_lock_deadline(1_234);
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(
        merged.agent.unwrap().index_lock_deadline_ms,
        Some(1_234),
        "a user-tier-only value must not be dropped by the merge",
    );
}

#[test]
fn cli_layer_outranks_all_others() {
    let mut user = ns_model("z");
    let mut project = ns_model("y");
    let mut local = ns_model("x");
    let mut cli = ns_model("cli");
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert_eq!(merged.model.as_deref(), Some("cli"));
}

#[test]
fn higher_provider_oauth_clears_inherited_api_key_source() {
    let mut user = ns_provider(None, Some("USER_API_KEY"));
    let mut project = NornSettings::default();
    let mut local = ns_provider(Some(ProviderAuthMode::OAuth), None);
    let mut cli = NornSettings::default();

    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);

    assert_eq!(
        merged.provider.as_ref().and_then(|provider| provider.auth),
        Some(ProviderAuthMode::OAuth),
    );
    assert_eq!(
        merged
            .provider
            .as_ref()
            .and_then(|provider| provider.api_key_env.as_deref()),
        None,
    );
}

#[test]
fn provider_oauth_keeps_same_or_higher_layer_api_key_source_for_validation() {
    for (mut user, mut local) in [
        (
            NornSettings::default(),
            ns_provider(Some(ProviderAuthMode::OAuth), Some("LOCAL_API_KEY")),
        ),
        (
            ns_provider(Some(ProviderAuthMode::OAuth), None),
            ns_provider(None, Some("LOCAL_API_KEY")),
        ),
    ] {
        let mut project = NornSettings::default();
        let mut cli = NornSettings::default();
        let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);

        assert_eq!(
            merged.provider.as_ref().and_then(|provider| provider.auth),
            Some(ProviderAuthMode::OAuth),
        );
        assert_eq!(
            merged
                .provider
                .as_ref()
                .and_then(|provider| provider.api_key_env.as_deref()),
            Some("LOCAL_API_KEY"),
        );
    }
}

#[test]
fn higher_provider_api_key_mode_retains_best_available_companion() {
    let mut user = ns_provider(None, Some("USER_API_KEY"));
    let mut project = ns_provider(None, Some("PROJECT_API_KEY"));
    let mut local = ns_provider(Some(ProviderAuthMode::ApiKey), None);
    let mut cli = NornSettings::default();

    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);

    assert_eq!(
        merged.provider.as_ref().and_then(|provider| provider.auth),
        Some(ProviderAuthMode::ApiKey),
    );
    assert_eq!(
        merged
            .provider
            .as_ref()
            .and_then(|provider| provider.api_key_env.as_deref()),
        Some("PROJECT_API_KEY"),
    );
}

#[test]
fn provider_api_key_does_not_resurrect_source_cleared_by_intermediate_oauth() {
    let mut user = ns_provider(None, Some("USER_API_KEY"));
    let mut project = ns_provider(Some(ProviderAuthMode::OAuth), None);
    let mut local = ns_provider(Some(ProviderAuthMode::ApiKey), None);
    let mut cli = NornSettings::default();

    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);

    assert_eq!(
        merged.provider.as_ref().and_then(|provider| provider.auth),
        Some(ProviderAuthMode::ApiKey),
    );
    assert_eq!(
        merged
            .provider
            .as_ref()
            .and_then(|provider| provider.api_key_env.as_deref()),
        None,
    );
}

#[test]
fn deny_arrays_union_across_layers() {
    let mut user = NornSettings {
        permissions: Some(PermissionSettings {
            deny: Some(vec!["A".to_owned(), "B".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        permissions: Some(PermissionSettings {
            deny: Some(vec!["C".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings {
        permissions: Some(PermissionSettings {
            // Empty Vec at local must NOT remove entries A/B/C.
            deny: Some(vec![]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let perms = merged.permissions.expect("permissions present");
    let deny = perms.deny.expect("deny present");
    assert_eq!(deny, vec!["A".to_owned(), "B".to_owned(), "C".to_owned()]);
}

#[test]
fn deny_cannot_be_unundone_at_higher_layer() {
    // CO6: there is no syntax for un-denying — the merge contract is
    // additive, and the local layer cannot drop an A from user.deny.
    let mut user = NornSettings {
        permissions: Some(PermissionSettings {
            deny: Some(vec!["A".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings::default();
    let mut local = NornSettings {
        permissions: Some(PermissionSettings {
            deny: Some(vec!["B".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let deny = merged.permissions.unwrap().deny.unwrap();
    assert!(
        deny.contains(&"A".to_owned()),
        "user deny entry must survive"
    );
    assert!(deny.contains(&"B".to_owned()));
}

#[test]
fn allow_dedup_across_layers() {
    let mut user = NornSettings {
        permissions: Some(PermissionSettings {
            allow: Some(vec!["X".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        permissions: Some(PermissionSettings {
            allow: Some(vec!["X".to_owned(), "Y".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let allow = merged.permissions.unwrap().allow.unwrap();
    assert_eq!(allow, vec!["X".to_owned(), "Y".to_owned()]);
}

#[test]
fn ask_dedup_same_as_allow() {
    let mut user = NornSettings {
        permissions: Some(PermissionSettings {
            ask: Some(vec!["A".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        permissions: Some(PermissionSettings {
            ask: Some(vec!["A".to_owned(), "B".to_owned()]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let ask = merged.permissions.unwrap().ask.unwrap();
    assert_eq!(ask, vec!["A".to_owned(), "B".to_owned()]);
}

#[test]
fn hooks_extend_within_event_slot() {
    let mut user = NornSettings {
        hooks: Some(HookSettings {
            pre_tool: Some(vec![HookEntry {
                matcher: Some("write".to_owned()),
                command: "user-hook.sh".to_owned(),
                timeout: 5,
            }]),
            ..HookSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        hooks: Some(HookSettings {
            pre_tool: Some(vec![HookEntry {
                matcher: Some("edit".to_owned()),
                command: "project-hook.sh".to_owned(),
                timeout: 10,
            }]),
            ..HookSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let pre_tool = merged.hooks.unwrap().pre_tool.unwrap();
    assert_eq!(pre_tool.len(), 2);
    assert_eq!(pre_tool[0].command, "user-hook.sh");
    assert_eq!(pre_tool[1].command, "project-hook.sh");
}

#[test]
fn mcp_same_name_project_fully_replaces_user_definition() {
    let mut user_map = BTreeMap::new();
    user_map.insert(
        "s1".to_owned(),
        McpServerSettings {
            enabled: None,
            transport: Some("stdio".to_owned()),
            command: Some("user-bin".to_owned()),
            args: Some(vec!["--user".to_owned()]),
            url: None,
            env: None,
            headers: None,
            max_inbound_message_bytes: Some(1024),
            request_timeout_ms: Some(1000),
        },
    );
    let mut user = NornSettings {
        mcp_servers: Some(user_map),
        ..NornSettings::default()
    };

    let mut project_map = BTreeMap::new();
    project_map.insert(
        "s1".to_owned(),
        McpServerSettings {
            enabled: None,
            transport: None,
            command: None,
            args: None,
            url: Some("https://example.com".to_owned()),
            env: None,
            headers: None,
            max_inbound_message_bytes: Some(2048),
            request_timeout_ms: None,
        },
    );
    project_map.insert(
        "s2".to_owned(),
        McpServerSettings {
            enabled: None,
            transport: Some("stdio".to_owned()),
            command: Some("s2-bin".to_owned()),
            args: None,
            url: None,
            env: None,
            headers: None,
            max_inbound_message_bytes: None,
            request_timeout_ms: None,
        },
    );
    let mut project = NornSettings {
        mcp_servers: Some(project_map),
        ..NornSettings::default()
    };

    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let map = merged.mcp_servers.unwrap();
    let s1 = &map["s1"];
    // Full replacement: transport/command/args from user are gone.
    assert_eq!(s1.transport, None);
    assert_eq!(s1.command, None);
    assert_eq!(s1.args, None);
    assert_eq!(s1.url.as_deref(), Some("https://example.com"));
    let s2 = &map["s2"];
    assert_eq!(s2.command.as_deref(), Some("s2-bin"));
}

#[test]
fn tools_write_deep_merge_preserves_sibling_keys() {
    let mut user = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: Some(500),
                length_overrides: None,
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: None,
                length_overrides: Some(vec![LengthOverrideEntry {
                    pattern: "**/*.md".to_owned(),
                    limit: 2000,
                }]),
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let write = merged.tools.unwrap().write.unwrap();
    assert_eq!(write.max_code_lines, Some(500));
    let overrides = write.length_overrides.unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0].pattern, "**/*.md");
}

#[test]
fn tools_write_same_key_higher_overrides_lower() {
    let mut user = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: Some(500),
                length_overrides: None,
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: Some(800),
                length_overrides: None,
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let write = merged.tools.unwrap().write.unwrap();
    assert_eq!(write.max_code_lines, Some(800));
}

#[test]
fn tools_skill_deep_merge_higher_layer_wins() {
    // `tools.skill.shell_execution` follows the same four-layer scalar
    // precedence as `tools.write` fields: the highest layer that sets
    // the key wins.
    let mut user = NornSettings {
        tools: Some(ToolSettings {
            write: None,
            skill: Some(SkillToolSettings {
                shell_execution: Some(true),
            }),
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        tools: Some(ToolSettings {
            write: None,
            skill: Some(SkillToolSettings {
                shell_execution: Some(false),
            }),
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let skill = merged.tools.unwrap().skill.unwrap();
    assert_eq!(skill.shell_execution, Some(false));
}

#[test]
fn tools_skill_sibling_write_keys_survive_skill_only_layers() {
    // A project layer contributing only `tools.skill` must not clobber
    // the user layer's `tools.write` block — the deep merge preserves
    // sibling sub-sections contributed at different layers.
    let mut user = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: Some(100),
                length_overrides: None,
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        tools: Some(ToolSettings {
            write: None,
            skill: Some(SkillToolSettings {
                shell_execution: Some(false),
            }),
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let tools = merged.tools.unwrap();
    assert_eq!(tools.write.unwrap().max_code_lines, Some(100));
    assert_eq!(tools.skill.unwrap().shell_execution, Some(false));
}

#[test]
fn tools_skill_absent_everywhere_stays_none() {
    // No layer configures `tools.skill` — the merged slot must stay
    // `None` so the runtime defers to the tool's documented default
    // (shell execution enabled).
    let mut user = NornSettings {
        tools: Some(ToolSettings {
            write: Some(WriteToolSettings {
                max_code_lines: Some(100),
                length_overrides: None,
            }),
            skill: None,
            bash: None,
            edit: None,
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let tools = merged.tools.unwrap();
    assert!(tools.skill.is_none());
    assert_eq!(tools.write.unwrap().max_code_lines, Some(100));
}

#[test]
fn sub_struct_some_when_any_layer_some() {
    let mut user = NornSettings {
        provider: Some(ProviderSettings {
            base_url: Some("https://user.example.com".to_owned()),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let provider = merged.provider.unwrap();
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://user.example.com")
    );
    // All other provider fields remain None.
    assert!(provider.timeout.is_none());
}

#[test]
fn provider_rate_and_retry_knobs_merge_per_field() {
    // The three rate/retry duration knobs are independent scalars: each
    // is taken from the highest layer that supplies it, exactly like
    // every other provider field.
    let mut user = NornSettings {
        provider: Some(ProviderSettings {
            rate_limit_interval: Some("30s".to_owned()),
            retry_backoff: Some("250ms".to_owned()),
            retry_after_ceiling: Some("5m".to_owned()),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        provider: Some(ProviderSettings {
            retry_backoff: Some("2s".to_owned()),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings {
        provider: Some(ProviderSettings {
            retry_after_ceiling: Some("90s".to_owned()),
            ..ProviderSettings::default()
        }),
        ..NornSettings::default()
    };
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let provider = merged.provider.expect("provider present");
    assert_eq!(
        provider.rate_limit_interval.as_deref(),
        Some("30s"),
        "only the user layer supplies rate_limit_interval",
    );
    assert_eq!(
        provider.retry_backoff.as_deref(),
        Some("2s"),
        "project layer outranks user for retry_backoff",
    );
    assert_eq!(
        provider.retry_after_ceiling.as_deref(),
        Some("90s"),
        "CLI layer outranks all others for retry_after_ceiling",
    );
}

#[test]
fn model_aliases_merge_by_name_with_higher_layer_replacing() {
    let mut user_aliases = BTreeMap::new();
    user_aliases.insert(
        "55".to_owned(),
        ModelAliasSettings::Model("gpt-5.5".to_owned()),
    );
    user_aliases.insert(
        "spark".to_owned(),
        ModelAliasSettings::Model("gpt-5.3-codex-spark".to_owned()),
    );
    let mut project_aliases = BTreeMap::new();
    project_aliases.insert(
        "55".to_owned(),
        ModelAliasSettings::Model("gpt-5.4".to_owned()),
    );

    let mut user = NornSettings {
        model_aliases: Some(user_aliases),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        model_aliases: Some(project_aliases),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();

    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let aliases = merged.model_aliases.expect("model aliases present");
    assert_eq!(aliases["55"].model(), "gpt-5.4");
    assert_eq!(aliases["spark"].model(), "gpt-5.3-codex-spark");
}

#[test]
fn provider_profiles_merge_by_name_with_higher_layer_replacing() {
    let mut user_profiles = BTreeMap::new();
    user_profiles.insert(
        "local".to_owned(),
        ProviderProfileSettings {
            api_shape: Some("openai_chat_completions".to_owned()),
            provider: ProviderSettings {
                base_url: Some("http://user.example/v1".to_owned()),
                ..ProviderSettings::default()
            },
        },
    );
    user_profiles.insert(
        "codex".to_owned(),
        ProviderProfileSettings {
            api_shape: Some("openai_responses".to_owned()),
            provider: ProviderSettings::default(),
        },
    );
    let mut local_profiles = BTreeMap::new();
    local_profiles.insert(
        "local".to_owned(),
        ProviderProfileSettings {
            api_shape: Some("openai_chat_completions".to_owned()),
            provider: ProviderSettings {
                base_url: Some("http://localhost:1234/v1".to_owned()),
                api_key_env: Some("NORN_OPENAI_COMPAT_API_KEY".to_owned()),
                ..ProviderSettings::default()
            },
        },
    );

    let mut user = NornSettings {
        provider_profiles: Some(user_profiles),
        ..NornSettings::default()
    };
    let mut project = NornSettings::default();
    let mut local = NornSettings {
        provider_profiles: Some(local_profiles),
        ..NornSettings::default()
    };
    let mut cli = NornSettings::default();

    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let profiles = merged.provider_profiles.expect("provider profiles present");
    assert_eq!(
        profiles["local"].provider.base_url.as_deref(),
        Some("http://localhost:1234/v1"),
    );
    assert_eq!(
        profiles["local"].provider.api_key_env.as_deref(),
        Some("NORN_OPENAI_COMPAT_API_KEY"),
    );
    assert!(profiles.contains_key("codex"));
}

#[test]
fn all_none_layers_produce_all_none_result() {
    let mut usr = NornSettings::default();
    let mut prj = NornSettings::default();
    let mut lcl = NornSettings::default();
    let mut ovr = NornSettings::default();
    let merged = merge_settings(&mut usr, &mut prj, &mut lcl, &mut ovr);
    assert!(merged.model.is_none());
    assert!(merged.model_aliases.is_none());
    assert!(merged.provider_profiles.is_none());
    assert!(merged.provider.is_none());
    assert!(merged.permissions.is_none());
    assert!(merged.hooks.is_none());
    assert!(merged.tools.is_none());
    assert!(merged.mcp_servers.is_none());
    assert!(merged.variants.is_none());
}

#[test]
fn empty_some_permissions_still_produces_some_result() {
    // Edge case from R4 acceptance: `local []` is a present but empty
    // contribution. It must not turn the merged result into None.
    let mut usr = NornSettings::default();
    let mut prj = NornSettings::default();
    let mut local = NornSettings {
        permissions: Some(PermissionSettings {
            deny: Some(vec![]),
            ..PermissionSettings::default()
        }),
        ..NornSettings::default()
    };
    let mut ovr = NornSettings::default();
    let merged = merge_settings(&mut usr, &mut prj, &mut local, &mut ovr);
    let perms = merged.permissions.expect("permissions present");
    assert_eq!(perms.deny.as_deref(), Some(&[] as &[String]));
}

// ---------------------------------------------------------------------------
// Agent variants (D3: keyed, wholesale-by-name, later layer wins)
// ---------------------------------------------------------------------------

fn variant_map(entries: &[(&str, VariantSettings)]) -> BTreeMap<String, VariantSettings> {
    entries
        .iter()
        .map(|(name, variant)| ((*name).to_owned(), variant.clone()))
        .collect()
}

#[test]
fn variants_same_name_project_fully_replaces_user_definition() {
    let mut user = NornSettings {
        variants: Some(variant_map(&[(
            "reviewer",
            VariantSettings {
                description: Some("user reviewer".to_owned()),
                prompt: Some("user prompt".to_owned()),
                tools: Some(vec!["read".to_owned()]),
                model: Some("user-model".to_owned()),
                reasoning_effort: Some("low".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        variants: Some(variant_map(&[(
            "reviewer",
            VariantSettings {
                // Only a model — everything else absent. Wholesale replace
                // means the user's prompt/tools/effort do NOT survive.
                model: Some("project-model".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let variants = merged.variants.expect("variants present");
    let reviewer = &variants["reviewer"];
    assert_eq!(reviewer.model.as_deref(), Some("project-model"));
    assert!(
        reviewer.prompt.is_none(),
        "user prompt must be discarded on wholesale replace",
    );
    assert!(reviewer.tools.is_none(), "user tools discarded");
    assert!(reviewer.description.is_none(), "user description discarded");
    assert!(reviewer.reasoning_effort.is_none(), "user effort discarded");
}

#[test]
fn variants_distinct_names_union_across_layers() {
    let mut user = NornSettings {
        variants: Some(variant_map(&[(
            "scout",
            VariantSettings {
                prompt: Some("scout".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let mut project = NornSettings {
        variants: Some(variant_map(&[(
            "auditor",
            VariantSettings {
                prompt: Some("audit".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let variants = merged.variants.expect("variants present");
    assert_eq!(variants.len(), 2, "distinct names union");
    assert_eq!(variants["scout"].prompt.as_deref(), Some("scout"));
    assert_eq!(variants["auditor"].prompt.as_deref(), Some("audit"));
}

#[test]
fn variants_none_everywhere_stays_none() {
    let mut user = NornSettings::default();
    let mut project = NornSettings::default();
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    assert!(merged.variants.is_none());
}

#[test]
fn variants_cli_layer_wins_over_local() {
    let mut user = NornSettings::default();
    let mut project = NornSettings::default();
    let mut local = NornSettings {
        variants: Some(variant_map(&[(
            "scout",
            VariantSettings {
                model: Some("local-model".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let mut cli = NornSettings {
        variants: Some(variant_map(&[(
            "scout",
            VariantSettings {
                model: Some("cli-model".to_owned()),
                ..VariantSettings::default()
            },
        )])),
        ..NornSettings::default()
    };
    let merged = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let variants = merged.variants.expect("variants present");
    assert_eq!(
        variants["scout"].model.as_deref(),
        Some("cli-model"),
        "CLI (highest) layer wins wholesale over local",
    );
}
