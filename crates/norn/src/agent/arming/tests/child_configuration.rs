use super::super::*;

/// The child-path window guard: a catalogued model fills from the
/// catalog and validates clean; absent metadata without an applicable
/// explicit window is rejected loudly.
#[test]
fn arm_child_window_fills_catalog_model_and_rejects_unknown() {
    let model = crate::model_catalog::default_selection().model;
    let mut config = AgentLoopConfig::default();
    assert!(
        arm_child_window(
            Some(crate::model_selection::CatalogBackend::CODEX),
            &mut config,
            model
        )
        .is_ok(),
        "catalogued child model validates",
    );
    assert_eq!(
        config.context_window_limit,
        CatalogBackend::CODEX
            .model(model)
            .map(|entry| entry.context_window),
        "the child's window is filled from the catalog for its own model",
    );

    let mut unknown = AgentLoopConfig::default();
    let reason = arm_child_window(
        Some(crate::model_selection::CatalogBackend::CODEX),
        &mut unknown,
        "not-in-catalog-model-xyz",
    )
    .err()
    .map_or_else(
        || "unexpected child-window success".to_owned(),
        |error| error.to_string(),
    );
    assert!(
        reason.contains("not-in-catalog-model-xyz"),
        "the rejection names the model: {reason}",
    );
}

/// The child rejection prescribes CHILD remedies only: a catalogued
/// model or an explicit spawn-time `model` that is catalogued. The
/// root-only knobs cannot establish a different child's model window.
/// The direct child override remains the remedy for that case.
#[test]
fn arm_child_window_rejection_prescribes_child_remedies_not_root_knobs() {
    let mut config = AgentLoopConfig::default();
    let reason = arm_child_window(
        Some(crate::model_selection::CatalogBackend::CODEX),
        &mut config,
        "not-in-catalog-model-xyz",
    )
    .err()
    .map_or_else(
        || "unexpected child-window success".to_owned(),
        |error| error.to_string(),
    );
    assert!(
        reason.contains("child model 'not-in-catalog-model-xyz'"),
        "names the child's model: {reason}",
    );
    assert!(
        !reason.contains("typo"),
        "does not invent a typo diagnosis: {reason}"
    );
    assert!(
        reason.contains("child_policy.loop_config.context_window"),
        "names the ruled child override (owner ruling 2026-07-07): {reason}",
    );
    for root_only in ["agent.context_window", "-c ", "builder"] {
        assert!(
            !reason.contains(root_only),
            "must not prescribe the root-only remedy '{root_only}': {reason}",
        );
    }
}

/// Operator-explicit inheritance is bound to both the live model and a
/// concrete backend; the same policy can safely support descendants.
#[test]
fn child_window_inheritance_retains_explicit_provenance_for_descendants() -> Result<(), ConfigError>
{
    for backend in [CatalogBackend::RESPONSES, CatalogBackend::CHAT] {
        let selection = crate::model_selection::ModelRuntime::new(
            Some(backend),
            "gpt-5.5",
            Some(64_000),
            None,
            None,
            std::collections::BTreeMap::new(),
        )?;
        let root = ToolContext::empty();
        root.insert_extension(Arc::new(AgentModel {
            model: selection.model().to_owned(),
            reasoning_effort: None,
        }));
        publish_parent_context_window(&root, &selection);
        let mut child_config = AgentLoopConfig::default();
        let policy =
            resolve_child_context_window(Some(&root), Some(backend), &mut child_config, "gpt-5.5")?;
        assert_eq!(policy.policy.explicit_window, Some(64_000));
        assert_eq!(child_config.context_window_limit, Some(64_000));
        let child = ToolContext::empty();
        child.insert_extension(Arc::new(AgentModel {
            model: "gpt-5.5".to_owned(),
            reasoning_effort: None,
        }));
        policy.publish(&child);
        assert_eq!(
            child
                .get_extension::<crate::tool::output_budget::ToolOutputBudget>()
                .as_deref(),
            Some(&crate::tool::output_budget::ToolOutputBudget::for_context_window(Some(64_000))),
            "a child needs its own effective tool budget in its fresh context",
        );
        let mut grandchild_config = AgentLoopConfig::default();
        let descendant = resolve_child_context_window(
            Some(&child),
            Some(backend),
            &mut grandchild_config,
            "gpt-5.5",
        )?;
        assert_eq!(descendant.policy.explicit_window, Some(64_000));
        assert_eq!(grandchild_config.context_window_limit, Some(64_000));
        let mut overridden = AgentLoopConfig {
            context_window_limit: Some(32_000),
            ..AgentLoopConfig::default()
        };
        let explicit_child = resolve_child_context_window(
            Some(&root),
            Some(backend),
            &mut overridden,
            "different-model",
        )?;
        assert_eq!(explicit_child.policy.explicit_window, Some(32_000));
    }
    Ok(())
}

