//! Spawn execution pipeline.

use super::{
    ActionLog, AgentCancellation, AgentEventSender, AgentHandles, AgentModel, AgentRegistry,
    AgentWakeRegistry, Arc, ChildBranchKind, ChildBranchMetadata, ChildBranchRequest, ChildLaunch,
    ChildLoopConfig, ChildResultSender, CoordinationEnvelope, HookRegistry, LifecycleEmitter,
    ParentPromptPlan, ReclaimHandshake, ReclaimOnResultDelivery, SpawnAgentArgs, SpawnIdentityArgs,
    SubagentDescriptor, SubagentKind, ToolContext, ToolEnvelope, ToolError, ToolOutput, Utc,
    VariantCatalog, auto_child_path, build_child_context, build_child_loop_context,
    effective_child_tools, grant_child_policy, infra_from, install_child_result_channel,
    launch_child, resolve_parent_model, resolve_profile_root, resolve_spawn,
    resolve_spawner_policy, slugify_name_stem,
};

pub(super) async fn execute(
    envelope: &ToolEnvelope,
    ctx: &ToolContext,
) -> Result<ToolOutput, ToolError> {
    let args: SpawnAgentArgs =
        serde_json::from_value(envelope.model_args.clone()).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("invalid arguments: {e}"),
            }
        })?;

    // Reserved-key check at the argument boundary so the caller gets
    // synchronous feedback; the agent loop re-checks the same
    // invariant as a backstop when the child run starts.
    if let Some(schema) = args.output_schema.as_ref() {
        crate::r#loop::schema::check_reserved_envelope_keys(schema).map_err(|e| {
            ToolError::ExecutionFailed {
                reason: format!("spawn_agent: {e}"),
            }
        })?;
    }
    let infra = infra_from(ctx)?;

    let parent_registry =
        infra
            .tool_registry
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed {
                reason: "spawn_agent requires AgentToolInfra.tool_registry to be configured; \
                 orchestrator must provide a ToolRegistry so the sub-agent has tools available"
                    .to_string(),
            })?;

    // The spawning agent's `AgentHandles` extension must be installed
    // before we launch a child — otherwise the child would run
    // unobservable, with no status channel and no steering channel.
    // `build_runtime` installs it during runtime construction; a
    // missing extension surfaces as a typed `MissingExtension` error.
    let handles = ctx.require_extension::<AgentHandles>()?;
    let wake_registry = ctx.require_extension::<AgentWakeRegistry>()?;

    // The coordination envelope is the runtime's deliberate child
    // policy (W3.0 made it builder-required; the CLI assembly
    // publishes its own). A context that can spawn but carries no
    // envelope is a wiring error, surfaced as the same typed
    // `MissingExtension` failure as a missing `AgentHandles` — spawn
    // never invents a policy for the child.
    let coordination = ctx.require_extension::<CoordinationEnvelope>()?;

    // The child's grant (W3.4): the caller's own granted policy — the
    // harness-stamped grant for spawned/forked callers, the envelope's
    // `child_policy` for the root — narrowed by the optional
    // `child_policy` argument, or derived by inherit-with-decrement
    // when omitted. Depth exhaustion and widening both fail typed
    // here, naming the caller's own budget; the registry re-validates
    // the same invariants from ground truth at reservation.
    let spawner_policy = resolve_spawner_policy(&infra, &coordination);
    let child_policy =
        grant_child_policy(&spawner_policy, args.child_policy.clone(), "spawn_agent")?;

    // Variant resolution (agent-variants R3, spec §3 steps 1–4):
    // variant/profile exclusivity, catalog lookup, role and model
    // resolution. Parent-model inheritance reads runtime ground truth
    // only — the live AgentModel extension (refreshed at every step
    // start with the step's actual model), else the caller's registry
    // entry (see `resolve_parent_model` for why the extension wins).
    let catalog = ctx.get_extension::<VariantCatalog>();
    let resolution = resolve_spawn(
        SpawnIdentityArgs {
            variant: args.variant.clone(),
            profile: args.profile.clone(),
            role: args.role.clone(),
            model: args.model.clone(),
        },
        catalog.as_deref(),
        "spawn_agent",
        || resolve_parent_model(&infra.registry, infra.agent_id, Some(ctx), "spawn_agent"),
    )?;
    let child_model = resolution.model.clone();
    let child_role = resolution.role.clone();

    // Child context-window validation (spec §7): fill the child's
    // window from the catalog for the CHILD's resolved model and
    // validate it — mirroring the root build's arm-then-validate
    // sequence — BEFORE anything is reserved or persisted, so a
    // failure is a clean typed error with no burned name and no
    // dangling reservation. The same resolved config rides into the
    // launch below; the launch-side arming finds the window already
    // set and leaves it untouched.
    let mut child_config = ChildLoopConfig::resolve(child_policy.loop_config);
    crate::agent::arming::arm_child_window(&mut child_config, &child_model).map_err(|e| {
        ToolError::ExecutionFailed {
            reason: format!("spawn_agent: {e}"),
        }
    })?;

    // Build the child's loop context and resolve the variant's or
    // profile's tool list. Profile authority stays rooted at the
    // immutable launch directory even after a tool changes the live
    // execution directory with `cd`.
    let profile_root = resolve_profile_root(ctx, args.profile.is_some())?;
    let (mut child_loop_ctx, profile_tools) = build_child_loop_context(
        resolution
            .variant_prompt
            .as_deref()
            .zip(resolution.variant_prompt_origin),
        args.profile.as_deref(),
        &profile_root,
    )?;
    // Reasoning effort (spec §3.6 + owner rulings 2026-07-07):
    // variant → profile → the parent's ACTIVE effort from the live
    // per-step stamp, validated against the model catalog for the
    // CHILD's resolved model BEFORE the reservation — see
    // `resolve_child_reasoning_effort` for the full contract.
    // Threaded onto the child's LoopContext, which the child loop's
    // prompt assembly copies into every provider request.
    child_loop_ctx.reasoning_effort =
        super::super::variant_resolve::resolve_child_reasoning_effort(
            &super::super::variant_resolve::ChildEffortInputs {
                variant_effort: resolution.reasoning_effort,
                variant_name: resolution.variant_name.as_deref(),
                profile_effort: child_loop_ctx.reasoning_effort,
                profile_name: args.profile.as_deref(),
                parent_live_effort: ctx
                    .get_extension::<AgentModel>()
                    .and_then(|live| live.reasoning_effort),
                child_model: &child_model,
                child_role: &child_role,
                surface: "spawn_agent",
            },
        )?;

    // Effective tools = allowlist ∩ policy (R6): the explicit `tools`
    // argument wins, else the variant's subset, else the profile's
    // list; the granted policy then strips what the child may never
    // use — `signal_agent` under MessagingScope::None, spawn_agent
    // AND fork for a leaf grant — at assembly, so the child's tool
    // definitions and its `SubAgentExecutor` gate agree (the
    // call-rejection paths stay as defence-in-depth).
    let base_allow_list: Option<Vec<String>> = args
        .tools
        .clone()
        .or(resolution.variant_tools.clone())
        .or(profile_tools);
    let mcp_selection = args
        .mcp_servers
        .clone()
        .or(resolution.variant_mcp_servers.clone());
    let allow_list =
        effective_child_tools(parent_registry, base_allow_list, &child_policy, &child_role);

    // Skill listing (parity with the root): advertise "# Available
    // Skills" on the child's system prompt only when the parent
    // published a skill catalog AND the skill tool is on the child's
    // resolved surface. Uses the same shared mechanism + filtered
    // listing the root builder uses, so the section cannot drift.
    if let Some(catalog) = ctx.get_extension::<crate::skill::SkillCatalog>() {
        crate::agent::arming::install_child_skill_listing(
            &mut child_loop_ctx,
            &catalog,
            crate::agent::arming::child_skill_tool_available(
                parent_registry,
                allow_list.as_deref(),
            ),
        );
    }

    // Auto paths nest under the spawning agent's own registry path so
    // the agents tree reads as a real tree at every depth (W3.4).
    let path = args
        .path
        .unwrap_or_else(|| auto_child_path(&infra.registry, infra.agent_id, "spawn"));

    // The session-tree name label and the audit-trail provenance
    // prefer the variant name, then the profile name, then the
    // resolved role — so the R4 child name (and the hook matcher
    // input) carries the variant when one was used.
    let role_label = resolution
        .variant_name
        .clone()
        .or_else(|| args.profile.clone())
        .unwrap_or_else(|| child_role.clone());

    // Provenance carried on both typed lifecycle phases: the RESOLVED
    // role and model, with `profile` disclosing the variant name when
    // a variant was used — `subagent.started` on the parent's
    // timeline thereby records the variant durably.
    let descriptor = SubagentDescriptor {
        kind: SubagentKind::Spawn,
        role: child_role.clone(),
        model: child_model.clone(),
        profile: resolution
            .variant_name
            .clone()
            .or_else(|| args.profile.clone()),
    };

    // Two-phase reservation: the guard stays unconfirmed across the
    // fallible store resolution below, so an error rolls the
    // reservation back via RAII instead of leaking a confirmed entry
    // that no launch wrapper will ever transition to a terminal
    // status.
    let guard = AgentRegistry::reserve(
        &infra.registry,
        path.clone(),
        child_role.clone(),
        child_model.clone(),
        Some(infra.agent_id),
        child_policy.clone(),
        Some(&spawner_policy),
    )
    .map_err(|e| ToolError::ExecutionFailed {
        reason: format!("spawn reservation failed: {e}"),
    })?;
    let child_id = guard.id();
    child_loop_ctx.agent_id = Some(child_id);
    child_loop_ctx.pending_agent_messages = Some(Arc::clone(&infra.pending_messages));

    // Mint the child's session through the parent's branching binding
    // (V2-R2): a persistent parent yields a real write-through child
    // timeline under the root's children/ dir, with the ChildBranch
    // reservation durably on the parent's timeline PARENT-FIRST; an
    // ephemeral parent propagates ephemerality with the honest
    // `session: None` reservation. The branched binding rides on the
    // child's infra so grandchild mints recurse structurally. The
    // mint's blocking file I/O runs off the executor (F5).
    let branched = super::super::delegation::branch_child_off_executor(
        &infra.session,
        &infra.event_store,
        &ChildBranchRequest {
            child_session_id: child_id.to_string(),
            name_stem: slugify_name_stem(&role_label, "spawn"),
            kind: ChildBranchKind::Spawn,
            durability: infra.session.child_durability(),
            model: child_model.clone(),
            working_dir: ctx.working_dir().display().to_string(),
        },
    )
    .map_err(|e| ToolError::ExecutionFailed {
        reason: format!("spawn_agent: session branch failed: {e}"),
    })?;
    let child_store = Arc::clone(&branched.store);

    let child_event_sender = ctx
        .get_extension::<crate::provider::agent_event::SharedAgentEventChannel>()
        .map(|ch| AgentEventSender::new(ch.0.clone(), child_id, format!("spawn/{child_model}")));

    // Typed lifecycle: `Started` is emitted before the child task
    // launches, so it always precedes the child's own provider
    // events on the broadcast channel; the wrapper task emits
    // `Completed`. Both phases also land as Custom audit events on
    // the parent's session store.
    let lifecycle = LifecycleEmitter::new(
        child_event_sender.clone(),
        Arc::clone(&infra.event_store),
        infra.agent_id,
        child_id,
        descriptor,
        Utc::now(),
    );
    // The Started audit joins the primary write-through contract
    // (session-fidelity Gap 10) and fires BEFORE the reservation is
    // confirmed: on a persist failure the guard's RAII rollback
    // reclaims the registry slot, so a refused spawn can never leave
    // a phantom Active child pinning the parent's concurrency budget
    // (the only residue is the already-tolerated burned name +
    // dangling reservation).
    lifecycle
        .emit_started()
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!(
                "failed to persist the subagent.started audit event; \
             spawn aborted before launch: {error}",
            ),
        })?;

    // Provenance recorded on the child's AgentHandle so the parent can
    // attribute the child's audit trail (NA-008 R3).
    let branch_metadata = ChildBranchMetadata {
        child_agent_id: child_id,
        parent_agent_id: infra.agent_id,
        profile_name: resolution
            .variant_name
            .clone()
            .or_else(|| args.profile.clone()),
        spawned_at: Utc::now(),
    };

    // Hierarchical cancellation (W3.5): the child's run token is a
    // child of the spawner's published token, so cancelling the
    // spawner — or any ancestor above it — cascades to this child and
    // its whole subtree, each run ending with its real `Cancelled`
    // outcome through its own wrapper. A parent context that
    // publishes no token (embedder roots assembled outside
    // `AgentBuilder`) yields a free-standing token — exactly the
    // pre-cascade behavior; see `AgentCancellation` for the boundary.
    let child_cancel = ctx
        .get_extension::<AgentCancellation>()
        .map_or_else(tokio_util::sync::CancellationToken::new, |parent_cancel| {
            parent_cancel.0.child_token()
        });

    // Per-child ToolContext: fresh identity, fresh AgentHandles, shared
    // infrastructure forwarded from the parent, the granted policy
    // stamped for signal_agent's scope enforcement and the child's own
    // spawn-time budget reads.
    let child_ctx = build_child_context(
        &infra,
        child_id,
        Arc::clone(&child_store),
        ctx,
        Arc::clone(&branched.binding),
        child_policy.clone(),
        child_cancel.clone(),
    );
    // Every agent's context carries its own launch model and source-aware
    // prompt plan. A later fork inherits that typed plan, preserving the
    // distinct compiled-policy, configured-policy, profile, and skill
    // authorities installed on `child_loop_ctx` above. The human task is sent
    // separately as the child's User message and never enters this plan.
    child_ctx.insert_extension(Arc::new(AgentModel {
        model: child_model.clone(),
        reasoning_effort: child_loop_ctx.reasoning_effort,
    }));
    child_ctx.insert_extension(Arc::new(ParentPromptPlan::from_loop_context(
        &child_loop_ctx,
    )));
    // Per-agent result channel (W3.4): a child whose grant lets it
    // delegate gets its own child-result channel — sender on its
    // context for its spawn/fork sites, receiver wired onto its loop
    // below — so grandchild results deliver to *this child*, one hop
    // at a time.
    let child_result_rx = install_child_result_channel(
        &child_ctx,
        &child_policy,
        coordination.child_result_capacity,
    );
    let child_tools = super::super::live_tools::child_tool_snapshot(
        ctx,
        parent_registry,
        allow_list,
        mcp_selection,
        Arc::clone(&child_ctx),
    )?;
    let child_executor = child_tools.executor;
    let tool_defs = child_tools.definitions;

    // Launch the child on its own task and register the handle so the
    // parent can observe and steer it.
    let result_sender = ctx.get_extension::<ChildResultSender>();

    let persistent = infra
        .registry
        .read()
        .get(infra.agent_id)
        .is_none_or(|entry| entry.role != "fork");
    let reclaim_on_delivery =
        result_sender.is_some() && ctx.get_extension::<ReclaimOnResultDelivery>().is_some();
    let (handle_installed_tx, reclaim_handshake) = if reclaim_on_delivery {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            Some(tx),
            Some(ReclaimHandshake {
                handles: Arc::clone(&handles),
                handle_installed: rx,
            }),
        )
    } else {
        (None, None)
    };

    // NH-006 R5 / C56: fire SubagentHook::on_subagent_start before
    // launching the child. The hook is observational — Block has no
    // semantics on start (the trait method returns `()`). The shared
    // hook registry is published by the CLI's runtime builder onto
    // the orchestrator's ToolContext as an Arc<HookRegistry>
    // extension, so spawn sites without a LoopContext reference can
    // retrieve it here. Absent → no hook to fire.
    let hooks = ctx.get_extension::<HookRegistry>();
    if let Some(hooks_arc) = hooks.as_ref() {
        hooks_arc
            .run_subagent_start(&child_id.to_string(), &role_label)
            .await;
    }

    // Hook coverage (parent → child): the child's loop dispatches
    // pre/post-tool hooks from its *own* LoopContext, so the parent's
    // shared registry must be installed here — otherwise operator
    // policy/observability hooks silently never see child tool calls.
    child_loop_ctx.hooks = hooks.as_ref().map(Arc::clone);
    // The child's loop and its ToolContext share one working-dir
    // handle (seeded from the parent's current dir by
    // `build_child_context`), so the child's bash `cd` moves its
    // loop-level command execution and its tool path resolution
    // together — mirroring the fork pipeline and `build_runtime`.
    child_loop_ctx.working_dir = child_ctx.shared_working_dir();
    // Per-agent action log: the child's loop records its own tool
    // dispatches into the child's log (installed on the child context
    // by `build_child_context`), so the child's `action_log` queries
    // work and the parent's scoped queries see the child's entries.
    child_loop_ctx.action_log = child_ctx.get_extension::<ActionLog>();
    // Result delivery from the child's own children: the loop drains
    // this receiver at the same step boundaries the root uses — zero
    // loop changes, results bubble one hop per level.
    child_loop_ctx.child_result_rx = child_result_rx;

    // Mailbox registration is the final fallible setup step. It precedes
    // confirmation, so every Active child has a durable recipient timeline;
    // no blocking hook or other await exists between confirmation and the
    // synchronous controller launch.
    let mailbox_lease = Arc::new(crate::agent::PendingMailboxLease::new());
    infra
        .pending_messages
        .register_child_mailbox(
            child_id,
            branched.binding.mailbox_id(),
            &child_store,
            &mailbox_lease,
        )
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!("spawn mailbox registration failed: {error}"),
        })?;
    guard
        .confirm()
        .map_err(|error| ToolError::ExecutionFailed {
            reason: format!("spawn confirm failed: {error}"),
        })?;

    let handle = launch_child(ChildLaunch {
        provider: Arc::clone(&infra.provider),
        executor: child_executor,
        store: child_store,
        loop_ctx: child_loop_ctx,
        tool_defs,
        task: args.task,
        output_schema: args.output_schema,
        model: child_model,
        agent_registry: Arc::clone(&infra.registry),
        result_sender: result_sender.map(|s| (*s).clone()),
        child_id,
        branch_metadata,
        hooks,
        role_label,
        event_sender: child_event_sender,
        reclaim: reclaim_handshake,
        lifecycle,
        router: Arc::clone(&infra.router),
        inbound_capacity: child_policy.inbound_capacity,
        config: child_config,
        cancel: child_cancel,
        wake_registry: persistent.then(|| Arc::clone(&wake_registry)),
        persistent,
        pending_messages: Arc::clone(&infra.pending_messages),
        mailbox_lease,
    });
    handles.insert(handle);
    if persistent && let Some(handle) = handles.wake_handle(child_id) {
        wake_registry.insert(handle);
    }
    if let Some(tx) = handle_installed_tx
        && tx.send(()).is_err()
    {
        tracing::debug!(
            child_id = %child_id,
            "spawn_agent: wrapper exited before the handle-installed ack; \
             reclamation ownership lies with whoever ended the wrapper",
        );
    }

    Ok(ToolOutput::success(serde_json::json!({
        "agent_id": child_id.to_string(),
        "path": path,
        "status": "active",
    })))
}
