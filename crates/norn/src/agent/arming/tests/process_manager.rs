use super::super::*;

/// Whether a pid is still live, via `kill -0` (no unsafe libc call).
#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

/// NP-001 R9: arming installs the `ProcessManager` extension (the `process`
/// tool resolves it) and binds its shutdown guard to the loop context, for
/// any agent — root or child — that goes through this shared mechanism.
#[test]
fn arm_process_manager_installs_extension_and_shutdown_guard() {
    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::new(SessionId("sess".to_owned())));
    let mut loop_context = LoopContext::new("base");
    loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::new()));
    let event_store = Arc::new(EventStore::new());
    let agent_id = Uuid::new_v4();

    assert!(ctx.get_extension::<ProcessManager>().is_none());
    arm_process_manager(&ctx, &mut loop_context, &event_store, agent_id, None, None);

    assert!(
        ctx.get_extension::<ProcessManager>().is_some(),
        "the process tool's manager extension is installed",
    );
    assert!(
        loop_context.process_manager.is_some(),
        "the shutdown guard is bound to the loop context",
    );
}

/// R9 / F4: dropping the `LoopContext` (which owns the
/// `ProcessManagerGuard`) runs the manager's shutdown at the runtime-drop
/// level — a still-running process group is killed, so an OS pid probe of a
/// backgrounded grandchild fails afterwards. This proves teardown through
/// the real arming path and guard drop, not merely a direct
/// `ProcessManager::shutdown` call.
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial]
async fn dropping_the_loop_context_kills_running_process_groups()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    temp_env::async_with_vars([("NORN_HOME", Some(dir.path().as_os_str()))], async {
        let gc_file = dir.path().join("grandchild.pid");
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SessionId("sess".to_owned())));
        let mut loop_context = LoopContext::new("base");
        loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::new()));
        let event_store = Arc::new(EventStore::new());
        let agent_id = Uuid::new_v4();

        arm_process_manager(&ctx, &mut loop_context, &event_store, agent_id, None, None);
        let manager = ctx
            .get_extension::<ProcessManager>()
            .ok_or("manager was not armed on the tool context")?;
        let cwd = std::env::current_dir()?;
        let handle = manager
            .spawn(
                &format!("sleep 300 & echo $! > '{}'; sleep 300", gc_file.display()),
                &cwd,
                None,
            )
            .await?;

        // Probe the backgrounded grandchild (it shares the process group): its
        // parent is the shell, so after the group kill init reaps it and
        // `kill -0` then fails. The shell child itself would linger as a zombie
        // (the aborted supervisor never reaps it), so it is not a reliable probe.
        let gc_pid: i64 = {
            let mut found = None;
            for _ in 0..600 {
                if let Ok(text) = std::fs::read_to_string(&gc_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    found = Some(pid);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            found.ok_or("grandchild pid was not recorded")?
        };
        assert!(process_alive(gc_pid), "the grandchild is alive after spawn");
        let _ = handle;

        // Drop the loop context: its ProcessManagerGuard shuts the manager down
        // and kills the still-running group — even though the manager Arc still
        // lingers on the tool context.
        drop(loop_context);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while process_alive(gc_pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild (pid {gc_pid}) survived the loop-context drop",
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // The manager Arc lingered, but the guard drop already ran shutdown.
        drop(manager);
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await
}
