//! Hook-settings extraction from the merged settings document.
//!
//! [`load_hooks_from_settings`] projects the `hooks` section out of an
//! already-merged [`NornSettings`] value. There is exactly ONE settings
//! pipeline: [`crate::config::loader::load_settings`] reads the three
//! on-disk tiers once,
//! [`crate::config::validate_working_directory_authority`]
//! rejects command authority from the project/local layers before merge,
//! [`crate::config::merge::merge_settings`] combines the trusted layers, and
//! [`crate::config::validate::validate_settings`] enforces the semantic rules.
//! This function never touches the filesystem or establishes provenance —
//! callers hand it the already-validated merged document, so hook assembly
//! costs no extra disk reads.
//!
//! Validation lives at two earlier layers:
//!
//! - `timeout` is REQUIRED at the type level (CO6 / D9 — no hardcoded
//!   default): [`HookEntry`](crate::config::types::HookEntry) declares it
//!   as a plain `u64`, so an entry omitting it fails typed
//!   deserialisation with an error naming the field and the file.
//! - Empty commands are rejected with a typed error by
//!   [`crate::config::validate::validate_settings`].
//!
//! Settings are captured at call time (CO7 — no hot-reload). Operators
//! who edit settings while a session is running must start a new
//! session for the changes to take effect.

use crate::config::types::{HookSettings, NornSettings};

/// Extract the merged hook configuration from an already-merged
/// [`NornSettings`].
///
/// Returns the `hooks` section by value (cloned) so the caller can hand
/// it to [`crate::runtime_init::assemble_hook_registry`] without holding
/// a borrow on the settings. An absent section yields the all-`None`
/// [`HookSettings::default`], which downstream assembly treats as "no
/// shell hooks declared".
#[must_use]
pub fn load_hooks_from_settings(settings: &NornSettings) -> HookSettings {
    settings.hooks.clone().unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;
    use crate::config::loader::load_settings;
    use crate::config::types::HookEntry;

    #[test]
    fn absent_hooks_section_yields_default() {
        let settings = NornSettings::default();
        let hooks = load_hooks_from_settings(&settings);
        assert!(hooks.pre_tool.is_none());
        assert!(hooks.post_tool.is_none());
        assert!(hooks.post_tool_failure.is_none());
        assert!(hooks.pre_llm.is_none());
        assert!(hooks.post_llm.is_none());
        assert!(hooks.session_event.is_none());
        assert!(hooks.user_prompt.is_none());
        assert!(hooks.stop.is_none());
        assert!(hooks.subagent_start.is_none());
        assert!(hooks.subagent_stop.is_none());
        assert!(hooks.session_start.is_none());
        assert!(hooks.session_end.is_none());
        assert!(hooks.pre_compaction.is_none());
    }

    #[test]
    fn trusted_merged_hooks_section_is_projected_verbatim() {
        let settings = NornSettings {
            hooks: Some(HookSettings {
                pre_tool: Some(vec![
                    HookEntry {
                        matcher: Some("Write".to_owned()),
                        command: "trusted-first.sh".to_owned(),
                        timeout: 5,
                    },
                    HookEntry {
                        matcher: Some("Edit".to_owned()),
                        command: "trusted-second.sh".to_owned(),
                        timeout: 10,
                    },
                ]),
                ..HookSettings::default()
            }),
            ..NornSettings::default()
        };
        let hooks = load_hooks_from_settings(&settings);
        let pre_tool = hooks.pre_tool.expect("pre_tool preserved");
        assert_eq!(pre_tool.len(), 2);
        assert_eq!(pre_tool[0].command, "trusted-first.sh");
        assert_eq!(pre_tool[1].command, "trusted-second.sh");
    }

    /// The runtime pipeline must reject command-bearing working-directory
    /// layers before the mechanical merge can erase their provenance.
    #[test]
    #[serial_test::serial]
    fn single_pipeline_rejects_working_directory_hooks_before_merge()
    -> Result<(), Box<dyn std::error::Error>> {
        struct NornHomeGuard {
            prior: Option<std::ffi::OsString>,
        }
        impl NornHomeGuard {
            fn set(path: &std::path::Path) -> Self {
                let prior = std::env::var_os("NORN_HOME");
                // SAFETY: paired with `#[serial_test::serial]`; no
                // concurrent reader observes the mutated env.
                unsafe { std::env::set_var("NORN_HOME", path) }
                Self { prior }
            }
        }
        impl Drop for NornHomeGuard {
            fn drop(&mut self) {
                match &self.prior {
                    Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                    None => unsafe { std::env::remove_var("NORN_HOME") },
                }
            }
        }

        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let norn_home_guard = NornHomeGuard::set(user_home.path());

        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{"hooks":{"pre_tool":[{"matcher":"Write","command":"user.sh","timeout":5}]}}"#,
        )
        .unwrap();
        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{"hooks":{"pre_tool":[{"matcher":"Edit","command":"project.sh","timeout":10}]}}"#,
        )
        .unwrap();
        std::fs::write(
            norn_dir.join("settings.local.json"),
            r#"{"hooks":{"pre_tool":[{"matcher":"Bash","command":"local.sh","timeout":15}]}}"#,
        )
        .unwrap();

        let layers = load_settings(cwd.path())?;
        let validation = crate::config::validate_working_directory_authority(
            &layers.user,
            &layers.project,
            &layers.local,
        );
        let error = validation.err().ok_or_else(|| {
            std::io::Error::other("working-directory hooks passed authority validation")
        })?;
        let rendered = error.to_string();
        assert!(rendered.contains("hooks"));
        assert!(!rendered.contains("project.sh"));
        assert!(!rendered.contains("local.sh"));

        drop(norn_home_guard);
        Ok(())
    }
}
