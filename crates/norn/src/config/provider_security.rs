//! Trust-boundary checks for settings loaded from a working directory.

use super::{HookSettings, ModelAliasSettings, NornSettings, ProviderSettings};
use crate::error::ConfigError;
use std::path::Path;

/// Rejects runtime authority that must not originate below the CWD.
///
/// Project and local settings can arrive with a cloned repository. They may
/// tune non-sensitive behavior, but cannot choose credentials, network
/// destinations, backend-bearing aliases, raw-debug sinks, executable paths,
/// automatic commands, or eager private-file reads. Validation is performed on
/// the raw layers so a higher-precedence override cannot hide a forbidden
/// field.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidConfig`] naming the forbidden field without
/// echoing its configured value or a repository-controlled profile name.
pub(crate) fn validate_working_directory_authority(
    user: &NornSettings,
    project: &NornSettings,
    local: &NornSettings,
) -> Result<(), ConfigError> {
    validate_user_file_paths(user)?;
    validate_layer("project", project)?;
    validate_layer("local", local)?;
    validate_trusted_backend_collisions(user, project, local)?;
    validate_indirect_backend_selection(user, project, local)
}

fn validate_layer(layer: &str, settings: &NornSettings) -> Result<(), ConfigError> {
    if let Some(provider) = settings.provider.as_ref()
        && let Some(field) = restricted_provider_field(provider)
    {
        return Err(untrusted_field_error(layer, &format!("provider.{field}")));
    }

    if let Some(profiles) = settings.provider_profiles.as_ref() {
        for profile in profiles.values() {
            if profile.api_shape.is_some() {
                return Err(untrusted_field_error(
                    layer,
                    "provider_profiles.<profile>.api_shape",
                ));
            }
            if let Some(field) = restricted_provider_field(&profile.provider) {
                return Err(untrusted_field_error(
                    layer,
                    &format!("provider_profiles.<profile>.{field}"),
                ));
            }
        }
    }

    if let Some(aliases) = settings.model_aliases.as_ref() {
        for target in aliases.values() {
            if target.provider_profile().is_some() {
                return Err(untrusted_field_error(
                    layer,
                    "model_aliases.<alias>.provider_profile",
                ));
            }
            if target.api_shape().is_some() {
                return Err(untrusted_field_error(
                    layer,
                    "model_aliases.<alias>.api_shape",
                ));
            }
        }
    }

    if settings.hooks.as_ref().is_some_and(contains_shell_hooks) {
        return Err(untrusted_field_error(layer, "hooks"));
    }

    if settings.variants.as_ref().is_some_and(|variants| {
        variants
            .values()
            .any(|variant| variant.prompt_file.is_some())
    }) {
        return Err(untrusted_field_error(
            layer,
            "variants.<variant>.prompt_file",
        ));
    }

    if settings
        .tools
        .as_ref()
        .and_then(|tools| tools.skill.as_ref())
        .and_then(|skill| skill.shell_execution)
        .is_some_and(|enabled| enabled)
    {
        return Err(untrusted_field_error(layer, "tools.skill.shell_execution"));
    }

    if settings
        .skills
        .as_ref()
        .and_then(|skills| skills.search_paths.as_ref())
        .is_some_and(|paths| !paths.is_empty())
    {
        return Err(untrusted_field_error(layer, "skills.search_paths"));
    }

    if settings
        .context
        .as_ref()
        .and_then(|context| context.search_paths.as_ref())
        .is_some_and(|paths| !paths.is_empty())
    {
        return Err(untrusted_field_error(layer, "context.search_paths"));
    }

    Ok(())
}

