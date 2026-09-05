//! Backend and transition regressions for model policy.

use super::*;
use crate::config::ModelAliasSelection;
use crate::model_catalog::{BackendEntry, ModelCatalog, ModelSelection, ProviderEntry};

fn selection(model: &str, window: Option<u64>) -> Result<ModelRuntime, ConfigError> {
    ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        model,
        window,
        None,
        None,
        BTreeMap::new(),
    )
}

#[test]
fn derived_windows_follow_sol_spark_astra() -> Result<(), ConfigError> {
    let sol = selection("sol", None)?;
    assert_eq!(sol.window(), 272_000);
    assert_eq!(sol.explicit_window(), None);
    let spark = sol.prepare("codex-spark")?;
    assert_eq!(spark.model(), "gpt-5.3-codex-spark");
    assert_eq!(spark.window(), 128_000);
    let astra = spark.prepare("astra")?;
    assert_eq!(astra.model(), "gpt-6-astra");
    assert_eq!(astra.window(), 372_000);
    assert_eq!(astra.explicit_window(), None);
    Ok(())
}

#[test]
fn explicit_default_remains_explicit_and_refuses_spark() -> Result<(), ConfigError> {
    let mut sol = selection("sol", Some(272_000))?;
    sol.set_effort(Some(ReasoningEffort::Ultra))?;
    sol.set_tier(Some(ServiceTier::Fast))?;
    assert!(sol.prepare("codex-spark").is_err());
    assert_eq!(sol.model(), "gpt-5.6-sol");
    assert_eq!(sol.window(), 272_000);
    assert_eq!(sol.explicit_window(), Some(272_000));
    assert_eq!(sol.effort(), Some(ReasoningEffort::Ultra));
    assert_eq!(sol.tier(), Some(ServiceTier::Fast));
    Ok(())
}

#[test]
fn undeclared_backend_never_borrows_codex_metadata() -> Result<(), ConfigError> {
    assert!(resolve_window(Some(CatalogBackend::RESPONSES), "gpt-6-astra", None).is_err());
    assert!(resolve_window(None, "gpt-6-astra", None).is_err());
    let mut api = ModelRuntime::new(
        Some(CatalogBackend::RESPONSES),
        "gpt-6-astra",
        Some(400_000),
        None,
        None,
        BTreeMap::new(),
    )?;
    assert!(api.set_effort(Some(ReasoningEffort::Ultra)).is_err());
    assert!(api.set_tier(Some(ServiceTier::Fast)).is_err());
    assert_eq!(api.window(), 400_000);
    assert!(resolve_window(Some(CatalogBackend::CODEX), "unknown", None).is_err());
    assert_eq!(resolve_window(None, "unknown", Some(400_000))?, 400_000);
    assert!(resolve_window(None, "unknown", Some(0)).is_err());
    Ok(())
}

#[test]
fn aliases_preserve_precedence_and_profile_binding() -> Result<(), ConfigError> {
    let mut aliases = BTreeMap::new();
    aliases.insert(
        "sol".to_owned(),
        ModelAliasSettings::Model("gpt-6-astra".to_owned()),
    );
    aliases.insert(
        "gpt-5.6-sol".to_owned(),
        ModelAliasSettings::Model("gpt-5.3-codex-spark".to_owned()),
    );
    aliases.insert(
        "same".to_owned(),
        ModelAliasSettings::Selection(ModelAliasSelection {
            provider_profile: Some("subscription".to_owned()),
            api_shape: Some("openai_responses".to_owned()),
            model: "gpt-6-astra".to_owned(),
        }),
    );
    aliases.insert(
        "other".to_owned(),
        ModelAliasSettings::Selection(ModelAliasSelection {
            provider_profile: Some("api".to_owned()),
            api_shape: Some("openai_responses".to_owned()),
            model: "gpt-6-astra".to_owned(),
        }),
    );
    let mut sol = ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        "gpt-5.6-sol",
        None,
        None,
        None,
        aliases,
    )?;
    assert_eq!(sol.model(), "gpt-5.6-sol");
    assert_eq!(sol.prepare("sol")?.model(), "gpt-6-astra");
    sol.bind_provider_profile(Some("subscription".to_owned()));
    assert_eq!(sol.prepare("same")?.model(), "gpt-6-astra");
    assert!(sol.prepare("other").is_err());
    assert_eq!(sol.model(), "gpt-5.6-sol");
    Ok(())
}