/// Neither another model/route, an unknown route, a stale live model stamp,
/// nor catalogue-derived values can become an inherited explicit window.
#[test]
fn child_window_inheritance_refuses_unrelated_or_derived_metadata() -> Result<(), ConfigError> {
    let selection = crate::model_selection::ModelRuntime::new(
        Some(CatalogBackend::RESPONSES),
        "gpt-5.5",
        Some(64_000),
        None,
        None,
        std::collections::BTreeMap::new(),
    )?;
    let parent = ToolContext::empty();
    publish_parent_context_window(&parent, &selection);
    parent.insert_extension(Arc::new(AgentModel {
        model: "gpt-5.5".to_owned(),
        reasoning_effort: None,
    }));
    for (backend, model) in [
        (Some(CatalogBackend::RESPONSES), "different-model"),
        (Some(CatalogBackend::CHAT), "gpt-5.5"),
        (None, "gpt-5.5"),
    ] {
        let mut config = AgentLoopConfig::default();
        assert!(resolve_child_context_window(Some(&parent), backend, &mut config, model).is_err());
        assert_eq!(config.context_window_limit, None);
    }
    parent.insert_extension(Arc::new(AgentModel {
        model: "new-live-model".to_owned(),
        reasoning_effort: None,
    }));
    assert!(
        resolve_child_context_window(
            Some(&parent),
            Some(CatalogBackend::RESPONSES),
            &mut AgentLoopConfig::default(),
            "gpt-5.5",
        )
        .is_err()
    );

    let custom = crate::model_selection::ModelRuntime::new(
        None,
        "gpt-5.5",
        Some(64_000),
        None,
        None,
        std::collections::BTreeMap::new(),
    )?;
    publish_parent_context_window(&parent, &custom);
    parent.insert_extension(Arc::new(AgentModel {
        model: "gpt-5.5".to_owned(),
        reasoning_effort: None,
    }));
    assert!(
        resolve_child_context_window(
            Some(&parent),
            None,
            &mut AgentLoopConfig::default(),
            "gpt-5.5"
        )
        .is_err(),
        "two absent route identities are not a proven route match"
    );

    let derived = crate::model_selection::ModelRuntime::new(
        Some(CatalogBackend::CODEX),
        "gpt-5.5",
        None,
        None,
        None,
        std::collections::BTreeMap::new(),
    )?;
    publish_parent_context_window(&parent, &derived);
    parent.insert_extension(Arc::new(AgentModel {
        model: "gpt-5.5".to_owned(),
        reasoning_effort: None,
    }));
    let mut config = AgentLoopConfig::default();
    let policy = resolve_child_context_window(
        Some(&parent),
        Some(CatalogBackend::CODEX),
        &mut config,
        "gpt-5.5",
    )?;
    assert_eq!(policy.policy.explicit_window, None);
    assert_eq!(config.context_window_limit, Some(derived.window()));
    let derived_child = ToolContext::empty();
    policy.publish(&derived_child);
    assert_eq!(
        derived_child
            .get_extension::<crate::tool::output_budget::ToolOutputBudget>()
            .as_deref(),
        Some(
            &crate::tool::output_budget::ToolOutputBudget::for_context_window(Some(
                derived.window()
            ))
        ),
        "catalogue-derived child windows must also install their own tool budget",
    );
    assert!(
        resolve_child_context_window(
            Some(&parent),
            Some(CatalogBackend::RESPONSES),
            &mut AgentLoopConfig::default(),
            "gpt-5.5",
        )
        .is_err()
    );
    Ok(())
}

