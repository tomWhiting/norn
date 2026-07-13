use super::*;
use crate::integration::{McpClient, McpClientConfig, McpTransport};
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

#[test]
fn parses_multiline_sse_data() -> Result<(), Box<dyn std::error::Error>> {
    let mut decoder = SseDecoder::default();
    let messages = decoder.push(
        concat!(
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":1,\"result\":{}}\n\n",
        )
        .as_bytes(),
    )?;

    assert_eq!(
        messages,
        [serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {}
        })]
    );
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
    })
    .await?;

    assert_eq!(client.tools().len(), 1);
    server.await??;
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
