//! R1 assembly acceptance tests (brief §5.2 / §5.4).
//!
//! Two guards on the unified print-assembly path
//! ([`builder_from_cli`](norn_cli::runtime::builder_from_cli) →
//! `AgentBuilder::build` → `AgentParts`):
//!
//! - `print_agent_has_skill_tool_when_catalog_present` — a real skills
//!   fixture makes the assembled agent's registry carry the `skill` tool
//!   (closes §5.2, the embedded skill-tool-missing bug).
//! - `print_prompt_is_provider_aware` — a hosted-web-search provider makes
//!   the assembled system prompt reframe `web_search` as provider-native
//!   (closes §5.4, the provider-blind CLI prompt).
//!
//! Both run under an isolated `HOME` / `NORN_HOME` and working directory
//! (`temp-env`) so no on-disk settings, skills, or sessions perturb the
//! assertions. Each is a single `#[test]` that sets the process working
//! directory inside the `temp-env` scope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use clap::Parser;

use norn::agent::AgentParts;
use norn::config::NornSettings;
use norn::provider::ProviderStateIdentity;
use norn::provider::mock::MockProvider;
use norn::provider::tools::ProviderCapabilities;
use norn::provider::traits::Provider;

use norn_cli::cli::Cli;
use norn_cli::config::{
    apply_cli_profile_overrides, apply_settings_reasoning_to_profile, resolve_model_selection,
    resolve_profile,
};
use norn_cli::runtime::builder_from_cli;

fn merged_settings() -> Result<NornSettings, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    Ok(norn::runtime_init::load_merged_settings(&cwd)?)
}

/// Assemble through the unified path with a caller-chosen provider (so a
/// test can bind a hosted-search provider), resolving the profile through
/// the same helper pipeline the print driver runs before `builder_from_cli`.
fn build_parts_with(
    cli: &Cli,
    provider: Arc<dyn Provider>,
) -> Result<AgentParts, Box<dyn std::error::Error>> {
    let settings = merged_settings()?;
    let mut profile = resolve_profile(cli.profile.as_deref())?;
    apply_settings_reasoning_to_profile(&settings, &mut profile)?;
    let applied = apply_cli_profile_overrides(cli, &mut profile)?;
    let model_selection = resolve_model_selection(&profile.model, &settings)?;
    profile.model = model_selection.model;

    Ok(
        builder_from_cli(cli, provider, profile, &settings, &applied)?
            .build()?
            .into_parts(),
    )
}

fn write_skill(cwd: &Path, name: &str) {
    let dir = cwd.join(".norn").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("mkdir skills");
    std::fs::write(
        dir.join("SKILL.md"),
        "---\ndescription: a demo skill\n---\nbody\n",
    )
    .expect("write SKILL.md");
}

fn with_isolated_env<T>(f: impl FnOnce() -> T) -> T {
    let home = tempfile::tempdir().expect("home tempdir");
    let workdir = tempfile::tempdir().expect("work tempdir");
    let home_path = home.path().to_path_buf();
    temp_env::with_vars(
        [
            ("NORN_HOME", Some(home_path.as_os_str())),
            ("HOME", Some(home_path.as_os_str())),
        ],
        || {
            std::env::set_current_dir(workdir.path()).expect("chdir into isolated workdir");
            f()
        },
    )
}

#[test]
fn print_agent_has_skill_tool_when_catalog_present() -> Result<(), Box<dyn std::error::Error>> {
    with_isolated_env(|| {
        let cwd = std::env::current_dir().expect("cwd");
        write_skill(&cwd, "demo");
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);
        let parts = build_parts_with(&cli, provider)?;
        assert!(
            parts.registry.get("skill").is_some(),
            "a present skills catalog must put the `skill` tool in the \
             assembled print agent's registry",
        );
        Ok(())
    })
}

/// The provider-native reframing phrase that only appears when the bound
/// provider hosts web search (see `hosted_surface_description`).
const HOSTED_PHRASE: &str = "not a callable function tool";

#[test]
fn print_prompt_is_provider_aware() -> Result<(), Box<dyn std::error::Error>> {
    with_isolated_env(|| {
        let cli = Cli::parse_from(["norn", "-m", "gpt-5.5", "--no-session"]);

        // Hosted-search provider: the assembled prompt reframes web_search.
        let hosted: Arc<dyn Provider> = Arc::new(
            MockProvider::with_capabilities(Vec::new(), ProviderCapabilities::openai_responses())
                .with_state_identity(ProviderStateIdentity::derive(
                    "norn.cli.assembly-acceptance",
                    b"hosted-provider-fixture",
                )),
        );
        let hosted_prompt = build_parts_with(&cli, hosted)?
            .loop_context
            .system_sections
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("base system prompt section is missing"))?;
        assert!(
            hosted_prompt.contains(HOSTED_PHRASE),
            "hosted-search provider must reframe web_search as provider-native \
             in the assembled system prompt",
        );

        // Non-hosted provider: web_search stays a function tool, so the
        // hosted reframing phrase must be absent — proving the prompt tracks
        // the bound provider rather than being provider-blind.
        let plain: Arc<dyn Provider> = Arc::new(MockProvider::with_capabilities(
            Vec::new(),
            ProviderCapabilities::default(),
        ));
        let plain_prompt = build_parts_with(&cli, plain)?
            .loop_context
            .system_sections
            .first()
            .cloned()
            .ok_or_else(|| std::io::Error::other("base system prompt section is missing"))?;
        assert!(
            !plain_prompt.contains(HOSTED_PHRASE),
            "a non-hosted provider must not reframe web_search as hosted",
        );
        Ok(())
    })
}