#[test]
fn publication_updates_budget_and_stamp_after_validation() -> Result<(), ConfigError> {
    let sol = selection("sol", None)?;
    let mut config = crate::agent_loop::config::AgentLoopConfig::default();
    let mut context = crate::agent_loop::loop_context::LoopContext::new("");
    let tools = ToolContext::empty();
    sol.apply(&mut config, &mut context, Some(&tools));
    let spark = sol.prepare("codex-spark")?;
    spark.apply(&mut config, &mut context, Some(&tools));
    assert_eq!(config.context_window_limit, Some(128_000));
    assert_eq!(
        tools.get_extension::<ToolOutputBudget>().as_deref(),
        Some(&ToolOutputBudget::for_context_window(Some(128_000)))
    );
    assert_eq!(
        tools
            .get_extension::<AgentModel>()
            .map(|stamp| stamp.model.clone()),
        Some("gpt-5.3-codex-spark".to_owned())
    );
    assert!(spark.prepare("unknown").is_err());
    assert_eq!(config.context_window_limit, Some(128_000));
    assert_eq!(
        tools.get_extension::<ToolOutputBudget>().as_deref(),
        Some(&ToolOutputBudget::for_context_window(Some(128_000)))
    );
    Ok(())
}

#[test]
fn identical_model_ids_resolve_within_fixture_backend() -> Result<(), Box<dyn std::error::Error>> {
    const FIRST: ModelEntry = ModelEntry {
        id: "same-model",
        alias: "same",
        display_name: "Fixture",
        description: "Fixture policy",
        context_window: 128_000,
        max_context_window: 128_000,
        default_reasoning_effort: "low",
        supported_reasoning_efforts: &["low"],
        default_reasoning_summary: "auto",
        supports_reasoning_summaries: false,
        service_tiers: &[],
        web_search_tool_type: "none",
        input_modalities: &["text"],
        supports_image_detail_original: false,
        supports_search_tool: false,
        supports_parallel_tool_calls: false,
        apply_patch_tool_type: "none",
    };
    const SECOND: ModelEntry = ModelEntry {
        context_window: 272_000,
        max_context_window: 872_000,
        supported_reasoning_efforts: &["ultra"],
        ..FIRST
    };
    const FIXTURE: ModelCatalog = ModelCatalog {
        schema_version: 1,
        default: ModelSelection {
            provider: "fixture",
            backend: "first",
            model: "same-model",
        },
        providers: &[ProviderEntry {
            id: "fixture",
            display_name: "Fixture",
            backends: &[
                BackendEntry {
                    id: "first",
                    display_name: "First",
                    auth: "test",
                    api_surface: "test",
                    models: &[FIRST],
                },
                BackendEntry {
                    id: "second",
                    display_name: "Second",
                    auth: "test",
                    api_surface: "test",
                    models: &[SECOND],
                },
            ],
        }],
    };
    let first = CatalogBackend {
        provider: "fixture",
        backend: "first",
    }
    .model_in(&FIXTURE, "same-model");
    let second = CatalogBackend {
        provider: "fixture",
        backend: "second",
    }
    .model_in(&FIXTURE, "same-model");
    assert_eq!(resolve_entry_window("same-model", first, None)?, 128_000);
    assert_eq!(resolve_entry_window("same-model", second, None)?, 272_000);
    assert!(resolve_entry_window("same-model", first, Some(272_000)).is_err());
    assert_eq!(
        resolve_entry_window("same-model", second, Some(272_000))?,
        272_000
    );
    assert!(first.is_some_and(|entry| entry.supported_reasoning_efforts == ["low"]));
    assert!(second.is_some_and(|entry| entry.supported_reasoning_efforts == ["ultra"]));
    Ok(())
}

