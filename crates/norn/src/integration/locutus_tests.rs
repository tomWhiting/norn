//! Native socket fixtures prove admission identity and stop ordering without audio hardware.

use super::*;
use locutus_contract::door::{Mode, Source};
use tokio::io::Lines;
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Reader = Lines<BufReader<OwnedReadHalf>>;

fn request(socket: PathBuf) -> ReadAloudRequest {
    ReadAloudRequest {
        id: Uuid::new_v4(),
        seat: "norn-test-seat".to_owned(),
        socket,
        text: "A completed answer.".to_owned(),
        voice: None,
    }
}

async fn emit(writer: &mut OwnedWriteHalf, event: DoorEvent) -> TestResult {
    writer
        .write_all(format!("{}\n", event.encode()).as_bytes())
        .await?;
    Ok(())
}

async fn receive(reader: &mut Reader) -> TestResult<DoorIntent> {
    let line = reader
        .next_line()
        .await?
        .ok_or_else(|| std::io::Error::other("client closed before its expected intent"))?;
    Ok(DoorIntent::decode(&line)?)
}

async fn accept(listener: &UnixListener) -> TestResult<(Reader, OwnedWriteHalf, String)> {
    let (socket, _) = listener.accept().await?;
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader).lines();
    emit(
        &mut writer,
        DoorEvent::State {
            mode: Mode::Off,
            source: Source::None,
            ears: false,
            hearing: false,
            held: false,
            speaking: false,
            pending: false,
            name: None,
            holder: None,
            sink: None,
            at: 0.0,
            session: "voice-session".to_owned(),
        },
    )
    .await?;
    assert!(
        matches!(receive(&mut reader).await?, DoorIntent::Register { seat, .. } if seat == "norn-test-seat")
    );
    let DoorIntent::Say {
        request: Some(tag),
        text,
        id: None,
        more: false,
        ..
    } = receive(&mut reader).await?
    else {
        return Err(std::io::Error::other("expected one tagged native say").into());
    };
    assert_eq!(text, "A completed answer.");
    Ok((reader, writer, tag))
}

fn say(tag: &str, id: u64) -> DoorEvent {
    DoorEvent::Say {
        id,
        text: "A completed answer.".to_owned(),
        voice: "bm_fable".to_owned(),
        request: Some(tag.to_owned()),
        at: 1.0,
        session: "voice-session".to_owned(),
    }
}

fn spoken(tag: &str, id: u64) -> DoorEvent {
    DoorEvent::Spoken {
        id,
        ms: 2500,
        request: Some(tag.to_owned()),
        at: 4.0,
        session: "voice-session".to_owned(),
    }
}

fn untagged_error() -> DoorEvent {
    DoorEvent::Error {
        message: "untagged service error".to_owned(),
        request: None,
        at: 2.0,
        session: "voice-session".to_owned(),
    }
}

#[tokio::test]
async fn untagged_error_is_terminal_only_before_admission() -> TestResult {
    for admitted in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("door");
        let listener = UnixListener::bind(&path)?;
        let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
        let server = async {
            let (reader, mut writer, tag) = accept(&listener).await?;
            if admitted {
                emit(&mut writer, say(&tag, 41)).await?;
            }
            emit(&mut writer, untagged_error()).await?;
            if admitted {
                emit(&mut writer, spoken(&tag, 41)).await?;
            }
            drop(reader);
            TestResult::Ok(())
        };
        let (result, fixture_outcome) = tokio::join!(
            read_aloud(request(path), CancellationToken::new(), progress),
            server
        );
        fixture_outcome?;
        if admitted {
            assert!(matches!(
                result?,
                ReadAloudOutcome::Completed { id: 41, .. }
            ));
        } else {
            assert!(
                matches!(result, Err(ReadAloudError::Protocol { reason, .. }) if reason == "untagged service error")
            );
        }
        drop(receiver);
    }
    Ok(())
}

#[tokio::test]
async fn completed_receipt_retains_the_stop_request() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let cancel = CancellationToken::new();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (mut reader, mut writer, tag) = accept(&listener).await?;
        emit(&mut writer, say(&tag, 41)).await?;
        cancel.cancel();
        assert!(
            matches!(receive(&mut reader).await?, DoorIntent::Hush { request: Some(echoed), .. } if echoed == tag)
        );
        emit(&mut writer, spoken(&tag, 41)).await?;
        TestResult::Ok(())
    };
    let (result, fixture_outcome) =
        tokio::join!(read_aloud(request(path), cancel.clone(), progress), server);
    fixture_outcome?;
    assert!(matches!(
        result?,
        ReadAloudOutcome::Completed {
            stop_requested: true,
            ..
        }
    ));
    drop(receiver);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn unanswered_hush_releases_the_socket_with_an_unknown_outcome() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let cancel = CancellationToken::new();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (mut reader, writer, tag) = accept(&listener).await?;
        cancel.cancel();
        assert!(
            matches!(receive(&mut reader).await?, DoorIntent::Hush { request: Some(echoed), .. } if echoed == tag)
        );
        tokio::time::advance(STOP_RECEIPT_DEADLINE).await;
        assert!(reader.next_line().await?.is_none());
        drop(writer);
        TestResult::Ok(())
    };
    let (result, fixture_outcome) =
        tokio::join!(read_aloud(request(path), cancel.clone(), progress), server);
    fixture_outcome?;
    assert!(
        matches!(result?, ReadAloudOutcome::StopUnconfirmed { stop_sent: true, waited } if waited >= STOP_RECEIPT_DEADLINE)
    );
    assert_eq!(*receiver.borrow(), ReadAloudProgress::Stopping);
    Ok(())
}

