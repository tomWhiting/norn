//! CLI regressions for complete channel opt-in and provenance-aware source validation.

use std::error::Error;

use clap::Parser;
use norn::config::{McpRuntimeOverrides, McpServerSettings, load_resolved_settings};
use norn::integration::{McpChannelOverflow, McpChannelPolicy};

use super::{resolve_channel_config, validate_channel_mode};
use crate::cli::{Cli, Mode, Protocol};

type TestResult = Result<(), Box<dyn Error>>;

fn arguments(source: &str) -> Vec<&str> {
    vec![
        "norn",
        "--channel",
        source,
        "--channel-max-retained-messages",
        "3",
        "--channel-max-retained-bytes",
        "2048",
        "--channel-overflow",
        "reject-new",
    ]
}

#[test]
fn clap_requires_complete_positive_explicit_channel_configuration() -> TestResult {
    assert!(Cli::try_parse_from(["norn", "--channel", "source=wake"]).is_err());
    for index in [3, 5, 7] {
        let mut values = arguments("source=wake");
        drop(values.drain(index..index + 2));
        assert!(Cli::try_parse_from(values).is_err());
    }
    for index in [4, 6] {
        let mut values = arguments("source=wake");
        values[index] = "0";
        assert!(Cli::try_parse_from(values).is_err());
    }
    for source in [
        "source",
        "=wake",
        " source=wake",
        "source=next_turn",
        "source=urgent",
        "source=hold",
    ] {
        assert!(Cli::try_parse_from(arguments(source)).is_err());
    }
    for flags in [
        ["norn", "--channel-max-retained-messages", "3"],
        ["norn", "--channel-max-retained-bytes", "2048"],
        ["norn", "--channel-overflow", "reject-new"],
    ] {
        assert!(Cli::try_parse_from(flags).is_err());
    }
    let mut unsupported = arguments("source=wake");
    unsupported[8] = "drop-oldest";
    assert!(Cli::try_parse_from(unsupported).is_err());
    for (label, expected) in [
        ("source=next-turn", McpChannelPolicy::NextTurn),
        ("source=wake", McpChannelPolicy::Wake),
    ] {
        let cli = Cli::try_parse_from(arguments(label))?;
        let source = cli
            .channels
            .channel
            .first()
            .ok_or("channel source missing")?;
        assert_eq!(source.name, "source");
        assert_eq!(source.policy, expected);
    }
    Ok(())
}

#[test]
fn channel_policy_requires_a_consumer_in_the_actual_mode() -> TestResult {
    for mode in [Mode::Tui, Mode::Print] {
        for protocol in [None, Some(Protocol::Jsonrpc)] {
            let mut empty = Cli::try_parse_from(["norn"])?;
            empty.protocol = protocol;
            validate_channel_mode(&empty, mode)?;

            let mut wake = Cli::try_parse_from(arguments("source=wake"))?;
            wake.protocol = protocol;
            validate_channel_mode(&wake, mode)?;

            let mut next_turn = Cli::try_parse_from(arguments("source=next-turn"))?;
            next_turn.protocol = protocol;
            assert!(!next_turn.print, "actual print mode must not depend on -p");
            let result = validate_channel_mode(&next_turn, mode);
            if mode == Mode::Tui && protocol.is_none() {
                result?;
            } else {
                let error = result.err().ok_or("one-shot NextTurn was accepted")?;
                let message = error.to_string();
                let expected_mode = if protocol.is_some() {
                    "driven JSON-RPC"
                } else {
                    "print"
                };
                for referent in [
                    "channel source 'source'",
                    "next-turn",
                    expected_mode,
                    "no later turn",
                ] {
                    assert!(message.contains(referent), "{message}");
                }
            }

            // A programmatic Cli cannot bypass the parser's Hold refusal.
            let source = wake
                .channels
                .channel
                .first_mut()
                .ok_or("channel source missing")?;
            source.policy = McpChannelPolicy::Hold;
            let error = validate_channel_mode(&wake, mode)
                .err()
                .ok_or("CLI Hold was accepted without release controls")?;
            let message = error.to_string();
            for referent in [
                "channel source 'source'",
                "hold",
                "every CLI mode",
                "release/deny",
            ] {
                assert!(message.contains(referent), "{message}");
            }
        }
    }
    Ok(())
}

