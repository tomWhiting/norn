use super::*;

#[test]
fn build_includes_all_standard_tools_by_default() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .build()
        .expect("build succeeds");
    for name in [
        "read",
        "write",
        "edit",
        "bash",
        "apply_patch",
        "search",
        "action_log",
    ] {
        assert!(
            agent.registry.get(name).is_some(),
            "tool '{name}' must be present by default",
        );
    }
}

#[test]
fn build_with_runtime_base_and_diagnostic_override_installs_one_post_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .load_runtime_base()
        .diagnostic_infra(infra)
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    assert_eq!(
        ctx.post_checks.len(),
        1,
        "runtime base plus diagnostic override must install exactly one diagnostics post-check",
    );
}

/// Guard that swaps `NORN_HOME` for the duration of a test and restores
/// the prior value on drop. Consumers must be `#[serial]`.
#[allow(unsafe_code)]
struct NornHomeGuard {
    prior: Option<std::ffi::OsString>,
}

#[allow(unsafe_code)]
impl NornHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let prior = std::env::var_os("NORN_HOME");
        // SAFETY: paired with `#[serial]` on the sole consumer, so no
        // concurrent reader observes the mutated env.
        unsafe { std::env::set_var("NORN_HOME", path) };
        Self { prior }
    }
}

#[allow(unsafe_code)]
impl Drop for NornHomeGuard {
    fn drop(&mut self) {
        // SAFETY: see [`Self::set`].
        match &self.prior {
            Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
            None => unsafe { std::env::remove_var("NORN_HOME") },
        }
    }
}

/// Findings 1 & 2 (security): under confinement the computed read-exempt
/// set is drawn ONLY from well-known, non-model-writable convention
/// locations. It contains the NORN_HOME-aware home skills dir and the
/// narrow `~/.norn/{skills,profiles,rules}` subdirs; it never contains a
/// settings-declared `skills.search_paths` entry (Finding 1: those are
/// model-writable and would be a persistent escape), nor the `~/.norn/`
/// root itself or either session namespace (Finding 2: the active store
/// and legacy source hold transcripts for ALL workspaces).
#[test]
#[serial_test::serial]
#[allow(clippy::unwrap_used)]
fn confined_read_exempt_set_excludes_settings_paths_and_sessions() {
    let norn_home = tempfile::tempdir().expect("norn_home");

    // The convention subdirs must exist on disk — the context setter
    // canonicalizes and drops non-existent exempt roots.
    for sub in ["skills", "profiles", "rules", "session-store", "sessions"] {
        std::fs::create_dir(norn_home.path().join(sub)).expect("mk norn subdir");
    }

    // A settings-declared skill search path that lives OUTSIDE the
    // workspace and exists on disk — under the pre-fix code this would
    // have been canonicalized into the exempt set.
    let outside = tempfile::tempdir().expect("outside");
    let outside_skills = outside.path().join("evil-skills");
    std::fs::create_dir(&outside_skills).expect("mk outside skills");

    // Trusted user settings declare that absolute search path. Repository
    // settings cannot declare search paths at all; the separate invariant
    // here is that even trusted extra paths do not become confinement
    // exemptions writable by the model.
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        norn_home.path().join("settings.json"),
        serde_json::json!({
            "skills": { "search_paths": [outside_skills.to_string_lossy()] }
        })
        .to_string(),
    )
    .expect("write settings");

    // Publish the process-global override only after its complete settings
    // document exists. Non-serial builder tests can read NORN_HOME, so exposing
    // a partially-written file here creates an unrelated parse race.
    let norn_home_guard = NornHomeGuard::set(norn_home.path());

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(workspace.path())
        .workspace_root(workspace.path())
        .load_runtime_base()
        .build()
        .expect("build succeeds");

    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let roots = ctx.read_exempt_roots();

    let canon = |p: &std::path::Path| p.canonicalize().expect("canonicalize");
    let skills = canon(&norn_home.path().join("skills"));
    let profiles = canon(&norn_home.path().join("profiles"));
    let rules = canon(&norn_home.path().join("rules"));

    assert!(
        roots.contains(&skills),
        "home skills dir must be exempt: {roots:?}",
    );
    assert!(
        roots.contains(&profiles),
        "home profiles dir must be exempt: {roots:?}",
    );
    assert!(
        roots.contains(&rules),
        "home rules dir must be exempt: {roots:?}",
    );

    let session_store = canon(&norn_home.path().join("session-store"));
    let legacy_sessions = canon(&norn_home.path().join("sessions"));
    let norn_root = canon(norn_home.path());
    let settings_path = canon(&outside_skills);
    assert!(
        !roots.contains(&session_store),
        "active session store must NEVER be exempt: {roots:?}",
    );
    assert!(
        !roots.contains(&legacy_sessions),
        "legacy sessions dir must NEVER be exempt: {roots:?}",
    );
    assert!(
        !roots.contains(&norn_root),
        "the ~/.norn root itself must never be exempt: {roots:?}",
    );
    assert!(
        !roots.contains(&settings_path),
        "settings-declared search_paths must never be exempt (model-writable): {roots:?}",
    );
    drop(norn_home_guard);
}

