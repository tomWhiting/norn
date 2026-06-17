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
    HookEntry, HookSettings, LengthOverrideEntry, McpServerSettings, ModelAliasSettings,
    NornSettings, PermissionSettings, ProviderProfileSettings, ProviderSettings, ToolSettings,
    WriteToolSettings,
};

fn ns_model(model: &str) -> NornSettings {
    NornSettings {
        model: Some(model.to_owned()),
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
                timeout: None,
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
                timeout: None,
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
            transport: Some("stdio".to_owned()),
            command: Some("user-bin".to_owned()),
            args: Some(vec!["--user".to_owned()]),
            url: None,
            env: None,
            headers: None,
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
            transport: None,
            command: None,
            args: None,
            url: Some("https://example.com".to_owned()),
            env: None,
            headers: None,
        },
    );
    project_map.insert(
        "s2".to_owned(),
        McpServerSettings {
            transport: Some("stdio".to_owned()),
            command: Some("s2-bin".to_owned()),
            args: None,
            url: None,
            env: None,
            headers: None,
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
