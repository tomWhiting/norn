//! Concurrent persisted message traffic, with real PTY typing and exit observations.

use super::*;
use norn::agent_loop::inbound::{ChannelMessage, MessageKind, frame_message};
use norn::provider::agent_event::AgentMessageLifecycle;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct Traffic {
    stop: mpsc::Sender<()>,
    worker: JoinHandle<io::Result<()>>,
    count: Arc<AtomicU64>,
}

impl Traffic {
    pub(super) fn start(store: Arc<EventStore>, events: AgentEventSender) -> io::Result<Self> {
        // Deliberately exceed the fixture broadcast capacity before typing so
        // history catch-up and live receiver lag are exercised together.
        let backlog = 256;
        for seq in 1..=backlog {
            emit(&store, &events, seq)?;
        }
        let count = Arc::new(AtomicU64::new(backlog));
        let observed = Arc::clone(&count);
        let (stop, stopped) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            // Fixture pacing only: one delivered message per millisecond, each
            // with a real saved input and audit. Stop is pushed through the channel.
            loop {
                match stopped.recv_timeout(Duration::from_millis(1)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let next = observed.load(Ordering::SeqCst) + 1;
                        emit(&store, &events, next)?;
                        observed.store(next, Ordering::SeqCst);
                    }
                }
            }
        });
        Ok(Self {
            stop,
            worker,
            count,
        })
    }

    pub(super) fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    pub(super) fn finish(self) -> io::Result<()> {
        // A producer that failed may already have dropped its receiver; its
        // joined result below is authoritative and must still be surfaced.
        let stop = self.stop.send(());
        join(self.worker, "message flood producer")?;
        stop.map_err(|error| io::Error::other(format!("message flood stop: {error}")))
    }
}

fn emit(store: &EventStore, events: &AgentEventSender, seq: u64) -> io::Result<()> {
    let input = ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::nil(),
        from: "flood worker".to_owned(),
        role: None,
        to_id: events.agent_id(),
        content: format!("flood payload {seq}"),
        kind: MessageKind::Update,
        seq: Some(seq),
        timestamp: chrono::Utc::now(),
    };
    let event = SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: frame_message(&input),
    };
    let event_id = store.append(event).map_err(io::Error::other)?;
    let delivered = AgentMessageLifecycle::Delivered {
        message_id: input.id,
        from_id: input.sender_id,
        from: input.from,
        to_id: input.to_id,
        seq: input.seq,
        delivered_at: chrono::Utc::now(),
    };
    let mut data = serde_json::to_value(&delivered)?;
    data["user_event_id"] = serde_json::to_value(&event_id)?;
    store
        .append(SessionEvent::Custom {
            base: EventBase::new(Some(event_id)),
            event_type: delivered.session_event_type().to_owned(),
            data,
        })
        .map_err(io::Error::other)?;
    events.send_message(delivered);
    Ok(())
}

/// The producer is stopped only after the terminal has restored its modes.
pub fn verify() -> TestResult {
    let mut app = Workspace::start(Some("enter"), false, false, false, None)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exercise(&mut app)))
        .map_err(|payload| panic_error(payload.as_ref(), "message flood assertions"))
        .and_then(|result| result);
    let cleanup = app.finish(result.is_err());
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (result, cleanup) => Err(io::Error::other(format!(
            "message flood: {result:?}; cleanup: {cleanup:?}; terminal:\n{}",
            String::from_utf8_lossy(&app.output.bytes()?)
        ))
        .into()),
    }
}

fn exercise(app: &mut Workspace) -> io::Result<()> {
    app.input(b"flood fixture\r", |screen| screen.contains(INITIAL))?;
    let started = app.control("start_flood")?;
    let before = started["count"]
        .as_u64()
        .ok_or_else(|| io::Error::other("flood count missing"))?;
    let mut expected = String::new();
    let mut timings = Vec::new();
    for byte in b"draft survives during continuous agent messages" {
        expected.push(char::from(*byte));
        let at = Instant::now();
        let screen = app.input(&[*byte], |screen| {
            screen.cursor.0 == expected.len()
                && screen
                    .composer_rows()
                    .iter()
                    .any(|row| screen.lines()[*row] == expected.trim_end())
        })?;
        timings.push(at.elapsed().as_micros());
        if screen.contains("agent_message.") || screen.contains("Agent message") {
            return Err(io::Error::other(format!(
                "audit clutter in ordinary conversation:\n{}",
                screen.debug_text()
            )));
        }
    }
    let count = app.control("flood_count")?["count"]
        .as_u64()
        .ok_or_else(|| io::Error::other("flood count missing"))?;
    if count <= before {
        return Err(io::Error::other("producer did not continue during typing"));
    }
    let screen = app.input(b"\x03", |screen| {
        screen.contains("Press Ctrl+C again within 3s to exit")
    })?;
    if !screen
        .composer_rows()
        .iter()
        .any(|row| screen.lines()[*row] == expected)
    {
        return Err(io::Error::other("flood cancellation discarded draft"));
    }
    let at = Instant::now();
    app.send(b"\x03")?;
    app.output
        .wait("terminal restored under message flood", |bytes| {
            Ok(Lifecycle::from_output(bytes, 24, 100)
                .assert_restored()
                .is_ok()
                .then_some(()))
        })?;
    let exit_us = at.elapsed().as_micros();
    let last = app.control("flood_count")?;
    app.control("close")?;
    app.finish(false)?;
    let report: Value = serde_json::from_slice(&std::fs::read(&app.final_report)?)?;
    let inputs = report["user_events"]
        .as_array()
        .ok_or_else(|| io::Error::other("final user events missing"))?;
    if inputs.first() != Some(&json!("flood fixture"))
        || inputs.iter().skip(1).any(|input| {
            !input
                .as_str()
                .is_some_and(|text| text.starts_with("<agent_message from=\"flood worker\""))
        })
    {
        return Err(io::Error::other(format!(
            "unexpected message admission: {report}"
        )));
    }
    if report["provider_calls"] != 1 {
        return Err(io::Error::other(format!(
            "unexpected provider admission: {report}"
        )));
    }
    eprintln!(
        "message flood observations: {}",
        json!({"messages_before_typing":before,"messages_after_typing":count,"messages_at_exit":last["count"],"input_to_frame_us":timings,"second_ctrl_c_to_restored_us":exit_us})
    );
    Ok(())
}
