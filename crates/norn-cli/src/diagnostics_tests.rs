//! Verify the production writer, bounded loss reporting and terminal capture lifetime.

use super::*;
use tracing_subscriber::fmt::MakeWriter;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn actual_formatter_routes_complete_events_from_another_thread() -> TestResult {
    let router = Router::default();
    let (mut guard, mut receiver) = router.begin(NonZeroUsize::MIN)?;
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(router)
        .finish();
    let emitter = std::thread::spawn(move || {
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "diagnostic-test", attempt = 7, "provider retry");
        });
    });
    if let Err(payload) = emitter.join() {
        std::panic::resume_unwind(payload);
    }
    let event = receiver.recv().await?;
    assert_eq!(event.sequence, 1);
    assert_eq!(event.level, tracing::Level::WARN);
    assert_eq!(event.target, "diagnostic-test");
    assert!(event.text.contains("provider retry"));
    assert!(event.text.contains("attempt=7"));
    assert!(event.text.ends_with('\n'));
    assert!(!event.text.contains('\x1b'));
    receiver.acknowledge(event.sequence);
    let mut restored = Vec::new();
    guard.drain(&mut restored)?;
    assert!(
        restored.is_empty(),
        "retained diagnostics must not be printed twice"
    );
    Ok(())
}

#[tokio::test]
async fn overflow_is_explicit_and_exit_preserves_unseen_tail() -> TestResult {
    let router = Router::default();
    let (mut guard, mut receiver) = router.begin(NonZeroUsize::MIN)?;
    let mut stderr = Vec::new();
    router.write(tracing::Level::WARN, "test", b"first\n", &mut stderr)?;
    router.write(tracing::Level::ERROR, "test", b"second\n", &mut stderr)?;
    assert!(stderr.is_empty());
    assert!(matches!(
        receiver.recv().await,
        Err(broadcast::error::RecvError::Lagged(1))
    ));
    guard.drain(&mut stderr)?;
    let text = String::from_utf8(stderr)?;
    assert!(text.contains("1 pending events overwritten before display"));
    assert!(text.ends_with("second\n"));
    Ok(())
}

#[tokio::test]
async fn exit_skips_acknowledged_records_but_keeps_failed_retention() -> TestResult {
    let router = Router::default();
    let capacity = NonZeroUsize::new(2).ok_or("test capacity")?;
    let (mut guard, mut receiver) = router.begin(capacity)?;
    let mut stderr = Vec::new();
    router.write(tracing::Level::WARN, "test", b"retained\n", &mut stderr)?;
    let first = receiver.recv().await?;
    receiver.acknowledge(first.sequence);
    router.write(
        tracing::Level::WARN,
        "test",
        b"retention failed\n",
        &mut stderr,
    )?;
    let second = receiver.recv().await?;
    assert_eq!(second.sequence, 2);
    // Receiving is not acknowledgement: UI persistence can fail afterwards.
    guard.drain(&mut stderr)?;
    assert_eq!(stderr, b"retention failed\n");
    Ok(())
}

#[test]
fn stderr_before_after_capture_and_exclusive_ownership() -> TestResult {
    let router = Router::default();
    let mut stderr = Vec::new();
    router.write(tracing::Level::WARN, "test", b"before\n", &mut stderr)?;
    let (mut guard, receiver) = router.begin(NonZeroUsize::MIN)?;
    assert!(router.begin(NonZeroUsize::MIN).is_err());
    drop(receiver);
    router.write(tracing::Level::WARN, "test", b"during\n", &mut stderr)?;
    assert_eq!(stderr, b"before\n");
    guard.drain(&mut stderr)?;
    router.write(tracing::Level::WARN, "test", b"after\n", &mut stderr)?;
    assert_eq!(stderr, b"before\nduring\nafter\n");
    Ok(())
}

#[test]
fn writer_created_before_capture_routes_at_write_time() -> TestResult {
    let router = Router::default();
    let mut writer = router.make_writer();
    let (mut guard, receiver) = router.begin(NonZeroUsize::MIN)?;
    writer.write_all(b"delayed writer\n")?;
    drop(receiver);
    let mut restored = Vec::new();
    guard.drain(&mut restored)?;
    assert_eq!(restored, b"delayed writer\n");
    Ok(())
}

#[test]
fn drain_failure_restores_route_without_swallowing_the_io_error() -> TestResult {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("{} bytes refused", bytes.len()),
            ))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let router = Router::default();
    let (mut guard, receiver) = router.begin(NonZeroUsize::MIN)?;
    drop(receiver);
    router.write(tracing::Level::WARN, "test", b"pending\n", &mut Vec::new())?;
    assert_eq!(
        guard
            .drain(&mut Broken)
            .err()
            .ok_or("expected drain error")?
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    let mut restored = Vec::new();
    router.write(tracing::Level::WARN, "test", b"restored\n", &mut restored)?;
    assert_eq!(restored, b"restored\n");
    Ok(())
}

#[test]
fn unrepresentable_capacity_is_an_error_before_tokio_can_panic() -> TestResult {
    let capacity = NonZeroUsize::new(usize::MAX).ok_or("nonzero maximum")?;
    assert!(Router::default().begin(capacity).is_err());
    Ok(())
}

#[test]
fn finished_guard_cannot_drain_a_subsequent_capture() -> TestResult {
    let router = Router::default();
    let (mut first, first_receiver) = router.begin(NonZeroUsize::MIN)?;
    first.drain(&mut Vec::new())?;
    drop(first_receiver);
    let (mut second, second_receiver) = router.begin(NonZeroUsize::MIN)?;
    drop(first);
    let mut stderr = Vec::new();
    router.write(
        tracing::Level::WARN,
        "test",
        b"second capture\n",
        &mut stderr,
    )?;
    assert!(
        stderr.is_empty(),
        "an old guard must not take the new capture"
    );
    drop(second_receiver);
    second.drain(&mut stderr)?;
    assert_eq!(stderr, b"second capture\n");
    Ok(())
}
