use super::super::*;

/// The child-path window guard: a catalogued model fills from the
/// catalog and validates clean; an uncatalogued model (a child has no
/// explicit-window escape hatch) is rejected loudly, mirroring the
/// root's unknown-model rejection.
#[test]
fn arm_child_window_fills_catalog_model_and_rejects_unknown() {
    let model = crate::model_catalog::default_selection().model;
    let mut config = AgentLoopConfig::default();
    assert!(
        arm_child_window(&mut config, model).is_ok(),
        "catalogued child model validates",
    );
    assert_eq!(
        config.context_window_limit,
        crate::model_catalog::smallest_context_window_for_model(model),
        "the child's window is filled from the catalog for its own model",
    );

    let mut unknown = AgentLoopConfig::default();
    let reason = arm_child_window(&mut unknown, "not-in-catalog-model-xyz")
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
/// root-only knobs (`agent.context_window` settings, the `-c`
/// override, the builder window) do not exist on the child path and
/// must not be prescribed — children have no explicit-window input.
#[test]
fn arm_child_window_rejection_prescribes_child_remedies_not_root_knobs() {
    let mut config = AgentLoopConfig::default();
    let reason = arm_child_window(&mut config, "not-in-catalog-model-xyz")
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
        reason.contains("typo"),
        "leads with the typo hypothesis: {reason}"
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
        arm_child_window(&mut config, "not-in-catalog-model-xyz").is_ok(),
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
    let reason = arm_child_window(&mut config, "gpt-5.3-codex-spark")
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
        arm_child_window(&mut config, "gpt-5.3-codex-spark").is_ok(),
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
            Some(ReasoningEffort::High),
            &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
            model,
        ),
        Ok(Some(ReasoningEffort::High))
    ));
    assert!(matches!(
        arm_child_reasoning_effort(
            Some(ReasoningEffort::High),
            &ChildEffortSource::Inherited { child: "worker" },
            model,
        ),
        Ok(Some(ReasoningEffort::High))
    ));
    assert!(matches!(
        arm_child_reasoning_effort(
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
        reason.contains("not in the model catalog"),
        "states why no effort can be vouched for: {reason}",
    );
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