/// Missing API/Chat metadata names the actual route without asserting a typo.
#[test]
fn missing_child_metadata_names_actual_route() {
    for backend in [CatalogBackend::RESPONSES, CatalogBackend::CHAT] {
        let error = arm_child_window(Some(backend), &mut AgentLoopConfig::default(), "gpt-5.5")
            .err()
            .map_or_else(
                || "unexpected success".to_owned(),
                |error| error.to_string(),
            );
        assert!(
            error.contains(&format!("{}.{}", backend.provider, backend.backend)),
            "{error}"
        );
        assert!(error.contains("no capability metadata"), "{error}");
        assert!(!error.contains("typo"), "{error}");
    }
}

/// Owner ruling 2026-07-07: an explicit
/// `child_policy.loop_config.context_window` override on a deliberate
/// uncatalogued child model is accepted, with exactly that window
/// armed (mirroring the root's explicit-window semantics).
#[test]
fn arm_child_window_accepts_explicit_override_on_uncatalogued_model() {
    let mut config = AgentLoopConfig {
        context_window_limit: Some(32_000),
        ..AgentLoopConfig::default()
    };
    assert!(
        arm_child_window(
            Some(crate::model_selection::CatalogBackend::CODEX),
            &mut config,
            "not-in-catalog-model-xyz"
        )
        .is_ok(),
        "explicit child window on an uncatalogued model is valid",
    );
    assert_eq!(
        config.context_window_limit,
        Some(32_000),
        "the override is armed verbatim, never replaced by a catalog value",
    );
}