#[test]
fn user_alias_target_uses_catalogue_alias_without_following_user_alias_again()
-> Result<(), ConfigError> {
    let aliases = BTreeMap::from([
        (
            "fast".to_owned(),
            ModelAliasSettings::Model("codex-spark".to_owned()),
        ),
        (
            "codex-spark".to_owned(),
            ModelAliasSettings::Model("gpt-6-astra".to_owned()),
        ),
        (
            "work".to_owned(),
            ModelAliasSettings::Model("local-v1".to_owned()),
        ),
        (
            "local-v1".to_owned(),
            ModelAliasSettings::Model("local-v2".to_owned()),
        ),
    ]);
    let selected = ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        "fast",
        None,
        None,
        None,
        aliases,
    )?;
    assert_eq!(selected.model(), "gpt-5.3-codex-spark");
    assert_eq!(selected.prepare("fast")?.model(), "gpt-5.3-codex-spark");
    assert_eq!(selected.prepare("codex-spark")?.model(), "gpt-6-astra");
    assert_eq!(resolve_alias("work", &selected.aliases).model, "local-v1");
    Ok(())
}

#[test]
fn explicit_policy_refusals_distinguish_missing_metadata_from_declared_values()
-> Result<(), Box<dyn std::error::Error>> {
    let mut luna = selection("luna", None)?;
    let Err(error) = luna.set_effort(Some(ReasoningEffort::Ultra)) else {
        return Err("Luna must refuse undeclared ultra effort".into());
    };
    let message = error.to_string();
    assert!(message.contains("reasoning effort 'ultra' is not supported"));
    assert!(message.contains("gpt-5.6-luna"));
    assert!(message.contains("openai.codex_subscription"));
    assert!(message.contains("declared values: low, medium, high, xhigh, max"));
    assert!(!message.contains("no capability metadata"));
    let mut mini = selection("gpt-5.4-mini", None)?;
    let Err(error) = mini.set_tier(Some(ServiceTier::Fast)) else {
        return Err("Mini must refuse an undeclared tier".into());
    };
    assert!(
        error
            .to_string()
            .contains("declared values: no values declared")
    );

    for (backend, route) in [
        (Some(CatalogBackend::RESPONSES), "openai.responses_api"),
        (Some(CatalogBackend::CHAT), "openai.openai_compatible_chat"),
        (None, "provider without a model catalogue"),
    ] {
        let mut selected = ModelRuntime::new(
            backend,
            "gpt-5.5",
            Some(272_000),
            None,
            None,
            BTreeMap::new(),
        )?;
        for result in [
            selected.set_effort(Some(ReasoningEffort::Medium)),
            selected.set_tier(Some(ServiceTier::Fast)),
        ] {
            let Err(error) = result else {
                return Err(
                    "a route without metadata must refuse explicit capability settings".into(),
                );
            };
            let message = error.to_string();
            assert!(message.contains(route));
            assert!(message.contains("declares no capability metadata for model 'gpt-5.5'"));
            assert!(message.contains("until metadata for this route and model is added"));
            assert!(!message.contains("is not supported"));
        }
        assert_eq!(selected.effort(), None);
        assert_eq!(selected.tier(), None);
    }
    Ok(())
}

#[test]
fn explicit_windows_do_not_admit_whitespace_in_model_identity() -> Result<(), ConfigError> {
    let selected = selection("sol", Some(272_000))?;
    for model in [
        "two models",
        "model\nname",
        "model\tname",
        "model\u{2003}name",
    ] {
        let resolved = ModelRuntime::from_input(
            Some(CatalogBackend::CODEX),
            ModelInput::Resolved(model.to_owned()),
            Some(272_000),
            None,
            None,
            BTreeMap::new(),
        );
        assert!(resolved.is_err());
        assert!(selected.prepare(model).is_err());
        let alias = BTreeMap::from([(
            "bad".to_owned(),
            ModelAliasSettings::Model(model.to_owned()),
        )]);
        assert!(
            ModelRuntime::new(
                Some(CatalogBackend::CODEX),
                "bad",
                Some(272_000),
                None,
                None,
                alias
            )
            .is_err()
        );
    }
    assert_eq!(selected.model(), "gpt-5.6-sol");
    assert_eq!(selected.window(), 272_000);
    Ok(())
}

#[test]
fn startup_alias_profile_refusal_explains_provider_binding_without_claiming_live_switch()
-> Result<(), Box<dyn std::error::Error>> {
    let aliases = BTreeMap::from([(
        "remote".to_owned(),
        ModelAliasSettings::Selection(ModelAliasSelection {
            provider_profile: Some("remote-profile".to_owned()),
            api_shape: None,
            model: "gpt-5.6-sol".to_owned(),
        }),
    )]);
    let Err(error) = ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        "remote",
        None,
        None,
        None,
        aliases,
    ) else {
        return Err("a raw alias must not silently replace the provider".into());
    };
    let message = error.to_string();
    assert!(message.contains("remote-profile"));
    assert!(message.contains("before building the agent"));
    assert!(!message.contains("live model switch"));
    Ok(())
}

