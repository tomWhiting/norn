use super::*;
use crate::integration::{
    DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES, McpClient, McpClientConfig, McpTransport,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn streamable_http_negotiates_session_and_accepts_sse()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            serde_json::json!({"method": "initialize"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fixture", "version": "1"}
                    }
                }))
                .insert_header("content-type", "application/json")
                .insert_header("mcp-session-id", "fixture-session"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "method": "notifications/initialized"
        })))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"method": "tools/list"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(
                    concat!(
                    "event: message\n",
                    "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}\n\n",
                    ),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let client = McpClient::connect(McpClientConfig {
        name: "fixture".to_owned(),
        transport: McpTransport::Http { url: server.uri() },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    })
    .await?;

    assert_eq!(client.tools().len(), 1);
    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording is unavailable")?;
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let accept = request
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok());
        assert_eq!(accept, Some(ACCEPT));
    }
    assert!(requests[0].headers.get(SESSION_HEADER).is_none());
    assert!(requests[0].headers.get(PROTOCOL_HEADER).is_none());
    for request in &requests[1..] {
        assert!(request.headers.get(SESSION_HEADER).is_some());
        assert!(request.headers.get(PROTOCOL_HEADER).is_some());
    }
    Ok(())
}

#[tokio::test]
async fn connection_errors_do_not_disclose_endpoint_url_secrets() -> Result<(), TestError> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    drop(listener);
    let url = format!("http://user:USER_SECRET@{address}/PATH_SECRET?token=QUERY_SECRET");

    let rendered = match McpClient::connect(McpClientConfig {
        name: "redaction-fixture".to_owned(),
        transport: McpTransport::Http { url },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    })
    .await
    {
        Ok(_client) => {
            return Err(
                std::io::Error::other("connection-refusal fixture unexpectedly connected").into(),
            );
        }
        Err(error) => error.to_string(),
    };

    for secret in ["USER_SECRET", "PATH_SECRET", "QUERY_SECRET"] {
        assert!(
            !rendered.contains(secret),
            "rendered HTTP error disclosed endpoint secret {secret}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn error_status_cannot_replace_the_http_session() -> Result<(), TestError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"id": 1})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {}
                }))
                .insert_header("mcp-session-id", "valid-session"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"id": 2})))
        .respond_with(ResponseTemplate::new(401).insert_header("mcp-session-id", "hostile-session"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({"id": 3})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {}
        })))
        .mount(&server)
        .await;
    let transport = HttpTransport::new(
        server.uri(),
        &HashMap::new(),
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?;

    let accepted = transport
        .post(serde_json::json!({"jsonrpc": "2.0", "id": 1}).to_string())
        .await?;
    transport.response_from_http(accepted, 1).await?;
    let rejected = transport
        .post(serde_json::json!({"jsonrpc": "2.0", "id": 2}).to_string())
        .await?;
    let error = transport
        .response_from_http(rejected, 2)
        .await
        .err()
        .ok_or("error response unexpectedly succeeded")?;
    assert!(error.to_string().contains("401"));
    let accepted = transport
        .post(serde_json::json!({"jsonrpc": "2.0", "id": 3}).to_string())
        .await?;
    transport.response_from_http(accepted, 3).await?;

    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording is unavailable")?;
    assert_eq!(requests.len(), 3);
    assert!(requests[0].headers.get(SESSION_HEADER).is_none());
    for request in &requests[1..] {
        assert_eq!(
            request
                .headers
                .get(SESSION_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("valid-session"),
        );
    }
    Ok(())
}

#[tokio::test]
async fn remote_error_surfaces_do_not_disclose_configured_header_secrets() -> Result<(), TestError>
{
    const SECRET: &str = "mcp-remote-error-header-secret";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32001, "message": SECRET}
        })))
        .mount(&server)
        .await;
    let headers = HashMap::from([("authorization".to_owned(), format!("Bearer {SECRET}"))]);
    let transport = HttpTransport::new(
        server.uri(),
        &headers,
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?;
    let client = McpClient::from_transport("remote-error-fixture", Box::new(transport));

    let error = client
        .refreshed_tools()
        .await
        .err()
        .ok_or("remote MCP error unexpectedly succeeded")?;
    let display = error.to_string();
    let debug = format!("{error:?}");
    let source = std::error::Error::source(&error).ok_or("remote MCP error lost its source")?;
    assert!(display.contains("-32001"));
    assert!(source.to_string().contains("-32001"));
    assert!(!display.contains(SECRET));
    assert!(!debug.contains(SECRET));
    assert!(!source.to_string().contains(SECRET));
    match &error {
        IntegrationError::McpRemote(remote) => {
            assert_eq!(remote.code(), -32001);
            assert_eq!(remote.untrusted_message(), SECRET);
        }
        _ => {
            return Err(std::io::Error::other("expected a typed remote MCP error").into());
        }
    }
    let requests = server
        .received_requests()
        .await
        .ok_or("wiremock request recording is unavailable")?;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer mcp-remote-error-header-secret")
    );
    Ok(())
}

#[tokio::test]
async fn redirects_do_not_forward_configured_authorization() -> Result<(), TestError> {
    const AUTH_SECRET: &str = "mcp-redirect-auth-secret";
    for status in [301_u16, 302, 303, 307, 308] {
        let target = MockServer::start().await;
        let origin = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status).insert_header("location", target.uri().as_str()),
            )
            .mount(&origin)
            .await;
        let headers =
            HashMap::from([("authorization".to_owned(), format!("Bearer {AUTH_SECRET}"))]);

        let error = McpClient::connect(McpClientConfig {
            name: "redirect-fixture".to_owned(),
            transport: McpTransport::Http { url: origin.uri() },
            env: HashMap::new(),
            headers,
            working_dir: None,
            max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
            request_timeout_ms: None,
        })
        .await
        .err()
        .ok_or("MCP redirect was unexpectedly followed")?;
        let rendered = error.to_string();
        assert!(rendered.contains(&status.to_string()));
        assert!(!rendered.contains(AUTH_SECRET));

        let origin_requests = origin
            .received_requests()
            .await
            .ok_or("origin request recording is unavailable")?;
        assert_eq!(origin_requests.len(), 1);
        assert_eq!(
            origin_requests[0]
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer mcp-redirect-auth-secret"),
        );
        let target_requests = target
            .received_requests()
            .await
            .ok_or("redirect target request recording is unavailable")?;
        assert!(target_requests.is_empty());
    }
    Ok(())
}

