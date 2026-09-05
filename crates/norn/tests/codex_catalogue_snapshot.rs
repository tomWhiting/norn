//! Keep the operational Codex catalogue aligned with the reviewed metadata snapshot.

use norn::model_catalog::{ModelEntry, default_selection, find_model, resolve_model_alias};
use norn::provider::ReasoningEffort;
use serde::Deserialize;

#[derive(Deserialize)]
struct Snapshot {
    models: Vec<SnapshotModel>,
}

#[derive(Deserialize)]
struct SnapshotModel {
    slug: String,
    visibility: String,
    display_name: String,
    description: String,
    context_window: u64,
    max_context_window: u64,
    default_reasoning_level: String,
    supported_reasoning_levels: Vec<SnapshotEffort>,
    default_reasoning_summary: String,
    supports_reasoning_summaries: bool,
    service_tiers: Vec<SnapshotTier>,
    web_search_tool_type: String,
    input_modalities: Vec<String>,
    supports_image_detail_original: bool,
    supports_search_tool: bool,
    supports_parallel_tool_calls: bool,
    apply_patch_tool_type: String,
}

#[derive(Deserialize)]
struct SnapshotEffort {
    effort: String,
}

#[derive(Deserialize)]
struct SnapshotTier {
    id: String,
    name: String,
    description: String,
}

#[test]
fn visible_codex_models_match_reviewed_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let snapshot: Snapshot = serde_json::from_str(include_str!(
        "../../../assets/codex-model-metadata-20260905.json"
    ))?;
    assert_eq!(snapshot.models.len(), 9);
    for expected in snapshot.models {
        let actual = find_model("openai", "codex_subscription", &expected.slug);
        if expected.visibility == "hide" {
            assert!(
                actual.is_none(),
                "hidden model {} was exposed",
                expected.slug
            );
            continue;
        }
        assert_eq!(expected.visibility, "list", "{}", expected.slug);
        let actual = actual.ok_or_else(|| format!("missing Codex model {}", expected.slug))?;
        assert_snapshot_fields(actual, &expected);
    }
    Ok(())
}

fn assert_snapshot_fields(actual: &ModelEntry, expected: &SnapshotModel) {
    assert_eq!(actual.display_name, expected.display_name, "{}", actual.id);
    assert_eq!(actual.description, expected.description, "{}", actual.id);
    assert_eq!(
        actual.context_window, expected.context_window,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.max_context_window, expected.max_context_window,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.default_reasoning_effort, expected.default_reasoning_level,
        "{}",
        actual.id
    );
    let efforts: Vec<&str> = expected
        .supported_reasoning_levels
        .iter()
        .map(|level| level.effort.as_str())
        .collect();
    assert_eq!(actual.supported_reasoning_efforts, efforts, "{}", actual.id);
    assert_eq!(
        actual.default_reasoning_summary, expected.default_reasoning_summary,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.supports_reasoning_summaries, expected.supports_reasoning_summaries,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.service_tiers.len(),
        expected.service_tiers.len(),
        "{}",
        actual.id
    );
    for (actual_tier, expected_tier) in actual.service_tiers.iter().zip(&expected.service_tiers) {
        assert_eq!(actual_tier.id, "fast", "{}", actual.id);
        assert_eq!(
            actual_tier.provider_value, expected_tier.id,
            "{}",
            actual.id
        );
        assert_eq!(
            actual_tier.display_name, expected_tier.name,
            "{}",
            actual.id
        );
        assert_eq!(
            actual_tier.description, expected_tier.description,
            "{}",
            actual.id
        );
    }
    assert_eq!(
        actual.web_search_tool_type, expected.web_search_tool_type,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.input_modalities, expected.input_modalities,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.supports_image_detail_original, expected.supports_image_detail_original,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.supports_search_tool, expected.supports_search_tool,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.supports_parallel_tool_calls, expected.supports_parallel_tool_calls,
        "{}",
        actual.id
    );
    assert_eq!(
        actual.apply_patch_tool_type, expected.apply_patch_tool_type,
        "{}",
        actual.id
    );
}

#[test]
fn owner_default_is_astra_and_older_catalogue_models_remain() {
    assert_eq!(default_selection().model, "gpt-6-astra");
    assert_eq!(resolve_model_alias("astra"), Some("gpt-6-astra"));
    assert_eq!(resolve_model_alias("gpt-6-astra"), Some("gpt-6-astra"));
    assert!(find_model("openai", "codex_subscription", "gpt-5.4").is_some());
    assert!(find_model("openai", "responses_api", "gpt-6-astra").is_none());
}

#[test]
fn ultra_is_a_typed_effort_identifier() -> Result<(), serde_json::Error> {
    let effort: ReasoningEffort = serde_json::from_str("\"ultra\"")?;
    assert_eq!(effort, ReasoningEffort::Ultra);
    assert_eq!(effort.as_str(), "ultra");
    assert_eq!(serde_json::to_string(&effort)?, "\"ultra\"");
    Ok(())
}