fn validate_user_file_paths(settings: &NornSettings) -> Result<(), ConfigError> {
    if let Some(provider) = settings.provider.as_ref() {
        validate_user_provider_paths("provider", provider)?;
    }
    if let Some(profiles) = settings.provider_profiles.as_ref() {
        for profile in profiles.values() {
            validate_user_provider_paths("provider_profiles.<profile>", &profile.provider)?;
        }
    }
    if settings.variants.as_ref().is_some_and(|variants| {
        variants.values().any(|variant| {
            variant
                .prompt_file
                .as_deref()
                .is_some_and(|path| !Path::new(path).is_absolute())
        })
    }) {
        return Err(trusted_path_error("variants.<variant>.prompt_file"));
    }

    for (field, paths) in [
        (
            "skills.search_paths",
            settings
                .skills
                .as_ref()
                .and_then(|skills| skills.search_paths.as_ref()),
        ),
        (
            "context.search_paths",
            settings
                .context
                .as_ref()
                .and_then(|context| context.search_paths.as_ref()),
        ),
    ] {
        if paths.is_some_and(|entries| entries.iter().any(|path| !Path::new(path).is_absolute())) {
            return Err(trusted_path_error(field));
        }
    }

    Ok(())
}

fn validate_user_provider_paths(
    prefix: &str,
    provider: &ProviderSettings,
) -> Result<(), ConfigError> {
    if provider
        .debug_dump_dir
        .as_deref()
        .is_some_and(|path| !Path::new(path).is_absolute())
    {
        return Err(trusted_path_error(&format!("{prefix}.debug_dump_dir")));
    }
    if provider
        .runner_path
        .as_deref()
        .is_some_and(|path| !is_absolute_or_path_command(path))
    {
        return Err(trusted_path_error(&format!("{prefix}.runner_path")));
    }
    Ok(())
}

fn is_absolute_or_path_command(candidate: &str) -> bool {
    if Path::new(candidate).is_absolute() {
        return true;
    }
    if candidate.is_empty() || candidate.contains(['/', '\\']) {
        return false;
    }
    let mut components = Path::new(candidate).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

fn validate_trusted_backend_collisions(
    user: &NornSettings,
    project: &NornSettings,
    local: &NornSettings,
) -> Result<(), ConfigError> {
    for (layer, settings) in [("project", project), ("local", local)] {
        if let (Some(trusted), Some(untrusted)) =
            (user.model_aliases.as_ref(), settings.model_aliases.as_ref())
            && untrusted
                .keys()
                .any(|name| trusted.get(name).is_some_and(alias_selects_backend))
        {
            return Err(untrusted_field_error(
                layer,
                "model_aliases.<alias> (trusted backend alias collision)",
            ));
        }
        if let (Some(trusted), Some(untrusted)) = (
            user.provider_profiles.as_ref(),
            settings.provider_profiles.as_ref(),
        ) && untrusted.keys().any(|name| {
            trusted.get(name).is_some_and(|profile| {
                profile.api_shape.is_some()
                    || restricted_provider_field(&profile.provider).is_some()
            })
        }) {
            return Err(untrusted_field_error(
                layer,
                "provider_profiles.<profile> (trusted backend profile collision)",
            ));
        }
    }
    Ok(())
}

fn contains_shell_hooks(hooks: &HookSettings) -> bool {
    let HookSettings {
        pre_tool,
        post_tool,
        post_tool_failure,
        pre_llm,
        post_llm,
        session_event,
        user_prompt,
        stop,
        subagent_start,
        subagent_stop,
        session_start,
        session_end,
        pre_compaction,
    } = hooks;
    [
        pre_tool,
        post_tool,
        post_tool_failure,
        pre_llm,
        post_llm,
        session_event,
        user_prompt,
        stop,
        subagent_start,
        subagent_stop,
        session_start,
        session_end,
        pre_compaction,
    ]
    .into_iter()
    .any(|slot| slot.as_ref().is_some_and(|entries| !entries.is_empty()))
}

fn validate_indirect_backend_selection(
    user: &NornSettings,
    project: &NornSettings,
    local: &NornSettings,
) -> Result<(), ConfigError> {
    for (layer, settings) in [("project", project), ("local", local)] {
        let Some(model) = settings.model.as_deref() else {
            continue;
        };
        if is_catalog_model(model) {
            continue;
        }
        if effective_alias(model, user, project, local).is_some_and(alias_selects_backend) {
            return Err(untrusted_field_error(layer, "model"));
        }
    }
    Ok(())
}

fn effective_alias<'a>(
    model: &str,
    user: &'a NornSettings,
    project: &'a NornSettings,
    local: &'a NornSettings,
) -> Option<&'a ModelAliasSettings> {
    local
        .model_aliases
        .as_ref()
        .and_then(|aliases| aliases.get(model))
        .or_else(|| {
            project
                .model_aliases
                .as_ref()
                .and_then(|aliases| aliases.get(model))
        })
        .or_else(|| {
            user.model_aliases
                .as_ref()
                .and_then(|aliases| aliases.get(model))
        })
}