#[test]
fn parses_multiline_sse_data() -> Result<(), Box<dyn std::error::Error>> {
    let mut decoder = SseDecoder::default();
    let mut input = concat!(
        "data: {\"jsonrpc\":\"2.0\",\n",
        "data: \"id\":1,\"result\":{}}\n\n",
    )
    .as_bytes();
    let message = decoder.push_next(&mut input)?;

    assert_eq!(
        message,
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {}
        }))
    );
    assert!(input.is_empty());
    Ok(())
}

#[tokio::test]
async fn answers_ping_before_sse_response_stream_finishes() -> Result<(), TestError> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut initialize, _) = listener.accept().await?;
        read_http_request(&mut initialize).await?;
        write_json_response(
            &mut initialize,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "streaming-fixture", "version": "1"}
                }
            })
            .to_string(),
            Some("streaming-session"),
        )
        .await?;

        let (mut initialized, _) = listener.accept().await?;
        read_http_request(&mut initialized).await?;
        write_empty_response(&mut initialized).await?;

        let (mut list, _) = listener.accept().await?;
        read_http_request(&mut list).await?;
        list.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .await?;
        write_chunk(
            &mut list,
            b"data: {\"jsonrpc\":\"2.0\",\"id\":\"server-ping\",\"method\":\"ping\"}\n\n",
        )
        .await?;

        let (mut ping, _) = listener.accept().await?;
        let ping_request = read_http_request(&mut ping).await?;
        let ping_request = std::str::from_utf8(&ping_request)?;
        if !ping_request.contains("\"id\":\"server-ping\"")
            || !ping_request.contains("\"result\":{}")
        {
            return Err(std::io::Error::other("client did not answer the streamed ping").into());
        }
        write_empty_response(&mut ping).await?;

        write_chunk(
            &mut list,
            b"data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"echo\"}]}}\n\n",
        )
        .await?;
        list.write_all(b"0\r\n\r\n").await?;
        Ok::<_, TestError>(())
    });

    let client = McpClient::connect(McpClientConfig {
        name: "streaming-fixture".to_owned(),
        transport: McpTransport::Http {
            url: format!("http://{address}"),
        },
        env: HashMap::new(),
        headers: HashMap::new(),
        working_dir: None,
        max_inbound_message_bytes: DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        request_timeout_ms: None,
    })
    .await?;

    assert_eq!(client.tools().len(), 1);
    server.await??;
    Ok(())
}

#[tokio::test]
async fn mismatched_sse_response_id_fails_promptly_and_invalidates_client() -> Result<(), TestError>
{
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let (release_server, hold_server) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut list, _) = listener.accept().await?;
        read_http_request(&mut list).await?;
        list.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        )
        .await?;
        write_chunk(
            &mut list,
            b"data: {\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{\"tools\":[]}}\n\n",
        )
        .await?;

        hold_server.await?;
        Ok::<_, TestError>(())
    });
    let transport = HttpTransport::new(
        format!("http://{address}"),
        &HashMap::new(),
        Arc::new(ClientProtocolState::new(Vec::new())),
        DEFAULT_MCP_MAX_INBOUND_MESSAGE_BYTES,
        None,
    )?;
    let client = McpClient::from_transport("mismatched-id-fixture", Box::new(transport));

    let result =
        tokio::time::timeout(std::time::Duration::from_secs(2), client.refreshed_tools()).await;
    let release_result = release_server.send(());
    server.await??;
    release_result.map_err(|()| {
        std::io::Error::other("held-open SSE server exited before the test released it")
    })?;
    let error = result
        .map_err(|_elapsed| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "client silently waited after a mismatched SSE response id",
            )
        })?
        .err()
        .ok_or("mismatched SSE response id unexpectedly succeeded")?;

    assert!(
        error
            .to_string()
            .contains("JSON-RPC response id did not match request 1")
    );
    assert!(!client.is_live());
    Ok(())
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, TestError> {
    let mut request = Vec::new();
    let header_end = loop {
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let mut buffer = [0_u8; 1024];
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "HTTP request ended before its headers",
            )
            .into());
        }
        request.extend_from_slice(&buffer[..count]);
    };
    let headers = std::str::from_utf8(&request[..header_end])?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let mut buffer = [0_u8; 1024];
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "HTTP request ended before its body",
            )
            .into());
        }
        request.extend_from_slice(&buffer[..count]);
    }
    Ok(request)
}

async fn write_json_response(
    stream: &mut TcpStream,
    body: &str,
    session_id: Option<&str>,
) -> Result<(), TestError> {
    let session = session_id
        .map(|value| format!("Mcp-Session-Id: {value}\r\n"))
        .unwrap_or_default();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{session}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    Ok(())
}

async fn write_empty_response(stream: &mut TcpStream) -> Result<(), TestError> {
    stream
        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await?;
    Ok(())
}

async fn write_chunk(stream: &mut TcpStream, body: &[u8]) -> Result<(), TestError> {
    stream
        .write_all(format!("{:X}\r\n", body.len()).as_bytes())
        .await?;
    stream.write_all(body).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}
