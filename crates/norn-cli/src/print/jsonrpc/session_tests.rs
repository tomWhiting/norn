//! Persistent input admission, cancellation, framing and lifecycle regressions.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::frames::JsonRpcResponse;
use super::interventions::{InjectPriority, InterventionHandler};
use super::session::{SessionInput, spawn_session_input};
use super::stdin::StdinReader;
use super::writer::OutboundWriter;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Input = mpsc::UnboundedSender<std::io::Result<Vec<u8>>>;

struct Fixture {
    tx: Input,
    output: mpsc::UnboundedReceiver<String>,
    session: SessionInput,
}

impl Fixture {
    fn new() -> Self {
        let (tx, reader) = StdinReader::test_channel();
        let (writer, output) = OutboundWriter::test_channel();
        Self {
            tx,
            output,
            session: spawn_session_input(reader, writer),
        }
    }

    fn send(&self, id: &str, method: &str, params: Value) -> TestResult {
        let mut request = json!({"jsonrpc":"2.0", "id":id, "method":method});
        request["params"] = params;
        let line = format!("{request}\n");
        self.tx.send(Ok(line.into_bytes()))?;
        Ok(())
    }

    async fn frame(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        let frame = tokio::time::timeout(Duration::from_secs(10), self.output.recv())
            .await?
            .ok_or_else(|| std::io::Error::other("missing response"))?;
        Ok(serde_json::from_str(&frame)?)
    }

    async fn accepted(&mut self, expected: &str) -> TestResult {
        let request = tokio::time::timeout(Duration::from_secs(10), self.session.runs.recv())
            .await?
            .ok_or_else(|| std::io::Error::other("missing accepted run"))?;
        assert_eq!(request.id, json!(expected));
        Ok(())
    }

    async fn persistent(&mut self) -> TestResult {
        self.send("init", "initialize", json!({"runLifecycle":"persistent"}))?;
        assert_eq!(
            self.frame().await?["result"]["capabilities"]["runLifecycle"],
            "persistent"
        );
        Ok(())
    }

    async fn finish_run(&mut self, id: &str) -> TestResult {
        self.session
            .control
            .finish(JsonRpcResponse::ok(
                json!(id),
                json!({"stop":{"reason":"completed"}}),
            ))
            .await?;
        assert_eq!(self.frame().await?["id"], json!(id));
        Ok(())
    }

    async fn close(self) -> TestResult {
        drop(self.tx);
        tokio::time::timeout(Duration::from_secs(10), self.session.task).await???;
        Ok(())
    }
}

struct Handler {
    cancel: CancellationToken,
    messages: mpsc::UnboundedSender<(String, InjectPriority)>,
}

impl InterventionHandler for Handler {
    fn inject_message(&self, text: &str, priority: InjectPriority) -> Result<(), String> {
        self.messages
            .send((text.to_owned(), priority))
            .map_err(|error| error.to_string())
    }

    fn cancel(&self, reason: &str) -> Result<(), String> {
        assert!(!reason.is_empty());
        self.cancel.cancel();
        Ok(())
    }
}

#[tokio::test]
async fn sequential_requests_wait_for_the_previous_terminal_result() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.persistent().await?;
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    assert!(
        fixture
            .session
            .run_active
            .load(std::sync::atomic::Ordering::Acquire)
    );
    fixture.send("busy", "run/execute", json!({"prompt":"overlap"}))?;
    let busy = fixture.frame().await?;
    assert_eq!(busy["error"]["code"], -32000);
    assert_eq!(
        busy["error"]["message"],
        "run already active; wait for its terminal response"
    );
    fixture.send("init", "initialize", Value::Null)?;
    assert_eq!(
        fixture.frame().await?["result"]["capabilities"]["runLifecycle"],
        "persistent"
    );
    fixture.finish_run("first").await?;
    assert!(
        !fixture
            .session
            .run_active
            .load(std::sync::atomic::Ordering::Acquire)
    );
    fixture.send("second", "run/execute", json!({"prompt":"two"}))?;
    fixture.accepted("second").await?;
    fixture.finish_run("second").await?;
    fixture.close().await
}

#[tokio::test]
async fn partial_next_request_survives_completion_of_the_current_run() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.persistent().await?;
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    fixture.tx.send(Ok(
        b"{\"jsonrpc\":\"2.0\",\"id\":\"next\",\"method\":\"run/".to_vec(),
    ))?;
    tokio::task::yield_now().await;
    fixture.finish_run("first").await?;
    fixture
        .tx
        .send(Ok(b"execute\",\"params\":{\"prompt\":\"two\"}}\n".to_vec()))?;
    fixture.accepted("next").await?;
    fixture.finish_run("next").await?;
    fixture.close().await
}

