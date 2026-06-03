//! 3-tier loader for shell hook configuration.
//!
//! [`load_hooks_from_settings`] folds the three on-disk settings tiers
//! (`~/.norn/settings.json`, `<cwd>/.norn/settings.json`,
//! `<cwd>/.norn/settings.local.json`) into a single
//! [`HookSettings`] whose per-event-type arrays are the concatenation of
//! all matching entries — user-global first, project next,
//! local-project last — and validates that every entry has a required
//! timeout (CO6 / D9).
//!
//! Composition over duplication: the I/O, JSON parsing, `NotFound`
//! tolerance, and unknown-key warnings are owned by
//! [`crate::config::loader::load_settings`]. This loader runs on the
//! [`crate::config::loader::LoadedSettings`] that helper returns,
//! concatenates the hook slots, and applies the NH-003 timeout
//! validation. NH-005 (`ShellCommandHook`) and NH-006 (CLI wiring)
//! consume the returned [`HookSettings`].
//!
//! Settings are captured at call time (CO7 — no hot-reload). Operators
//! who edit settings while a session is running must start a new
//! session for the changes to take effect.

use std::path::Path;

use crate::config::loader::load_settings;
use crate::config::types::{HookEntry, HookSettings};
use crate::error::ConfigError;

/// Load and merge hook configuration from the three settings tiers.
///
/// Reads `~/.norn/settings.json` (user-global), `<cwd>/.norn/settings.json`
/// (project), and `<cwd>/.norn/settings.local.json` (local override) via
/// [`crate::config::loader::load_settings`], concatenates each event
/// type's hook entries in tier order (user → project → local), and
/// validates that every entry carries a non-[`None`] `timeout`.
///
/// Missing files are silently skipped (the underlying loader treats
/// `NotFound` as a default-shaped layer). Malformed JSON or any other
/// I/O failure surfaces as [`ConfigError::InvalidConfig`] with the file
/// path in the message.
///
/// # Errors
///
/// - Underlying I/O / parse errors from
///   [`crate::config::loader::load_settings`].
/// - The first [`HookEntry`] whose `timeout` is [`None`] is reported as
///   [`ConfigError::InvalidConfig`] naming the event type and command
///   (D9 — no hardcoded default).
pub fn load_hooks_from_settings(cwd: &Path) -> Result<HookSettings, ConfigError> {
    let loaded = load_settings(cwd)?;
    let user = loaded.user.hooks.unwrap_or_default();
    let project = loaded.project.hooks.unwrap_or_default();
    let local = loaded.local.hooks.unwrap_or_default();

    let merged = HookSettings {
        pre_tool: concat_slot(&[&user.pre_tool, &project.pre_tool, &local.pre_tool]),
        post_tool: concat_slot(&[&user.post_tool, &project.post_tool, &local.post_tool]),
        post_tool_failure: concat_slot(&[
            &user.post_tool_failure,
            &project.post_tool_failure,
            &local.post_tool_failure,
        ]),
        pre_llm: concat_slot(&[&user.pre_llm, &project.pre_llm, &local.pre_llm]),
        post_llm: concat_slot(&[&user.post_llm, &project.post_llm, &local.post_llm]),
        session_event: concat_slot(&[
            &user.session_event,
            &project.session_event,
            &local.session_event,
        ]),
        user_prompt: concat_slot(&[&user.user_prompt, &project.user_prompt, &local.user_prompt]),
        stop: concat_slot(&[&user.stop, &project.stop, &local.stop]),
        subagent_start: concat_slot(&[
            &user.subagent_start,
            &project.subagent_start,
            &local.subagent_start,
        ]),
        subagent_stop: concat_slot(&[
            &user.subagent_stop,
            &project.subagent_stop,
            &local.subagent_stop,
        ]),
        session_start: concat_slot(&[
            &user.session_start,
            &project.session_start,
            &local.session_start,
        ]),
        session_end: concat_slot(&[&user.session_end, &project.session_end, &local.session_end]),
        pre_compaction: concat_slot(&[
            &user.pre_compaction,
            &project.pre_compaction,
            &local.pre_compaction,
        ]),
    };

    validate_timeouts(&merged)?;
    Ok(merged)
}

