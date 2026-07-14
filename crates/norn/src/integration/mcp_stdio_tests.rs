use std::collections::HashMap;
use std::io;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as TestMutex;
use std::time::Duration;

use super::*;
use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpRuntime, McpTransport,
};

#[derive(Clone, Default)]
struct SharedLog(Arc<TestMutex<Vec<u8>>>);

impl io::Write for SharedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut destination = self
            .0
            .lock()
            .map_err(|error| io::Error::other(format!("shared log lock is poisoned: {error}")))?;
        std::io::Write::write(&mut *destination, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedLog {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

impl SharedLog {
    fn rendered(&self) -> io::Result<String> {
        let bytes = self
            .0
            .lock()
            .map_err(|error| io::Error::other(format!("shared log lock is poisoned: {error}")))?
            .clone();
        String::from_utf8(bytes).map_err(io::Error::other)
    }
}

#[cfg(unix)]
struct DescendantCleanup {
    stop: PathBuf,
    exited: PathBuf,
}

#[cfg(unix)]
impl Drop for DescendantCleanup {
    fn drop(&mut self) {
        let _write_result = std::fs::write(&self.stop, b"stop");
        for _ in 0..200 {
            if self.exited.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(unix)]
async fn wait_for_path(path: &std::path::Path) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_transport_returns_retained_descriptor_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let governor = Arc::new(crate::resource::DescriptorGovernor::with_capacity(
        crate::resource::THREE_PIPE_SPAWN_PEAK,
    ));
    let transport = StdioTransport::spawn_with_governor(
        "/bin/sh",
        &["-c".to_owned(), "sleep 30".to_owned()],
        &HashMap::new(),
        None,
        Arc::new(ClientProtocolState::new(Vec::new())),
        StdioConnectionOptions::new(DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, None),
        &governor,
    )?;

    assert_eq!(
        governor.available(),
        (crate::resource::THREE_PIPE_SPAWN_PEAK - crate::resource::THREE_PIPE_RETAINED) as usize,
    );
    drop(transport);
    tokio::time::timeout(Duration::from_secs(2), async {
        while governor.available() != crate::resource::THREE_PIPE_SPAWN_PEAK as usize {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn inherited_stderr_descendant_cannot_retain_transport_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let ready = temp.path().join("descendant-ready");
    let exited = temp.path().join("descendant-exited");
    let stop = temp.path().join("descendant-stop");
    let cleanup = DescendantCleanup {
        stop: stop.clone(),
        exited: exited.clone(),
    };
    let governor = Arc::new(crate::resource::DescriptorGovernor::with_capacity(
        crate::resource::THREE_PIPE_SPAWN_PEAK,
    ));
    let transport = StdioTransport::spawn_with_governor(
        "/bin/sh",
        &[
            "-c".to_owned(),
            concat!(
                "(trap 'touch descendant-exited' EXIT; ",
                "trap '' HUP TERM; touch descendant-ready; ",
                "count=0; while [ ! -e descendant-stop ] && [ \"$count\" -lt 100 ]; do ",
                "count=$((count + 1)); sleep 0.1; done) ",
                "</dev/null >/dev/null & sleep 30",
            )
            .to_owned(),
        ],
        &HashMap::new(),
        Some(temp.path()),
        Arc::new(ClientProtocolState::new(Vec::new())),
        StdioConnectionOptions::new(DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, None),
        &governor,
    )?;
    wait_for_path(&ready).await?;

    transport.invalidate().await;
    let spawn_peak = usize::try_from(crate::resource::THREE_PIPE_SPAWN_PEAK)?;
    let retained_without_stderr = usize::try_from(crate::resource::THREE_PIPE_RETAINED - 1)?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while governor.available() != spawn_peak - retained_without_stderr {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert!(!exited.exists());

    drop(transport);
    tokio::time::timeout(Duration::from_secs(2), async {
        while governor.available() != spawn_peak {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    std::fs::write(&stop, b"stop")?;
    wait_for_path(&exited).await?;
    drop(cleanup);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn closed_server_marks_transport_not_live() -> Result<(), Box<dyn std::error::Error>> {
    let transport = StdioTransport::spawn(
        "/bin/sh",
        &["-c".to_owned(), "read request".to_owned()],
        &HashMap::new(),
        None,
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?;
    assert!(transport.is_live());

    transport.notify("{}".to_owned()).await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while transport.is_live() {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    assert!(!transport.is_live());
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn server_stderr_metadata_does_not_disclose_configured_secrets()
-> Result<(), Box<dyn std::error::Error>> {
    const SENTINEL: &str = "mcp-stderr-secret-sentinel";
    let logs = SharedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    let subscriber_guard = tracing::subscriber::set_default(subscriber);
    // Other parallel tests can register this callsite while no scoped
    // subscriber is active, so refresh its cached interest for this guard.
    tracing::callsite::rebuild_interest_cache();
    let expected = concat!(
        "MCP server stderr: multiple withheld lines; ",
        "diagnostic drain interrupted (truncated)",
    );
    let trace_observation = StderrObservation::default();
    trace_observation.observe(SENTINEL.as_bytes());
    trace_observation.observe(b"\nsecond line");
    trace_observation.interrupt();
    trace_invalidation(trace_observation.snapshot());
    let script = concat!(
        "printf '\\033[31m%s\\033[0m\\r\\ninjected-metadata\\r\\n' ",
        "\"$MCP_STDERR_SECRET\" >&2; sleep 0.1; printf 'not-json\\n'; sleep 5",
    );
    let config = McpClientConfig {
        name: "stderr-fixture".to_owned(),
        transport: McpTransport::Stdio {
            command: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
        },
        env: HashMap::from([("MCP_STDERR_SECRET".to_owned(), SENTINEL.to_owned())]),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    };
    let error = McpClient::connect(config.clone())
        .await
        .err()
        .ok_or("invalid MCP server output unexpectedly connected")?;
    let displayed = error.to_string();
    let debugged = format!("{error:?}");
    assert!(displayed.contains("invalid JSON-RPC response"));
    assert!(displayed.contains(expected));
    assert!(debugged.contains(expected));

    let runtime = McpRuntime::connect([config]).await;
    let (_server, failure) = runtime
        .failures()
        .next()
        .ok_or("runtime did not retain the MCP connection failure")?;
    assert!(failure.contains(expected));

    let rendered = logs.rendered()?;
    assert!(rendered.contains(expected));
    for output in [&displayed, &debugged, failure, &rendered] {
        assert!(!output.contains(SENTINEL));
        assert!(!output.contains("injected-metadata"));
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\r'));
    }
    drop(subscriber_guard);
    tracing::callsite::rebuild_interest_cache();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_after_write_invalidates_the_channel() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("request-read");
    let script = format!("read request; touch {}; sleep 5", marker.display());
    let transport = Arc::new(StdioTransport::spawn(
        "/bin/sh",
        &["-c".to_owned(), script],
        &HashMap::new(),
        Some(temp.path()),
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?);
    let pending = {
        let transport = Arc::clone(&transport);
        tokio::spawn(async move {
            transport
                .request(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/list",
                        "params": {}
                    })
                    .to_string(),
                    1,
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    pending.abort();
    let _cancelled = pending.await;

    let error = transport
        .request(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
            2,
        )
        .await
        .err()
        .ok_or("cancelled MCP channel unexpectedly remained usable")?;

    assert!(error.to_string().contains("no longer usable"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_timeout_is_opt_in_and_none_has_no_client_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let script = concat!(
        "read request; sleep 0.075; ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'",
    );
    let unbounded = StdioTransport::spawn(
        "/bin/sh",
        &["-c".to_owned(), script.to_owned()],
        &HashMap::new(),
        None,
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?;
    tokio::time::timeout(
        Duration::from_secs(1),
        unbounded.request("{}".to_owned(), 1),
    )
    .await??;

    let bounded = StdioTransport::spawn(
        "/bin/sh",
        &["-c".to_owned(), script.to_owned()],
        &HashMap::new(),
        None,
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        Some(10),
    )?;
    let error = bounded
        .request("{}".to_owned(), 1)
        .await
        .err()
        .ok_or("explicit stdio request timeout was not enforced")?;
    assert!(matches!(
        error,
        IntegrationError::McpRequestTimedOut {
            transport: "stdio",
            timeout_ms: 10
        }
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_preserves_parent_environment_and_applies_overlay()
-> Result<(), Box<dyn std::error::Error>> {
    let script = concat!(
        "read request; ",
        "if [ -n \"$PATH\" ] && [ \"$MCP_OVERLAY\" = configured ]; then ",
        "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'; ",
        "else printf 'invalid\\n'; fi",
    );
    let transport = StdioTransport::spawn(
        "/bin/sh",
        &["-c".to_owned(), script.to_owned()],
        &HashMap::from([("MCP_OVERLAY".to_owned(), "configured".to_owned())]),
        None,
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        Some(1000),
    )?;

    transport.request("{}".to_owned(), 1).await?;
    Ok(())
}