/// Owner ruling 2026-07-07 + the 2026-07-05 incident guard on the
/// child path: an explicit child window above a catalogued model's
/// maximum is rejected loudly (never a silent clamp), naming the
/// model, both numbers, and the child knob — not the root's.
#[test]
fn arm_child_window_rejects_oversized_explicit_override() {
    let mut config = AgentLoopConfig {
        context_window_limit: Some(272_000),
        ..AgentLoopConfig::default()
    };
    let reason = arm_child_window(
        Some(crate::model_selection::CatalogBackend::CODEX),
        &mut config,
        "gpt-5.3-codex-spark",
    )
    .err()
    .map_or_else(
        || "unexpected child-window success".to_owned(),
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
    assert!(
        reason.contains("child_policy.loop_config.context_window"),
        "names the child knob: {reason}",
    );
    assert!(
        !reason.contains("agent.context_window"),
        "must not prescribe the root-only settings knob: {reason}",
    );
}

/// An explicit child window at or below a catalogued model's maximum
/// beats the catalog fill — explicit config always wins.
#[test]
fn arm_child_window_explicit_override_beats_catalog_fill() {
    let mut config = AgentLoopConfig {
        context_window_limit: Some(64_000),
        ..AgentLoopConfig::default()
    };
    assert!(
        arm_child_window(
            Some(crate::model_selection::CatalogBackend::CODEX),
            &mut config,
            "gpt-5.3-codex-spark"
        )
        .is_ok(),
        "an in-range explicit child window is valid",
    );
    assert_eq!(config.context_window_limit, Some(64_000));
}

/// Re-review R2: a supported effort passes through unchanged, and no
/// effort at all stays none — for any source.
#[test]
fn arm_child_reasoning_effort_passes_supported_and_none_through() {
    use crate::provider::request::ReasoningEffort;
    let model = crate::model_catalog::default_selection().model;
    assert!(matches!(
        arm_child_reasoning_effort(
            Some(crate::model_selection::CatalogBackend::CODEX),
            Some(ReasoningEffort::High),
            &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
            model,
        ),
        Ok(Some(ReasoningEffort::High))
    ));
    assert!(matches!(
        arm_child_reasoning_effort(
            Some(crate::model_selection::CatalogBackend::CODEX),
            Some(ReasoningEffort::High),
            &ChildEffortSource::Inherited { child: "worker" },
            model,
        ),
        Ok(Some(ReasoningEffort::High))
    ));
    assert!(matches!(
        arm_child_reasoning_effort(
            Some(crate::model_selection::CatalogBackend::CODEX),
            None,
            &ChildEffortSource::Inherited { child: "worker" },
            "not-in-catalog-model-xyz",
        ),
        Ok(None)
    ));
}

/// Re-review R2: an EXPLICITLY configured effort the child's resolved
/// model does not support is a typed error naming the setting and the
/// model's catalogued efforts — root `/effort` parity, including the
/// uncatalogued-model case (the root refuses an explicit effort on a
/// model the catalog cannot vouch for; so does the child path).
#[test]
fn arm_child_reasoning_effort_explicit_unsupported_is_a_typed_error() {
    use crate::provider::request::ReasoningEffort;

    // Catalogued model, unsupported effort ("none" is declared for no
    // catalogued model — factual catalog content, not an invention).
    let model = crate::model_catalog::default_selection().model;
    let reason = arm_child_reasoning_effort(
        Some(crate::model_selection::CatalogBackend::CODEX),
        Some(ReasoningEffort::None),
        &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
        model,
    )
    .err()
    .map_or_else(
        || "unexpected effort success".to_owned(),
        |error| error.to_string(),
    );
    assert!(
        reason.contains("variants.scout.reasoning_effort"),
        "names the setting: {reason}",
    );
    assert!(reason.contains(model), "names the model: {reason}");
    assert!(
        reason.contains("low, medium, high, xhigh"),
        "lists the model's catalogued efforts: {reason}",
    );

    // Uncatalogued model: explicit effort refused, root parity.
    let reason = arm_child_reasoning_effort(
        Some(crate::model_selection::CatalogBackend::CODEX),
        Some(ReasoningEffort::High),
        &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
        "not-in-catalog-model-xyz",
    )
    .err()
    .map_or_else(
        || "unexpected effort success".to_owned(),
        |error| error.to_string(),
    );
    assert!(
        reason.contains("declares no capability metadata"),
        "states why no effort can be vouched for: {reason}",
    );
    assert!(reason.contains("openai.codex_subscription"), "{reason}");
    assert!(!reason.contains("not supported"), "{reason}");
    assert!(
        reason.contains("variants.scout.reasoning_effort"),
        "names the setting: {reason}",
    );
}

/// Re-review R2: an INHERITED effort the child's resolved model does
/// not support degrades to `None` with a `tracing::warn!` naming the
/// child, the model, and the dropped effort — never an error (the
/// caller configured nothing wrong on this spawn), never silent.
#[test]
fn arm_child_reasoning_effort_inherited_unsupported_warns_and_degrades() {
    use std::sync::Arc;

    use crate::provider::request::ReasoningEffort;
    use parking_lot::Mutex;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut *self.0.lock(), buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedBuf {
        type Writer = SharedBuf;
        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = SharedBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();

    let degraded = tracing::subscriber::with_default(subscriber, || {
        arm_child_reasoning_effort(
            Some(crate::model_selection::CatalogBackend::CODEX),
            Some(ReasoningEffort::XHigh),
            &ChildEffortSource::Inherited { child: "explorer" },
            "not-in-catalog-model-xyz",
        )
    });
    assert!(
        matches!(degraded, Ok(None)),
        "the unsupported inherited effort is dropped",
    );

    let output = String::from_utf8(buf.0.lock().clone()).unwrap_or_default();
    assert!(output.contains("WARN"), "logs at warn: {output}");
    assert!(
        output.contains("child=explorer"),
        "names the child: {output}"
    );
    assert!(
        output.contains("model=not-in-catalog-model-xyz"),
        "names the model: {output}",
    );
    assert!(
        output.contains("effort=xhigh"),
        "names the dropped effort: {output}",
    );
}