#[test]
fn build_applies_embedding_profile_overrides() {
    let temp = tempfile::tempdir().expect("tempdir");
    let capability = Capability {
        name: "extra".to_owned(),
        tools: vec!["bash".to_owned()],
        system_instructions: vec!["Capability instruction.".to_owned()],
        disallowed_patterns: Vec::new(),
    };

    let agent = AgentBuilder::new(provider_with(vec![]))
        .profile(Profile {
            name: "base".to_owned(),
            model: "test-model".to_owned(),
            tools: Some(vec!["read".to_owned(), "write".to_owned()]),
            system_instructions: vec!["Base instruction.".to_owned()],
            ..Profile::default()
        })
        .working_dir(temp.path())
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .reasoning_effort(ReasoningEffort::High)
        .service_tier(ServiceTier::Fast)
        .allowed_tools(&["read"])
        .without_tools(&["write"])
        .capabilities(vec![capability])
        .append_system_prompt("Appended instruction.")
        .build()
        .expect("build succeeds");

    assert_eq!(
        agent.loop_context.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert_eq!(agent.loop_context.service_tier, Some(ServiceTier::Fast));
    assert!(agent.registry.get("read").is_some());
    assert!(
        agent.registry.get("bash").is_some(),
        "capability tools remain additive"
    );
    assert!(agent.registry.get("write").is_none());
    let base = agent.loop_context.base_system_instruction();
    assert!(base.contains("Base instruction."));
    assert!(base.contains("Capability instruction."));
    assert!(base.contains("Appended instruction."));
}

#[test]
fn build_preserves_workspace_profile_and_operator_append_authority() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent = AgentBuilder::new(provider_with(vec![]))
        .profile_with_origin(
            Profile {
                model: "test-model".to_owned(),
                system_instructions: vec!["Workspace instruction.".to_owned()],
                ..Profile::default()
            },
            ProfileOrigin::WorkingDirectory,
        )
        .working_dir(temp.path())
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .append_system_prompt("Operator instruction.")
        .build()
        .expect("build succeeds");

    let plan = agent
        .loop_context
        .stable_prompt_plan()
        .expect("root build installs a typed prompt plan");
    let fragments = plan.fragments();
    assert!(fragments.iter().any(|fragment| {
        fragment.source() == PromptSource::WorkspaceProfile
            && fragment.authority() == PromptAuthority::User
            && fragment.content() == "Workspace instruction."
    }));
    assert!(fragments.iter().any(|fragment| {
        fragment.source() == PromptSource::OperatorOverride
            && fragment.authority() == PromptAuthority::Developer
            && fragment.content() == "Operator instruction."
    }));
}

#[test]
fn build_with_diagnostic_infra_registers_stop_hook() {
    let temp = tempfile::tempdir().expect("tempdir");
    let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .diagnostic_infra(infra)
        .build()
        .expect("build succeeds");

    let hooks = agent
        .loop_context
        .hooks
        .as_ref()
        .expect("diagnostic infra installs hook registry");
    assert_eq!(hooks.stop_len(), 1);
}

#[test]
fn build_without_diagnostic_infra_does_not_register_stop_hook() {
    let temp = tempfile::tempdir().expect("tempdir");

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .build()
        .expect("build succeeds");

    assert!(agent.loop_context.hooks.is_none());
}

#[tokio::test]
async fn diagnostic_stop_hook_runs_after_user_stop_hooks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let infra = Arc::new(build_diagnostic_infra(temp.path(), None, None));
    let mut registry = HookRegistry::new();
    registry.register(Hook::Stop(Box::new(BlockingStopHook)));

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .hooks(Arc::new(registry))
        .diagnostic_infra(infra)
        .build()
        .expect("build succeeds");

    let outcome = agent
        .loop_context
        .hooks
        .as_ref()
        .expect("hooks installed")
        .run_stop("done")
        .await;

    match outcome {
        HookOutcome::Block { reason } => assert!(reason.starts_with("user-stop-hook")),
        HookOutcome::Proceed | HookOutcome::Modify { .. } => {
            panic!("user hook should block first")
        }
    }
}
