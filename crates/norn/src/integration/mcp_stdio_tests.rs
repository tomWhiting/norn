use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::*;

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
