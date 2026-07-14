use super::*;

#[tokio::test]
async fn stdio_line_rejects_before_exceeding_bound() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"12345\n";
    let mut reader = tokio::io::BufReader::new(&input[..]);
    let mut line = Vec::new();

    let error = read_bounded_stdio_line(&mut reader, &mut line, 4)
        .await
        .err()
        .ok_or("oversized stdio line was accepted")?;

    assert!(matches!(
        error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "stdio",
            limit_bytes: 4
        }
    ));
    assert!(line.len() <= 4);
    Ok(())
}

#[tokio::test]
async fn stdio_line_accepts_exact_bound_without_terminator() -> Result<(), IntegrationError> {
    let input = b"1234\n";
    let mut reader = tokio::io::BufReader::new(&input[..]);
    let mut line = Vec::new();

    assert!(read_bounded_stdio_line(&mut reader, &mut line, 4).await?);
    assert_eq!(line, b"1234");
    Ok(())
}

#[tokio::test]
async fn stdio_line_accepts_exact_bound_with_crlf() -> Result<(), IntegrationError> {
    let input = b"1234\r\n";
    let mut reader = tokio::io::BufReader::with_capacity(5, &input[..]);
    let mut line = Vec::new();

    assert!(read_bounded_stdio_line(&mut reader, &mut line, 4).await?);
    assert_eq!(line, b"1234");
    Ok(())
}

#[test]
fn sse_partial_line_and_event_payload_are_independently_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let mut partial = SseDecoder::new(4);
    let mut partial_chunk = &b"abcde"[..];
    let partial_error = partial
        .push_next(&mut partial_chunk)
        .err()
        .ok_or("oversized partial SSE line was accepted")?;
    assert!(matches!(
        partial_error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "HTTP SSE",
            limit_bytes: 4
        }
    ));

    let mut event = SseDecoder::new(20);
    let mut first_line = &b"data: 1234567\n"[..];
    assert_eq!(event.push_next(&mut first_line)?, None);
    let mut second_line = &b"data: 7654321\n"[..];
    let event_error = event
        .push_next(&mut second_line)
        .err()
        .ok_or("oversized multiline SSE event was accepted")?;
    assert!(matches!(
        event_error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "HTTP SSE",
            limit_bytes: 20
        }
    ));
    Ok(())
}

#[test]
fn sse_event_accepts_exact_wire_bound() -> Result<(), IntegrationError> {
    let mut decoder = SseDecoder::new(9);
    let mut input = &b"data: {}\n\n"[..];

    let message = decoder.push_next(&mut input)?;

    assert_eq!(message, Some(serde_json::json!({})));
    assert!(input.is_empty());
    Ok(())
}

#[test]
fn sse_event_bound_includes_ignored_fields() -> Result<(), Box<dyn std::error::Error>> {
    let mut decoder = SseDecoder::new(12);
    let mut input = &b": ignored\n: excess\n"[..];

    let error = decoder
        .push_next(&mut input)
        .err()
        .ok_or("ignored SSE fields escaped the event bound")?;

    assert!(matches!(
        error,
        IntegrationError::McpInboundMessageTooLarge {
            transport: "HTTP SSE",
            limit_bytes: 12
        }
    ));
    Ok(())
}

#[test]
fn sse_chunk_yields_one_event_at_a_time() -> Result<(), IntegrationError> {
    let mut decoder = SseDecoder::new(64);
    let mut input = &b"data: {\"id\":1}\n\ndata: {\"id\":2}\n\n"[..];

    let first = decoder.push_next(&mut input)?;
    assert_eq!(first, Some(serde_json::json!({"id": 1})));
    assert!(!input.is_empty());

    let second = decoder.push_next(&mut input)?;
    assert_eq!(second, Some(serde_json::json!({"id": 2})));
    assert!(input.is_empty());
    Ok(())
}