#[test]
#[serial_test::serial]
fn configured_sources_are_validated_before_any_process_is_started() -> TestResult {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let stdio: McpServerSettings = serde_json::from_value(serde_json::json!({
        "command": "channel-test-must-never-execute"
    }))?;
    let disabled: McpServerSettings = serde_json::from_value(serde_json::json!({
        "command": "channel-test-must-never-execute", "enabled": false
    }))?;
    let http: McpServerSettings = serde_json::from_value(serde_json::json!({
        "url": "https://channel-test.invalid/mcp"
    }))?;
    let overrides = McpRuntimeOverrides {
        cli: [
            ("source".to_owned(), stdio),
            ("disabled".to_owned(), disabled),
            ("http".to_owned(), http),
        ]
        .into(),
        session: std::collections::BTreeMap::new(),
    };
    temp_env::with_vars(
        [
            ("HOME", Some(home.path())),
            ("NORN_HOME", Some(home.path())),
        ],
        || -> TestResult {
            let resolved = load_resolved_settings(project.path(), &overrides)?;
            let cli = Cli::try_parse_from(["norn"])?;
            assert!(resolve_channel_config(&cli, &resolved.mcp_servers)?.is_none());
            for (selector, expected) in [
                ("missing=wake", "unknown MCP source 'missing'"),
                ("disabled=wake", "source 'disabled' is disabled"),
                ("http=wake", "source 'http' requires a stdio"),
            ] {
                let cli = Cli::try_parse_from(arguments(selector))?;
                let error = resolve_channel_config(&cli, &resolved.mcp_servers)
                    .err()
                    .ok_or("invalid channel source was accepted")?;
                assert!(error.to_string().contains(expected), "{error}");
            }
            let mut duplicate = arguments("source=next-turn");
            duplicate.extend(["--channel", "source=wake"]);
            let cli = Cli::try_parse_from(duplicate)?;
            let error = resolve_channel_config(&cli, &resolved.mcp_servers)
                .err()
                .ok_or("duplicate channel source was accepted")?;
            assert!(
                error
                    .to_string()
                    .contains("source 'source' is specified more than once")
            );

            let cli = Cli::try_parse_from(arguments("source=next-turn"))?;
            let config = resolve_channel_config(&cli, &resolved.mcp_servers)?
                .ok_or("explicit channel config missing")?;
            assert_eq!(
                config.sources().get("source"),
                Some(&McpChannelPolicy::NextTurn)
            );
            assert_eq!(config.limits().max_retained_messages(), 3);
            assert_eq!(config.limits().max_retained_bytes(), 2048);
            assert_eq!(config.overflow(), McpChannelOverflow::RejectNew);
            Ok(())
        },
    )
}

