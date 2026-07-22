use super::super::*;

/// The shared arming installs the estimator and the context-edit
/// tracker on the loop context and fills an unset window from the
/// catalog for the resolved model, leaving the reserve default
/// untouched. This is the exact end state every launch path (root,
/// spawn, fork, rhai) must produce — the single mechanism they all
/// call, so the auto-compaction trigger cannot drift between them.
#[test]
fn arm_auto_compaction_installs_estimator_edits_and_catalog_window() {
    let model = crate::model_catalog::default_selection().model;
    let catalog_window = crate::model_catalog::smallest_context_window_for_model(model);
    assert!(
        catalog_window.is_some(),
        "test precondition: the default model must be catalogued",
    );

    let mut loop_context = LoopContext::new("base");
    let mut config = AgentLoopConfig::default();
    assert!(loop_context.token_estimator.is_none());
    assert!(loop_context.context_edits.is_none());
    assert!(config.context_window_limit.is_none());

    arm_auto_compaction(&mut loop_context, &mut config, model);

    assert!(
        loop_context.token_estimator.is_some(),
        "arming installs the token estimator the preflight needs",
    );
    assert!(
        loop_context.context_edits.is_some(),
        "arming installs the context-edit tracker (floor + compaction commit)",
    );
    assert_eq!(
        config.context_window_limit, catalog_window,
        "an unset window is filled from the catalog for the resolved model",
    );
    assert_eq!(
        config.auto_compact_reserve_tokens,
        Some(30_000),
        "the reserve default flows through untouched by arming",
    );
}

/// An explicit window (settings / `-c` override / any future
/// child-policy field) is authoritative: arming only fills a `None`
/// window, so an explicit value survives even for a catalogued model.
#[test]
fn arm_auto_compaction_explicit_window_beats_catalog() {
    let model = crate::model_catalog::default_selection().model;
    let mut loop_context = LoopContext::new("base");
    let mut config = AgentLoopConfig {
        context_window_limit: Some(12_345),
        ..AgentLoopConfig::default()
    };

    arm_auto_compaction(&mut loop_context, &mut config, model);

    assert_eq!(
        config.context_window_limit,
        Some(12_345),
        "an explicit window must never be overwritten by the catalog value",
    );
    assert!(loop_context.token_estimator.is_some());
    assert!(loop_context.context_edits.is_some());
}

/// A model absent from the catalog keeps a `None` window — the trigger
/// stays disabled (`maybe_auto_compact` returns early on `None`),
/// matching the root behavior, with no error. The estimator and the
/// tracker are still installed (harmless with the trigger off).
#[test]
fn arm_auto_compaction_non_catalog_model_leaves_window_none() {
    let mut loop_context = LoopContext::new("base");
    let mut config = AgentLoopConfig::default();

    arm_auto_compaction(&mut loop_context, &mut config, "not-in-catalog-model-xyz");

    assert_eq!(
        config.context_window_limit, None,
        "a non-catalog model leaves the window None, disabling the trigger",
    );
    assert!(loop_context.token_estimator.is_some());
    assert!(loop_context.context_edits.is_some());
}

/// 2026-07-05 incident repro: a global 272k settings override on
/// gpt-5.3-codex-spark (max 128k) armed every threshold beyond the
/// real wall. Validation must reject it loudly, naming the model and
/// both numbers.
#[test]
fn validate_rejects_explicit_window_above_catalog_max() {
    let config = AgentLoopConfig {
        context_window_limit: Some(272_000),
        ..AgentLoopConfig::default()
    };
    let reason = validate_context_window(&config, "gpt-5.3-codex-spark")
        .err()
        .map_or_else(
            || "unexpected validation success".to_owned(),
            |error| error.to_string(),
        );
    assert!(
        reason.contains("gpt-5.3-codex-spark"),
        "names the model: {reason}"
    );
    assert!(
        reason.contains("272000"),
        "names the configured value: {reason}"
    );
    assert!(reason.contains("128000"), "names the catalog max: {reason}");
}

/// An explicit window at or below the model's catalogued maximum is
/// legitimate — including a max-window override above the standard
/// window on models that support one (validation is against
/// `max_context_window`, not `context_window`).
#[test]
fn validate_accepts_windows_up_to_catalog_max() {
    let exact = AgentLoopConfig {
        context_window_limit: Some(128_000),
        ..AgentLoopConfig::default()
    };
    assert!(
        validate_context_window(&exact, "gpt-5.3-codex-spark").is_ok(),
        "a window equal to the model max is valid",
    );

    let model = crate::model_catalog::default_selection().model;
    let catalog_max = crate::model_catalog::largest_max_context_window_for_model(model);
    assert!(
        catalog_max.is_some(),
        "test precondition: default model is catalogued",
    );
    let max = catalog_max.unwrap_or_default();
    let at_max = AgentLoopConfig {
        context_window_limit: Some(max),
        ..AgentLoopConfig::default()
    };
    assert!(
        validate_context_window(&at_max, model).is_ok(),
        "catalog max is valid",
    );
}

/// The catalog fill composed with validation: a catalogued model with
/// no explicit window arms from the catalog and validates clean — the
/// zero-config path every CLI run takes.
#[test]
fn validate_accepts_catalog_filled_window() {
    let mut loop_context = LoopContext::new("base");
    let mut config = AgentLoopConfig::default();
    arm_auto_compaction(&mut loop_context, &mut config, "gpt-5.3-codex-spark");
    assert_eq!(config.context_window_limit, Some(128_000));
    assert!(
        validate_context_window(&config, "gpt-5.3-codex-spark").is_ok(),
        "catalog-filled window validates",
    );
}

/// Owner ruling (Tom, 2026-07-05): an unknown model "probably means
/// the wrong model code" — running with protections silently disabled
/// is the ruled-against state, so a None window after the fill is a
/// hard error that leads with the typo hypothesis.
#[test]
fn validate_rejects_non_catalog_model_without_explicit_window() {
    let config = AgentLoopConfig::default();
    let reason = validate_context_window(&config, "not-in-catalog-model-xyz")
        .err()
        .map_or_else(
            || "unexpected validation success".to_owned(),
            |error| error.to_string(),
        );
    assert!(
        reason.contains("not-in-catalog-model-xyz"),
        "names the model: {reason}"
    );
    assert!(
        reason.contains("typo"),
        "leads with the typo hypothesis: {reason}"
    );
    assert!(
        reason.contains("agent.context_window"),
        "names the config keys that fix it: {reason}",
    );
}

/// A deliberate uncatalogued model with an explicit window is
/// legitimate (local/openai-compatible ids); validation has no
/// catalog ceiling to check it against and passes it through.
#[test]
fn validate_accepts_non_catalog_model_with_explicit_window() {
    let config = AgentLoopConfig {
        context_window_limit: Some(32_000),
        ..AgentLoopConfig::default()
    };
    assert!(
        validate_context_window(&config, "not-in-catalog-model-xyz").is_ok(),
        "explicit window on an uncatalogued model is valid",
    );
}
