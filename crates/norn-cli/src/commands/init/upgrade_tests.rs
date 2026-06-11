use super::*;
use diagnostics::conventions::{ConventionsConfig, Handling};
use serial_test::serial;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cli::ExitCode;
use tempfile::tempdir;

fn load(rendered: &str) -> Result<ConventionsConfig, diagnostics::conventions::ConventionsError> {
    ConventionsConfig::load_from_str(rendered)
}

fn with_cwd<R>(dir: &Path, f: impl FnOnce() -> R) -> Result<R, std::io::Error> {
    let original = std::env::current_dir()?;
    std::env::set_current_dir(dir)?;
    let result = f();
    std::env::set_current_dir(original)?;
    Ok(result)
}

fn migrated_config(input: &str) -> Result<(String, ConventionsConfig), Box<dyn std::error::Error>> {
    let rendered = upgrade_conventions(input, "test")?;
    let config = load(&rendered)?;
    Ok((rendered, config))
}

#[test]
fn advise_on_maps_to_advisory_activation() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
"#,
    )?;
    let rule = config.rule("rust-general").ok_or("rust-general")?;
    let activation = rule.rule.activations.get("clippy").ok_or("clippy")?;
    assert_eq!(activation.handling, Handling::Advise);
    Ok(())
}

#[test]
fn block_on_maps_to_blocking_activation() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
block_on = ["clippy"]
"#,
    )?;
    let rule = config.rule("rust-general").ok_or("rust-general")?;
    let activation = rule.rule.activations.get("clippy").ok_or("clippy")?;
    assert_eq!(activation.handling, Handling::Block);
    Ok(())
}

#[test]
fn block_on_wins_duplicate_legacy_activation() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
block_on = ["clippy"]
"#,
    )?;
    let rule = config.rule("rust-general").ok_or("rust-general")?;
    let activation = rule.rule.activations.get("clippy").ok_or("clippy")?;
    assert_eq!(activation.handling, Handling::Block);
    Ok(())
}

#[test]
fn already_migrated_activation_passes_through() -> Result<(), Box<dyn std::error::Error>> {
    let (rendered, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
clippy = { on = "tool", handling = "advise" }
"#,
    )?;
    assert!(rendered.contains("clippy"));
    let rule = config.rule("rust-general").ok_or("rust-general")?;
    assert!(rule.rule.activations.contains_key("clippy"));
    Ok(())
}

#[test]
fn bypass_detection_adds_bundled_pattern_activations() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
bypass_detection = true
"#,
    )?;
    let rule = config.rule("rust-general").ok_or("rust-general")?;
    assert!(rule.rule.activations.contains_key("allow-attr"));
    assert!(rule.rule.activations.contains_key("todo-markers"));
    let allow_attr = rule
        .rule
        .activations
        .get("allow-attr")
        .ok_or("allow-attr")?;
    assert_eq!(allow_attr.handling, Handling::Advise);
    Ok(())
}

#[test]
fn generated_rust_language_definition_contains_template_sections()
-> Result<(), Box<dyn std::error::Error>> {
    let (rendered, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
"#,
    )?;
    // The bundled rust language pack ships without an `[rust.lsp]`
    // section (clippy carries the post-mutation diagnostics), so only
    // the sections the template actually provides are asserted here.
    assert!(rendered.contains("[rust.diagnostics]"));
    assert!(rendered.contains("[rust.patterns]"));
    assert!(rendered.contains("[rust.remediation]"));
    assert!(config.lang_def("rust").is_some());
    Ok(())
}

#[test]
fn generated_typescript_language_definition_contains_template_sections()
-> Result<(), Box<dyn std::error::Error>> {
    let (rendered, config) = migrated_config(
        r#"
[typescript-general]
tools = ["write"]
paths = ["**/*.ts"]
advise_on = ["biome"]
"#,
    )?;
    assert!(rendered.contains("[typescript.lsp]"));
    assert!(rendered.contains("[typescript.diagnostics]"));
    assert!(rendered.contains("[typescript.patterns]"));
    assert!(config.lang_def("typescript").is_some());
    Ok(())
}

#[test]
fn existing_language_definition_is_preserved_and_template_fills_missing_categories()
-> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust.lsp]
server = "custom-rust-analyzer"