fn alias_selects_backend(alias: &ModelAliasSettings) -> bool {
    alias.provider_profile().is_some() || alias.api_shape().is_some()
}

fn is_catalog_model(model: &str) -> bool {
    crate::model_catalog::catalog()
        .providers
        .iter()
        .flat_map(|provider| provider.backends)
        .flat_map(|backend| backend.models)
        .any(|entry| entry.id == model)
}

fn restricted_provider_field(provider: &ProviderSettings) -> Option<&'static str> {
    let ProviderSettings {
        base_url,
        timeout: _,
        max_retries: _,
        options,
        api_key_env,
        auth,
        rate_limit: _,
        rate_limit_interval: _,
        retry_backoff: _,
        retry_after_ceiling: _,
        runner_path,
        debug_dump_dir,
    } = provider;
    if base_url.is_some() {
        Some("base_url")
    } else if api_key_env.is_some() {
        Some("api_key_env")
    } else if auth.is_some() {
        Some("auth")
    } else if options.is_some() {
        Some("options")
    } else if debug_dump_dir.is_some() {
        Some("debug_dump_dir")
    } else if runner_path.is_some() {
        Some("runner_path")
    } else {
        None
    }
}

fn untrusted_field_error(layer: &str, field: &str) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!(
            "{layer} working-directory settings cannot set {field}: working-directory configuration cannot choose credential or backend authority, automatic commands, eager external-file reads, raw-debug sinks, or executable paths; move this field to user settings or use an explicit CLI option where supported"
        ),
    }
}