#[tokio::test]
async fn unrelated_broadcasts_cannot_admit_or_complete_our_speech() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (reader, mut writer, tag) = accept(&listener).await?;
        emit(&mut writer, say("someone-else", 99)).await?;
        emit(&mut writer, spoken("someone-else", 99)).await?;
        emit(&mut writer, say(&tag, 41)).await?;
        emit(
            &mut writer,
            DoorEvent::Playing {
                id: 41,
                at: 1.2,
                latency_ms: 12,
                session: "voice-session".to_owned(),
            },
        )
        .await?;
        emit(&mut writer, spoken(&tag, 41)).await?;
        drop(reader);
        TestResult::Ok(())
    };
    let (result, fixture_outcome) = tokio::join!(
        read_aloud(request(path), CancellationToken::new(), progress),
        server
    );
    fixture_outcome?;
    assert_eq!(
        result?,
        ReadAloudOutcome::Completed {
            session: "voice-session".to_owned(),
            id: 41,
            duration_ms: 2500,
            stop_requested: false
        }
    );
    assert_eq!(*receiver.borrow(), ReadAloudProgress::Playing { id: 41 });
    Ok(())
}

#[tokio::test]
async fn stop_before_admission_uses_the_client_tag_not_a_guessed_chain() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let cancel = CancellationToken::new();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (mut reader, mut writer, tag) = accept(&listener).await?;
        cancel.cancel();
        assert_eq!(
            receive(&mut reader).await?,
            DoorIntent::Hush {
                id: None,
                request: Some(tag.clone())
            }
        );
        emit(
            &mut writer,
            DoorEvent::Hushed {
                id: 41,
                request: Some(tag),
                at_ms: 0,
                latency_ms: 0,
                at: 1.0,
                session: "voice-session".to_owned(),
            },
        )
        .await?;
        TestResult::Ok(())
    };
    let (result, fixture_outcome) =
        tokio::join!(read_aloud(request(path), cancel.clone(), progress), server);
    fixture_outcome?;
    assert_eq!(
        result?,
        ReadAloudOutcome::Stopped {
            session: "voice-session".to_owned(),
            id: 41,
            at_ms: 0,
            latency_ms: 0
        }
    );
    assert_eq!(*receiver.borrow(), ReadAloudProgress::Stopping);
    Ok(())
}

#[tokio::test]
async fn stop_receipt_retains_the_measured_position_and_device_uncertainty() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let cancel = CancellationToken::new();
    let (progress, mut receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (mut reader, mut writer, tag) = accept(&listener).await?;
        emit(&mut writer, say(&tag, 41)).await?;
        emit(
            &mut writer,
            DoorEvent::Playing {
                id: 41,
                at: 1.2,
                latency_ms: 12,
                session: "voice-session".to_owned(),
            },
        )
        .await?;
        receiver
            .wait_for(|state| matches!(state, ReadAloudProgress::Playing { .. }))
            .await?;
        cancel.cancel();
        assert_eq!(
            receive(&mut reader).await?,
            DoorIntent::Hush {
                id: None,
                request: Some(tag.clone())
            }
        );
        emit(
            &mut writer,
            DoorEvent::Hushed {
                id: 41,
                request: Some(tag),
                at_ms: 1200,
                latency_ms: 12,
                at: 2.4,
                session: "voice-session".to_owned(),
            },
        )
        .await?;
        TestResult::Ok(())
    };
    let (result, fixture_outcome) =
        tokio::join!(read_aloud(request(path), cancel.clone(), progress), server);
    fixture_outcome?;
    assert_eq!(
        result?,
        ReadAloudOutcome::Stopped {
            session: "voice-session".to_owned(),
            id: 41,
            at_ms: 1200,
            latency_ms: 12
        }
    );
    Ok(())
}

#[tokio::test]
async fn disconnect_after_submission_is_an_unknown_outcome() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let connection = accept(&listener).await?;
        drop(connection);
        TestResult::Ok(())
    };
    let (result, fixture_outcome) = tokio::join!(
        read_aloud(request(path), CancellationToken::new(), progress),
        server
    );
    fixture_outcome?;
    assert!(
        matches!(result, Err(ReadAloudError::Protocol { reason, .. }) if reason.contains("outcome unknown"))
    );
    drop(receiver);
    Ok(())
}

#[tokio::test]
async fn cancelling_before_connect_opens_no_socket() -> TestResult {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    assert_eq!(
        read_aloud(
            request(PathBuf::from("/not-a-voice-socket")),
            cancel,
            progress
        )
        .await?,
        ReadAloudOutcome::NotSubmitted
    );
    assert_eq!(*receiver.borrow(), ReadAloudProgress::Connecting);
    Ok(())
}

#[tokio::test]
async fn mismatched_terminal_identity_is_refused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("door");
    let listener = UnixListener::bind(&path)?;
    let (progress, receiver) = watch::channel(ReadAloudProgress::Connecting);
    let server = async {
        let (reader, mut writer, tag) = accept(&listener).await?;
        emit(&mut writer, say(&tag, 41)).await?;
        emit(&mut writer, spoken(&tag, 42)).await?;
        drop(reader);
        TestResult::Ok(())
    };
    let (result, fixture_outcome) = tokio::join!(
        read_aloud(request(path), CancellationToken::new(), progress),
        server
    );
    fixture_outcome?;
    assert!(
        matches!(result, Err(ReadAloudError::Protocol { reason, .. }) if reason.contains("different speech identity"))
    );
    drop(receiver);
    Ok(())
}