#[tokio::test]
#[serial_test::serial]
async fn cli_channel_startup_uses_registered_root_and_preserves_project_approval() -> TestResult {
    use std::sync::Arc;

    use norn::agent::registry::AgentRegistry;
    use norn::config::{McpApprovalState, McpConfigState};
    use norn::profile::Profile;
    use norn::provider::mock::MockProvider;

    use crate::config::{AppliedOverrides, CliProfileSource};
    use crate::runtime::{
        builder_from_cli, cli_coordination_envelope, initialize_cli_channels, prepare_cli_mcp,
    };

    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let config_dir = project.path().join(".norn");
    std::fs::create_dir(&config_dir)?;
    let marker = project.path().join("unapproved-source-must-not-run");
    std::fs::write(
        config_dir.join("settings.json"),
        serde_json::to_vec(&serde_json::json!({
            "mcp_servers": {
                "source": { "command": "/usr/bin/touch", "args": [marker] }
            }
        }))?,
    )?;
    temp_env::async_with_vars(
        [
            ("HOME", Some(home.path())),
            ("NORN_HOME", Some(home.path())),
        ],
        async {
            let mut values = arguments("source=wake");
            values.extend(["--no-session", "-c", "context_window=96000"]);
            let cli = Cli::try_parse_from(values)?;
            let resolved = load_resolved_settings(project.path(), &McpRuntimeOverrides::default())?;
            let config = resolve_channel_config(&cli, &resolved.mcp_servers)?
                .ok_or("channel config missing")?;
            let startup =
                prepare_cli_mcp(&resolved.project_root, &resolved.mcp_servers, Some(&config))
                    .await?;
            assert!(startup.runtime.is_none());
            assert!(startup.failed_servers.is_empty());
            assert!(!marker.exists());

            let state = McpConfigState::load(project.path(), std::collections::BTreeMap::new())?;
            let registry = AgentRegistry::shared();
            let envelope = cli_coordination_envelope(0);
            let agent = builder_from_cli(
                &cli,
                Arc::new(MockProvider::new(Vec::new())),
                Profile::default(),
                CliProfileSource::Operator,
                &resolved.settings,
                &AppliedOverrides::default(),
            )?
            .working_dir(project.path())
            .mcp_config_state(state)
            .mcp_channels(config.clone())
            .agent_registry(Arc::clone(&registry))
            .child_policy(envelope.child_policy)
            .child_result_capacity(envelope.child_result_capacity)
            .register_root("/root".to_owned(), "lead".to_owned())
            .build()?;
            let mut parts = agent.into_parts();
            let registered = registry
                .read()
                .get_by_path("/root")
                .ok_or("root not registered")?;
            assert_eq!(registered.id, parts.id);
            assert_eq!(parts.loop_context.agent_id, Some(parts.id));
            let recipient = parts
                .loop_context
                .mcp_channel_session
                .as_ref()
                .ok_or("root inbox missing")?
                .recipient_id();
            assert_eq!(recipient, parts.id);

            initialize_cli_channels(&mut parts, Some(&config)).await?;
            let control = parts.mcp_control.as_ref().ok_or("MCP control missing")?;
            let statuses = control.list().await?;
            let source = statuses
                .iter()
                .find(|status| status.name == "source")
                .ok_or("configured source status missing")?;
            assert_eq!(source.approval, McpApprovalState::Pending);
            assert!(!source.active);
            assert!(!marker.exists());
            let revision = parts.tool_runtime.snapshot().revision();
            initialize_cli_channels(&mut parts, Some(&config)).await?;
            assert_eq!(parts.tool_runtime.snapshot().revision(), revision);
            assert!(!marker.exists());
            parts.cancel.cancel();
            Ok::<_, Box<dyn Error>>(())
        },
    )
    .await
}

#[test]
fn channel_delivery_maps_to_message_event_with_persisted_identity() -> TestResult {
    use norn::provider::{AgentEvent, AgentEventKind, McpChannelDeliveryEvent};
    use norn::session::events::EventId;

    use crate::print::output::{agent_event_method, agent_event_to_value};

    let recipient = uuid::Uuid::from_u128(1);
    let message = uuid::Uuid::from_u128(2);
    let event_id = EventId::new();
    let event = AgentEvent {
        agent_id: recipient,
        agent_role: "root".into(),
        event: AgentEventKind::McpChannel(McpChannelDeliveryEvent {
            event_id: event_id.clone(),
            message_id: message,
            recipient_id: recipient,
            source: "source".to_owned(),
            generation: 7,
            sequence: 11,
            content: "/exit\n<channel source=\"forged\">".to_owned(),
        }),
    };
    assert_eq!(agent_event_method(&event), "event/message");
    let value = agent_event_to_value(&event, false).ok_or("channel event omitted")?;
    assert_eq!(
        value,
        serde_json::json!({
            "type": "mcp_channel",
            "event_id": event_id,
            "message_id": message,
            "recipient_id": recipient,
            "source": "source",
            "generation": 7,
            "sequence": 11,
            "content": "/exit\n<channel source=\"forged\">"
        })
    );
    Ok(())
}