fn trusted_path_error(field: &str) -> ConfigError {
    ConfigError::InvalidConfig {
        reason: format!(
            "user settings {field} entries must be absolute so a working directory cannot redirect a trusted file source"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(value: serde_json::Value) -> Result<NornSettings, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn project_base_url_is_rejected_without_echoing_value() -> Result<(), Box<dyn std::error::Error>>
    {
        let project = settings(serde_json::json!({
            "provider": {"base_url": "https://attacker.example/secret"}
        }))?;
        let result = validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        );
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

        assert!(rendered.contains("project"));
        assert!(rendered.contains("provider.base_url"));
        assert!(!rendered.contains("attacker.example"));
        Ok(())
    }

    #[test]
    fn local_api_key_env_is_rejected_without_echoing_name() -> Result<(), Box<dyn std::error::Error>>
    {
        let local = settings(serde_json::json!({
            "provider": {"api_key_env": "GITHUB_TOKEN"}
        }))?;
        let result = validate_working_directory_authority(
            &NornSettings::default(),
            &NornSettings::default(),
            &local,
        );
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

        assert!(rendered.contains("local"));
        assert!(rendered.contains("provider.api_key_env"));
        assert!(!rendered.contains("GITHUB_TOKEN"));
        Ok(())
    }

    #[test]
    fn profile_restrictions_do_not_echo_untrusted_profile_names()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = settings(serde_json::json!({
            "provider_profiles": {
                "hostile\u{7}profile": {
                    "auth": "api_key"
                }
            }
        }))?;
        let result = validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        );
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

        assert!(rendered.contains("provider_profiles.<profile>.auth"));
        assert!(!rendered.contains("hostile"));
        assert!(!rendered.contains('\u{7}'));
        Ok(())
    }

    #[test]
    fn working_directory_provider_profile_cannot_supply_api_shape()
    -> Result<(), Box<dyn std::error::Error>> {
        let user = settings(serde_json::json!({
            "model": "private",
            "model_aliases": {
                "private": {"model": "custom", "provider_profile": "missing"}
            }
        }))?;
        let project = settings(serde_json::json!({
            "provider_profiles": {
                "missing": {"api_shape": "openai_responses", "timeout": "30s"}
            }
        }))?;

        let Err(error) =
            validate_working_directory_authority(&user, &project, &NornSettings::default())
        else {
            return Err(std::io::Error::other(
                "repository provider profile API shape was accepted",
            )
            .into());
        };
        let error = error.to_string();

        assert!(error.contains("api_shape"));
        assert!(!error.contains("missing"));
        Ok(())
    }

    #[test]
    fn debug_sinks_and_executable_paths_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        for (field, value) in [
            ("debug_dump_dir", "/tmp/private-dump"),
            ("runner_path", "./repository-script"),
        ] {
            let project = settings(serde_json::json!({
                "provider": {(field): value}
            }))?;
            let result = validate_working_directory_authority(
                &NornSettings::default(),
                &project,
                &NornSettings::default(),
            );
            let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

            assert!(rendered.contains(field));
            assert!(!rendered.contains(value));
        }
        Ok(())
    }

    #[test]
    fn non_sensitive_working_directory_provider_settings_are_allowed()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = settings(serde_json::json!({
            "provider": {"timeout": "30s", "max_retries": 2}
        }))?;

        validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        )?;
        Ok(())
    }

    #[test]
    fn working_directory_provider_options_are_rejected_before_merge()
    -> Result<(), Box<dyn std::error::Error>> {
        for project in [
            settings(serde_json::json!({
                "provider": {"options": {"conversation": "repo-thread"}}
            }))?,
            settings(serde_json::json!({
                "provider_profiles": {
                    "local": {"options": {"prompt_cache_retention": "24h"}}
                }
            }))?,
        ] {
            let Err(error) = validate_working_directory_authority(
                &NornSettings::default(),
                &project,
                &NornSettings::default(),
            ) else {
                return Err(
                    std::io::Error::other("repository provider options were accepted").into(),
                );
            };
            let error = error.to_string();
            assert!(error.contains("options"));
            assert!(!error.contains("repo-thread"));
            assert!(!error.contains("24h"));
        }
        Ok(())
    }

    #[test]
    fn working_directory_aliases_cannot_select_a_backend() -> Result<(), Box<dyn std::error::Error>>
    {
        for (field, selection) in [
            (
                "provider_profile",
                serde_json::json!({
                    "provider_profile": "private-deployment",
                    "model": "custom-model"
                }),
            ),
            (
                "api_shape",
                serde_json::json!({
                    "api_shape": "openai_responses",
                    "model": "custom-model"
                }),
            ),
        ] {
            let project = settings(serde_json::json!({
                "model_aliases": {"repository-alias": selection}
            }))?;
            let result = validate_working_directory_authority(
                &NornSettings::default(),
                &project,
                &NornSettings::default(),
            );
            let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

            assert!(rendered.contains(&format!("model_aliases.<alias>.{field}")));
            assert!(!rendered.contains("repository-alias"));
            assert!(!rendered.contains("private-deployment"));
        }
        Ok(())
    }

    #[test]
    fn working_directory_model_cannot_activate_a_user_backend_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let user = settings(serde_json::json!({
            "model_aliases": {
                "private-alias": {
                    "provider_profile": "private-deployment",
                    "api_shape": "openai_responses",
                    "model": "custom-model"
                }
            }
        }))?;
        let project = settings(serde_json::json!({"model": "private-alias"}))?;
        let result =
            validate_working_directory_authority(&user, &project, &NornSettings::default());
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

        assert!(rendered.contains("project"));
        assert!(rendered.contains("model"));
        assert!(!rendered.contains("private-alias"));
        assert!(!rendered.contains("private-deployment"));
        Ok(())
    }

    #[test]
    fn model_only_working_directory_aliases_remain_allowed()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = settings(serde_json::json!({
            "model": "repository-alias",
            "model_aliases": {"repository-alias": "custom-model"}
        }))?;

        validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        )?;
        Ok(())
    }

    #[test]
    fn every_working_directory_shell_hook_slot_is_rejected_without_echoing_commands()
    -> Result<(), Box<dyn std::error::Error>> {
        for event in [
            "pre_tool",
            "post_tool",
            "post_tool_failure",
            "pre_llm",
            "post_llm",
            "session_event",
            "user_prompt",
            "stop",
            "subagent_start",
            "subagent_stop",
            "session_start",
            "session_end",
            "pre_compaction",
        ] {
            let untrusted = settings(serde_json::json!({
                "hooks": {(event): [{
                    "command": "printf command-secret",
                    "timeout": 1000
                }]}
            }))?;
            let empty = NornSettings::default();
            for (layer, project, local) in [
                ("project", &untrusted, &empty),
                ("local", &empty, &untrusted),
            ] {
                let result =
                    validate_working_directory_authority(&NornSettings::default(), project, local);
                let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

                assert!(rendered.contains(layer));
                assert!(rendered.contains("hooks"));
                assert!(!rendered.contains("command-secret"));
            }
        }
        Ok(())
    }

    #[test]
    fn user_shell_hooks_remain_trusted() -> Result<(), Box<dyn std::error::Error>> {
        let user = settings(serde_json::json!({
            "hooks": {
                "user_prompt": [{"command": "printf trusted", "timeout": 1000}]
            }
        }))?;

        validate_working_directory_authority(
            &user,
            &NornSettings::default(),
            &NornSettings::default(),
        )?;
        Ok(())
    }

    #[test]
    fn working_directory_variant_prompt_files_are_rejected_without_echoing_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = settings(serde_json::json!({
            "variants": {
                "repository-variant": {"prompt_file": "/private/path-secret"}
            }
        }))?;
        let result = validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        );
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());

        assert!(rendered.contains("variants.<variant>.prompt_file"));
        assert!(!rendered.contains("repository-variant"));
        assert!(!rendered.contains("path-secret"));
        Ok(())
    }

    #[test]
    fn user_file_sources_must_be_absolute_to_avoid_working_directory_redirects()
    -> Result<(), Box<dyn std::error::Error>> {
        for user in [
            settings(serde_json::json!({
                "variants": {"trusted": {"prompt_file": "relative-secret.md"}}
            }))?,
            settings(serde_json::json!({
                "skills": {"search_paths": ["relative-skills"]}
            }))?,
            settings(serde_json::json!({
                "context": {"search_paths": ["relative-context"]}
            }))?,
        ] {
            let result = validate_working_directory_authority(
                &user,
                &NornSettings::default(),
                &NornSettings::default(),
            );
            let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());
            assert!(rendered.contains("must be absolute"));
            assert!(!rendered.contains("relative-secret"));
            assert!(!rendered.contains("relative-skills"));
            assert!(!rendered.contains("relative-context"));
        }
        Ok(())
    }

    #[test]
    fn absolute_user_file_sources_remain_trusted() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::current_dir()?.join("trusted-user-files");
        let root = root.to_string_lossy();
        let user = settings(serde_json::json!({
            "variants": {"trusted": {"prompt_file": format!("{root}/prompt.md")}},
            "skills": {"search_paths": [format!("{root}/skills")]},
            "context": {"search_paths": [format!("{root}/context")]}
        }))?;

        validate_working_directory_authority(
            &user,
            &NornSettings::default(),
            &NornSettings::default(),
        )?;
        Ok(())
    }

    #[test]
    fn user_provider_paths_cannot_be_redirected_by_the_working_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        for user in [
            settings(serde_json::json!({
                "provider": {"debug_dump_dir": "relative-dumps"}
            }))?,
            settings(serde_json::json!({
                "provider": {"runner_path": "bin/runner"}
            }))?,
            settings(serde_json::json!({
                "provider_profiles": {"trusted": {"debug_dump_dir": "relative-profile-dumps"}}
            }))?,
        ] {
            let Err(error) = validate_working_directory_authority(
                &user,
                &NornSettings::default(),
                &NornSettings::default(),
            ) else {
                return Err(
                    std::io::Error::other("relative trusted provider path was accepted").into(),
                );
            };
            let error = error.to_string();
            assert!(error.contains("must be absolute"));
        }

        let user = settings(serde_json::json!({
            "provider": {"runner_path": "claude", "debug_dump_dir": "/tmp/dumps"}
        }))?;
        validate_working_directory_authority(
            &user,
            &NornSettings::default(),
            &NornSettings::default(),
        )?;
        Ok(())
    }

    #[test]
    fn working_directory_cannot_shadow_trusted_backend_bundles()
    -> Result<(), Box<dyn std::error::Error>> {
        let user = settings(serde_json::json!({
            "model_aliases": {
                "private": {"model": "custom", "provider_profile": "paid"}
            },
            "provider_profiles": {
                "paid": {
                    "api_shape": "openai_responses",
                    "base_url": "https://api.openai.com/v1",
                    "api_key_env": "OPENAI_API_KEY"
                }
            }
        }))?;
        for project in [
            settings(serde_json::json!({
                "model_aliases": {"private": "different-model"}
            }))?,
            settings(serde_json::json!({
                "provider_profiles": {"paid": {"timeout": "30s"}}
            }))?,
        ] {
            let Err(error) =
                validate_working_directory_authority(&user, &project, &NornSettings::default())
            else {
                return Err(std::io::Error::other(
                    "repository backend-bundle collision was accepted",
                )
                .into());
            };
            let error = error.to_string();
            assert!(error.contains("collision"));
            assert!(!error.contains("private"));
            assert!(!error.contains("paid"));
        }
        Ok(())
    }

    #[test]
    fn working_directory_search_paths_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        for (field, untrusted) in [
            (
                "skills.search_paths",
                settings(serde_json::json!({
                    "skills": {"search_paths": ["sentinel-external-skills"]}
                }))?,
            ),
            (
                "context.search_paths",
                settings(serde_json::json!({
                    "context": {"search_paths": ["sentinel-external-context"]}
                }))?,
            ),
        ] {
            for (project, local) in [
                (&untrusted, &NornSettings::default()),
                (&NornSettings::default(), &untrusted),
            ] {
                let result =
                    validate_working_directory_authority(&NornSettings::default(), project, local);
                let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());
                assert!(rendered.contains(field));
                assert!(!rendered.contains("sentinel-external"));
            }
        }
        Ok(())
    }

    #[test]
    fn working_directory_cannot_enable_skill_shell_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = settings(serde_json::json!({
            "tools": {"skill": {"shell_execution": true}}
        }))?;
        let result = validate_working_directory_authority(
            &NornSettings::default(),
            &project,
            &NornSettings::default(),
        );
        let rendered = result.map_or_else(|error| error.to_string(), |()| String::new());
        assert!(rendered.contains("tools.skill.shell_execution"));

        let restrictive_project = settings(serde_json::json!({
            "tools": {"skill": {"shell_execution": false}}
        }))?;
        validate_working_directory_authority(
            &NornSettings::default(),
            &restrictive_project,
            &NornSettings::default(),
        )?;
        Ok(())
    }
}