[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
"#,
    )?;
    let rust = config.lang_def("rust").ok_or("rust")?;
    let lsp = rust.lsp.as_ref().ok_or("rust.lsp")?;
    assert_eq!(lsp.server, "custom-rust-analyzer");
    let patterns = rust.patterns.as_ref().ok_or("rust.patterns")?;
    assert!(patterns.contains_key("allow-attr"));
    Ok(())
}

#[test]
fn brace_glob_paths_detect_bundled_languages() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[web]
tools = ["write"]
paths = ["src/**/*.{ts,tsx}"]
advise_on = ["biome"]
"#,
    )?;
    assert!(config.lang_def("typescript").is_some());
    assert!(config.rule("web").is_some());
    Ok(())
}

#[test]
fn colliding_renamed_rule_does_not_silently_overwrite_existing_rule() {
    let error = upgrade_conventions(
        r#"
[typescript]
tools = ["write"]
paths = ["**/*.ts"]
advise_on = ["biome"]

[typescript-general]
tools = ["write"]
paths = ["**/*.tsx"]
advise_on = ["eslint"]
"#,
        "test",
    )
    .err()
    .map(|error| error.to_string());
    assert!(error.is_some_and(|message| message.contains("would overwrite")));
}

#[test]
fn unknown_languages_get_rules_without_generated_language_definition()
-> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[perl-general]
tools = ["write"]
paths = ["**/*.pl"]
advise_on = ["perlcritic"]
"#,
    )?;
    assert!(config.rule("perl-general").is_some());
    assert!(config.lang_def("perl").is_none());
    Ok(())
}

#[test]
fn colliding_rule_name_is_renamed_to_general() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[typescript]
tools = ["write"]
paths = ["**/*.ts"]
advise_on = ["biome"]
"#,
    )?;
    assert!(config.rule("typescript-general").is_some());
    assert!(config.rule("typescript").is_none());
    assert!(config.lang_def("typescript").is_some());
    Ok(())
}

#[test]
fn non_colliding_general_name_stays_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
"#,
    )?;
    assert!(config.rule("rust-general").is_some());
    Ok(())
}

#[test]
fn merged_language_and_rule_table_is_split_in_output() -> Result<(), Box<dyn std::error::Error>> {
    let (_, config) = migrated_config(
        r#"
[typescript]
tools = ["write"]
paths = ["**/*.ts"]
advise_on = ["biome"]

[typescript.lsp]
server = "typescript-language-server"
"#,
    )?;
    assert!(config.lang_def("typescript").is_some());
    assert!(config.rule("typescript-general").is_some());
    Ok(())
}

#[test]
fn legacy_tables_missing_rule_shape_fail_clearly() {
    let error = upgrade_conventions(
        r#"
[rust-general]
advise_on = ["clippy"]
"#,
        "test",
    )
    .err()
    .map(|error| error.to_string());
    assert!(error.is_some_and(|message| message.contains("must include both `tools` and `paths`")));
}

#[test]
#[serial]
fn default_input_and_output_file_are_resolved_against_cwd() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    fs::write(
        dir.path().join(DEFAULT_FILENAME),
        r#"
[rust-general]
tools = ["write"]
paths = ["**/*.rs"]
advise_on = ["clippy"]
"#,
    )?;
    let code = with_cwd(dir.path(), || {
        run_upgrade(None, Some(PathBuf::from("new.toml")))
    })?;
    assert_eq!(code, ExitCode::Success);
    let output = dir.path().join("new.toml");
    assert!(output.exists());
    ConventionsConfig::load(&output)?;
    Ok(())
}

#[test]
#[serial]
fn custom_input_and_output_file_are_supported() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    fs::write(
        dir.path().join("custom.toml"),
        r#"
[typescript]
tools = ["write"]
paths = ["**/*.ts"]
advise_on = ["biome"]
"#,
    )?;
    let code = with_cwd(dir.path(), || {
        run_upgrade(
            Some(PathBuf::from("custom.toml")),
            Some(PathBuf::from("new.toml")),
        )
    })?;
    assert_eq!(code, ExitCode::Success);
    let output = fs::read_to_string(dir.path().join("new.toml"))?;
    assert!(output.contains("[typescript-general]"));
    Ok(())
}