#[test]
fn model_switch_cannot_change_compaction_state_under_an_existing_stable_prompt()
-> Result<(), Box<dyn std::error::Error>> {
    let mut sol = selection("sol", None)?;
    sol.bind_compaction_reserve(Some(150_000));
    let Err(error) = sol.prepare("codex-spark") else {
        return Err(
            "a live switch must not disable compaction promised by the stable prompt".into(),
        );
    };
    let message = error.to_string();
    assert!(message.contains("would disable automatic compaction"));
    assert!(message.contains("auto_compact_reserve_tokens=150000"));
    assert!(message.contains("context window 128000"));
    assert_eq!(sol.model(), "gpt-5.6-sol");
    assert_eq!(sol.window(), 272_000);
    assert_eq!(sol.prepare("astra")?.model(), "gpt-6-astra");

    let mut spark = selection("codex-spark", None)?;
    spark.bind_compaction_reserve(Some(150_000));
    let Err(error) = spark.prepare("sol") else {
        return Err(
            "a live switch must not enable compaction omitted from the stable prompt".into(),
        );
    };
    assert!(
        error
            .to_string()
            .contains("would enable automatic compaction")
    );
    assert_eq!(spark.window(), 128_000);
    spark.bind_compaction_reserve(None);
    assert_eq!(spark.prepare("sol")?.window(), 272_000);

    sol.bind_compaction_reserve(Some(30_000));
    assert_eq!(sol.prepare("codex-spark")?.window(), 128_000);
    Ok(())
}

#[test]
fn raw_model_tokens_trim_padding_but_resolved_ids_and_alias_targets_are_exact()
-> Result<(), ConfigError> {
    let aliases = BTreeMap::from([
        (
            "fast".to_owned(),
            ModelAliasSettings::Model("codex-spark".to_owned()),
        ),
        (
            "padded-target".to_owned(),
            ModelAliasSettings::Model(" gpt-5.6-sol ".to_owned()),
        ),
    ]);
    for (raw, canonical) in [
        ("  gpt-5.6-sol ", "gpt-5.6-sol"),
        ("\t sol\n", "gpt-5.6-sol"),
        (" fast ", "gpt-5.3-codex-spark"),
    ] {
        let selected = ModelRuntime::new(
            Some(CatalogBackend::CODEX),
            raw,
            None,
            None,
            None,
            aliases.clone(),
        )?;
        assert_eq!(selected.model(), canonical);
        assert_eq!(selected.prepare(raw)?.model(), canonical);
        assert!(
            ModelRuntime::from_input(
                Some(CatalogBackend::CODEX),
                ModelInput::Resolved(raw.to_owned()),
                Some(128_000),
                None,
                None,
                aliases.clone(),
            )
            .is_err()
        );
    }
    assert!(
        ModelRuntime::new(
            Some(CatalogBackend::CODEX),
            "padded-target",
            Some(272_000),
            None,
            None,
            aliases,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn window_refusals_render_one_config_wrapper_and_preserve_route_reason()
-> Result<(), Box<dyn std::error::Error>> {
    for (backend, window, route, reason) in [
        (
            CatalogBackend::RESPONSES,
            None,
            "openai.responses_api",
            "declares no capability metadata",
        ),
        (
            CatalogBackend::CODEX,
            Some(0),
            "openai.codex_subscription",
            "must be greater than zero",
        ),
        (
            CatalogBackend::CODEX,
            Some(872_001),
            "openai.codex_subscription",
            "exceeds model 'gpt-6-astra's maximum of 872000",
        ),
    ] {
        let Err(error) = resolve_window(Some(backend), "gpt-6-astra", window) else {
            return Err("an invalid window must be refused".into());
        };
        let message = error.to_string();
        assert_eq!(message.matches("invalid config:").count(), 1, "{message}");
        assert_eq!(message.matches(route).count(), 1, "{message}");
        assert!(message.contains(reason), "{message}");
        if window.is_none() {
            assert!(
                message.contains("set agent.context_window (-c context_window=<tokens>)"),
                "{message}"
            );
        }
    }
    Ok(())
}