#[tokio::test]
async fn queued_startup_injection_and_cancel_do_not_poison_the_next_run() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.persistent().await?;
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    fixture.send(
        "inject",
        "intervene/injectMessage",
        json!({"text":"change direction", "priority":"interrupt"}),
    )?;
    let token = CancellationToken::new();
    let descendant = token.child_token();
    let (messages, mut received) = mpsc::unbounded_channel();
    fixture.session.control.bind(Arc::new(Handler {
        cancel: token.clone(),
        messages,
    }))?;
    assert_eq!(fixture.frame().await?["result"]["status"], "injected");
    assert_eq!(
        received.recv().await,
        Some(("change direction".to_owned(), InjectPriority::Interrupt))
    );
    fixture.send(
        "cancel",
        "intervene/cancel",
        json!({"reason":"operator interrupted"}),
    )?;
    assert_eq!(
        fixture.frame().await?["result"]["status"],
        "cancel_requested"
    );
    assert!(descendant.is_cancelled());
    fixture.finish_run("first").await?;
    fixture.send("second", "run/execute", json!({"prompt":"two"}))?;
    fixture.accepted("second").await?;
    let fresh = CancellationToken::new();
    let (messages, next_inbox) = mpsc::unbounded_channel();
    fixture.session.control.bind(Arc::new(Handler {
        cancel: fresh.clone(),
        messages,
    }))?;
    fixture.finish_run("second").await?;
    assert!(descendant.is_cancelled());
    assert!(!fresh.is_cancelled());
    drop(next_inbox);
    fixture.close().await
}

#[tokio::test]
async fn stdin_failure_cancels_the_active_run_and_is_reported() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    let token = CancellationToken::new();
    let (messages, receiver) = mpsc::unbounded_channel();
    fixture.session.control.bind(Arc::new(Handler {
        cancel: token.clone(),
        messages,
    }))?;
    fixture
        .tx
        .send(Err(std::io::Error::other("broken input")))?;
    let result = tokio::time::timeout(Duration::from_secs(10), fixture.session.task).await??;
    assert!(result.is_err());
    assert!(token.is_cancelled());
    drop(receiver);
    Ok(())
}

#[tokio::test]
async fn eof_preserves_the_accepted_run_until_its_terminal_result() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    drop(fixture.tx);
    fixture
        .session
        .control
        .finish(JsonRpcResponse::ok(json!("first"), Value::Null))
        .await?;
    tokio::time::timeout(Duration::from_secs(10), fixture.session.task).await???;
    Ok(())
}

#[tokio::test]
async fn invalid_frames_and_intervention_failures_are_answered_without_ending_admission()
-> TestResult {
    let mut fixture = Fixture::new();
    fixture.tx.send(Ok(b"not json\n".to_vec()))?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32700);
    fixture.send("invalid", "run/execute", json!({"prompt": 9}))?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32600);
    fixture.send("first", "run/execute", json!({"input":"alias"}))?;
    fixture.accepted("first").await?;
    fixture.send("unknown", "intervene/pauseResume", Value::Null)?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32601);
    let (messages, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    fixture.session.control.bind(Arc::new(Handler {
        cancel: CancellationToken::new(),
        messages,
    }))?;
    fixture.send(
        "badpriority",
        "intervene/injectMessage",
        json!({"text":"hi","priority":"urgent"}),
    )?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32600);
    fixture.send("missingtext", "intervene/injectMessage", Value::Null)?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32600);
    fixture.send("closed", "intervene/injectMessage", json!({"text":"hi"}))?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32603);
    fixture.finish_run("first").await?;
    fixture.close().await
}

#[tokio::test]
async fn assembly_failure_answers_queued_controls_before_closing() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    fixture.send("inject", "intervene/injectMessage", json!({"text":"hi"}))?;
    fixture.send("init", "initialize", Value::Null)?;
    assert_eq!(fixture.frame().await?["id"], "init");
    fixture
        .session
        .control
        .finish_and_close(JsonRpcResponse::err(
            json!("first"),
            -32603,
            "assembly failed".to_owned(),
        ))
        .await?;
    let unavailable = fixture.frame().await?;
    assert_eq!(unavailable["id"], "inject");
    assert_eq!(unavailable["error"]["code"], -32603);
    assert_eq!(fixture.frame().await?["id"], "first");
    fixture.close().await
}

#[tokio::test]
async fn lifecycle_negotiation_is_explicit_and_fixed_after_the_first_run() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.send("invalid", "initialize", json!({"runLifecycle":"other"}))?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32600);
    fixture.persistent().await?;
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    fixture.send("change", "initialize", json!({"runLifecycle":"one_shot"}))?;
    assert_eq!(fixture.frame().await?["error"]["code"], -32000);
    fixture.send("same", "initialize", Value::Null)?;
    assert_eq!(
        fixture.frame().await?["result"]["capabilities"]["runLifecycle"],
        "persistent"
    );
    fixture.finish_run("first").await?;
    fixture.session.cancel.cancel();
    fixture.close().await
}

#[tokio::test]
async fn one_shot_is_the_default_and_closes_without_stdin_eof() -> TestResult {
    let mut fixture = Fixture::new();
    fixture.send("first", "run/execute", json!({"prompt":"one"}))?;
    fixture.accepted("first").await?;
    fixture.finish_run("first").await?;
    tokio::time::timeout(Duration::from_secs(10), fixture.session.task).await???;
    assert!(fixture.output.recv().await.is_none());
    Ok(())
}