/// Concatenate the three tiers' entry vectors for a single event slot.
///
/// Returns [`None`] when every tier is [`None`] for this slot, so the
/// resulting [`HookSettings`] preserves the "no hooks registered"
/// signal (downstream consumers can skip work when the slot is `None`).
fn concat_slot(layers: &[&Option<Vec<HookEntry>>]) -> Option<Vec<HookEntry>> {
    if layers.iter().all(|opt| opt.is_none()) {
        return None;
    }
    let mut out: Vec<HookEntry> = Vec::new();
    for layer in layers {
        if let Some(entries) = layer.as_ref() {
            out.extend(entries.iter().cloned());
        }
    }
    Some(out)
}

/// Walk every event slot and reject the first [`HookEntry`] missing a
/// `timeout`. The error names the slot and the command so operators can
/// locate the offending entry directly in their settings JSON.
///
/// Order matches the `HookSettings` field declaration (and the
/// `HookEventType` variant order) so multiple-error scenarios report a
/// stable first failure.
fn validate_timeouts(settings: &HookSettings) -> Result<(), ConfigError> {
    let slots: &[(&str, &Option<Vec<HookEntry>>)] = &[
        ("pre_tool", &settings.pre_tool),
        ("post_tool", &settings.post_tool),
        ("post_tool_failure", &settings.post_tool_failure),
        ("pre_llm", &settings.pre_llm),
        ("post_llm", &settings.post_llm),
        ("session_event", &settings.session_event),
        ("user_prompt", &settings.user_prompt),
        ("stop", &settings.stop),
        ("subagent_start", &settings.subagent_start),
        ("subagent_stop", &settings.subagent_stop),
        ("session_start", &settings.session_start),
        ("session_end", &settings.session_end),
        ("pre_compaction", &settings.pre_compaction),
    ];
    for (event_name, slot) in slots {
        let Some(entries) = slot.as_ref() else {
            continue;
        };
        for entry in entries {
            if entry.timeout.is_none() {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "hook entry for {event_name} is missing required field 'timeout' (command: {:?})",
                        entry.command
                    ),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants,
    unsafe_code
)]
mod tests {
    use super::*;

    /// Guard that swaps `NORN_HOME` for the duration of a test and
    /// restores the prior value on drop. Mirrors the helper in
    /// `crate::config::paths` and `crate::config::loader` so loader
    /// tests can isolate the user-tier resolution.
    struct NornHomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl NornHomeGuard {
        fn set(value: Option<&std::path::Path>) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial_test::serial]` on every
            // consumer; no concurrent reader observes the mutated env.
            match value {
                Some(path) => unsafe { std::env::set_var("NORN_HOME", path) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
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

    #[test]
    #[serial_test::serial]
    fn missing_files_produce_empty_hook_settings() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let settings = load_hooks_from_settings(cwd.path()).expect("missing files must not error");
        assert!(settings.pre_tool.is_none());
        assert!(settings.post_tool.is_none());
        assert!(settings.post_tool_failure.is_none());
        assert!(settings.pre_llm.is_none());
        assert!(settings.post_llm.is_none());
        assert!(settings.session_event.is_none());
        assert!(settings.user_prompt.is_none());
        assert!(settings.stop.is_none());
        assert!(settings.subagent_start.is_none());
        assert!(settings.subagent_stop.is_none());
        assert!(settings.session_start.is_none());
        assert!(settings.session_end.is_none());
        assert!(settings.pre_compaction.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn three_tier_merge_concatenates_in_priority_order() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "matcher": "Write", "command": "user.sh", "timeout": 5 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "matcher": "Edit", "command": "project.sh", "timeout": 10 }
                    ]
                }
            }"#,
        )
        .unwrap();
        std::fs::write(
            norn_dir.join("settings.local.json"),
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "matcher": "Bash", "command": "local.sh", "timeout": 15 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let settings = load_hooks_from_settings(cwd.path()).unwrap();
        let pre_tool = settings.pre_tool.expect("merged pre_tool present");
        assert_eq!(pre_tool.len(), 3, "user + project + local concatenated");
        assert_eq!(pre_tool[0].command, "user.sh", "user-tier comes first");
        assert_eq!(pre_tool[1].command, "project.sh", "project second");
        assert_eq!(pre_tool[2].command, "local.sh", "local last");
    }

    #[test]
    #[serial_test::serial]
    fn new_event_slots_round_trip_through_settings_json() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{
                "hooks": {
                    "user_prompt": [
                        { "command": "user-prompt.sh", "timeout": 3 }
                    ],
                    "stop": [
                        { "command": "stop.sh", "timeout": 4 }
                    ],
                    "subagent_start": [
                        { "matcher": "scout", "command": "sa-start.sh", "timeout": 5 }
                    ],
                    "subagent_stop": [
                        { "matcher": "scout", "command": "sa-stop.sh", "timeout": 6 }
                    ],
                    "session_start": [
                        { "command": "ss.sh", "timeout": 7 }
                    ],
                    "session_end": [
                        { "command": "se.sh", "timeout": 8 }
                    ],
                    "pre_compaction": [
                        { "command": "pc.sh", "timeout": 9 }
                    ],
                    "post_tool_failure": [
                        { "matcher": "Write", "command": "ptf.sh", "timeout": 10 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let settings = load_hooks_from_settings(cwd.path()).unwrap();
        assert_eq!(settings.user_prompt.as_ref().unwrap().len(), 1);
        assert_eq!(
            settings.user_prompt.as_ref().unwrap()[0].command,
            "user-prompt.sh"
        );
        assert_eq!(settings.stop.as_ref().unwrap()[0].command, "stop.sh");
        assert_eq!(
            settings.subagent_start.as_ref().unwrap()[0].command,
            "sa-start.sh"
        );
        assert_eq!(
            settings.subagent_stop.as_ref().unwrap()[0].command,
            "sa-stop.sh"
        );
        assert_eq!(settings.session_start.as_ref().unwrap()[0].command, "ss.sh");
        assert_eq!(settings.session_end.as_ref().unwrap()[0].command, "se.sh");
        assert_eq!(
            settings.pre_compaction.as_ref().unwrap()[0].command,
            "pc.sh"
        );
        assert_eq!(
            settings.post_tool_failure.as_ref().unwrap()[0].command,
            "ptf.sh"
        );
    }

    #[test]
    #[serial_test::serial]
    fn missing_timeout_is_rejected_with_descriptive_error() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "matcher": "Write", "command": "no-timeout.sh" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let err = load_hooks_from_settings(cwd.path())
            .expect_err("missing timeout must be rejected at load time");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(
            reason.contains("pre_tool"),
            "error must name the event type: {reason}"
        );
        assert!(
            reason.contains("timeout"),
            "error must mention the timeout field: {reason}"
        );
        assert!(
            reason.contains("no-timeout.sh"),
            "error must quote the command: {reason}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn explicit_timeout_passes_validation() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{
                "hooks": {
                    "pre_tool": [
                        { "matcher": "Write", "command": "ok.sh", "timeout": 30 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let settings = load_hooks_from_settings(cwd.path()).unwrap();
        let pre_tool = settings.pre_tool.unwrap();
        assert_eq!(pre_tool.len(), 1);
        assert_eq!(pre_tool[0].timeout, Some(30));
    }

    #[test]
    #[serial_test::serial]
    fn missing_timeout_in_later_tier_still_rejected() {
        // A user-tier entry with a valid timeout must not mask a
        // project-tier entry that is missing one: every entry in the
        // merged result is validated.
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        std::fs::write(
            user_home.path().join("settings.json"),
            r#"{
                "hooks": {
                    "post_llm": [
                        { "command": "user-ok.sh", "timeout": 5 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.json"),
            r#"{
                "hooks": {
                    "post_llm": [
                        { "command": "missing.sh" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let err = load_hooks_from_settings(cwd.path())
            .expect_err("later-tier missing timeout must error");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(reason.contains("post_llm"), "{reason}");
        assert!(reason.contains("missing.sh"), "{reason}");
    }

    #[test]
    #[serial_test::serial]
    fn malformed_json_surfaces_file_path_in_error() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        let bad_path = norn_dir.join("settings.json");
        std::fs::write(&bad_path, "{ not json }").unwrap();

        let err = load_hooks_from_settings(cwd.path()).expect_err("malformed JSON must error");
        let ConfigError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(
            reason.contains(&bad_path.display().to_string()),
            "reason missing file path: {reason}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn only_one_tier_present_still_loads_cleanly() {
        let user_home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let _guard = NornHomeGuard::set(Some(user_home.path()));

        // Only the local-override file exists.
        let norn_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&norn_dir).unwrap();
        std::fs::write(
            norn_dir.join("settings.local.json"),
            r#"{
                "hooks": {
                    "session_event": [
                        { "matcher": "UserMessage", "command": "log.sh", "timeout": 2 }
                    ]
                }
            }"#,
        )
        .unwrap();

        let settings = load_hooks_from_settings(cwd.path()).unwrap();
        let evs = settings.session_event.expect("session_event present");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].command, "log.sh");
        assert!(settings.pre_tool.is_none());
    }
}
