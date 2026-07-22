use super::*;

/// Track B finding 1 (blocker): `workspace_root` must produce a built
/// agent whose context denies out-of-root file access through a real
/// tool call — previously `confine_to_workspace` had zero production
/// callers, so the control could never be switched on.
#[tokio::test]
async fn workspace_root_confines_file_tools_through_built_context() {
    use crate::tool::envelope::ToolEnvelope;

    let outer = tempfile::tempdir().expect("tempdir");
    let root = outer.path().join("ws");
    std::fs::create_dir(&root).expect("mkdir ws");
    let secret = outer.path().join("secret.txt");
    std::fs::write(&secret, "outside the workspace").expect("write secret");
    let inside = root.join("inside.txt");
    std::fs::write(&inside, "inside the workspace").expect("write inside");

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(&root)
        .workspace_root(&root)
        .build()
        .expect("build succeeds");
    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    let tool = agent.registry.get("read").expect("read tool present");

    let read_envelope = |path: &std::path::Path| ToolEnvelope {
        tool_call_id: "tc-confine".to_owned(),
        tool_name: "read".to_owned(),
        model_args: serde_json::json!({ "path": path.display().to_string() }),
        metadata: Value::Null,
    };

    let denied = tool
        .execute(&read_envelope(&secret), ctx.as_ref())
        .await
        .expect("confinement refusal is a structured tool error");
    assert!(denied.is_error(), "out-of-root read must be refused");
    assert!(
        denied.content["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("refused")),
        "refusal must be explicit: {}",
        denied.content,
    );
    assert_eq!(
        denied.content["error"]["kind"], "permission_denied",
        "confinement refusal carries the typed kind",
    );

    let allowed = tool
        .execute(&read_envelope(&inside), ctx.as_ref())
        .await
        .expect("in-root read executes");
    assert!(!allowed.is_error(), "in-root read must succeed");
}

/// Finding 1 companion: without `workspace_root` the built context
/// stays unconfined — the historical embedder behaviour.
#[tokio::test]
async fn builder_without_workspace_root_stays_unconfined() {
    use crate::tool::envelope::ToolEnvelope;

    let outer = tempfile::tempdir().expect("tempdir");
    let root = outer.path().join("ws");
    std::fs::create_dir(&root).expect("mkdir ws");
    let elsewhere = outer.path().join("elsewhere.txt");
    std::fs::write(&elsewhere, "reachable").expect("write file");

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(&root)
        .build()
        .expect("build succeeds");
    let ctx = agent
        .registry
        .shared_context()
        .expect("registry exposes its shared tool context");
    assert!(
        ctx.workspace_root().is_none(),
        "no workspace_root means no confinement root on the context",
    );
    let tool = agent.registry.get("read").expect("read tool present");
    let out = tool
        .execute(
            &ToolEnvelope {
                tool_call_id: "tc-unconfined".to_owned(),
                tool_name: "read".to_owned(),
                model_args: serde_json::json!({
                    "path": elsewhere.display().to_string(),
                }),
                metadata: Value::Null,
            },
            ctx.as_ref(),
        )
        .await
        .expect("unconfined read executes");
    assert!(!out.is_error(), "unconfined context reads anywhere");
}

/// Finding 1: a `workspace_root` that does not exist fails the build
/// with a typed configuration error instead of confining nothing.
#[test]
fn workspace_root_must_exist() {
    let temp = tempfile::tempdir().expect("tempdir");
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .workspace_root(temp.path().join("does-not-exist"))
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("workspace_root"), "{reason}");
        }
        Err(other) => panic!("expected a config error, got: {other}"),
        Ok(_) => panic!("a missing workspace_root must fail the build"),
    }
}

/// Finding 1: a `workspace_root` that is a file (not a directory) fails
/// the build with a typed configuration error.
#[test]
fn workspace_root_must_be_a_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("a-file.txt");
    std::fs::write(&file, "not a dir").expect("write file");
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .workspace_root(&file)
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("not a directory"), "{reason}");
        }
        Err(other) => panic!("expected a config error, got: {other}"),
        Ok(_) => panic!("a non-directory workspace_root must fail the build"),
    }
}

/// Track B finding 8: the builder's `bash_drain_grace` override reaches
/// the registered bash tool — a backgrounded child holding the output
/// pipes is cut off after the overridden grace, not the 2s default.
#[tokio::test]
async fn bash_drain_grace_override_reaches_the_bash_tool() {
    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .bash_drain_grace(std::time::Duration::from_millis(200))
        .build()
        .expect("build succeeds");

    let executor: &dyn ToolExecutor = agent.registry.as_ref();
    let started = std::time::Instant::now();
    let out = executor
        .execute(
            "bash",
            "tc-grace",
            serde_json::json!({ "command": "sleep 5 & echo started" }),
        )
        .await
        .expect("bash executes");
    let elapsed = started.elapsed();
    assert_eq!(
        out["streams_still_open"],
        serde_json::json!(true),
        "the backgrounded sleep holds the pipe past the grace: {out}",
    );
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "a 200ms drain grace must return well before the 2s default \
         (elapsed: {elapsed:?})",
    );
}

/// Finding 8: setting `bash_drain_grace` while excluding bash is a
/// contradiction and must fail the build rather than be silently inert.
#[test]
fn bash_drain_grace_with_bash_excluded_fails_build() {
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .bash_drain_grace(std::time::Duration::from_secs(1))
        .without_tools(&["bash"])
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("bash_drain_grace"), "{reason}");
        }
        Err(other) => panic!("expected a config error, got: {other}"),
        Ok(_) => panic!("bash_drain_grace without bash must fail the build"),
    }
}
