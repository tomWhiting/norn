//! Completion frontier proof with concurrent publications, retained order, lag and errors.

use tokio::sync::broadcast::{self, error::TryRecvError};

use super::drain_frontier;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn publications_during_consumption_remain_for_the_normal_owner() -> TestResult {
    let (sender, mut receiver) = broadcast::channel(16);
    for value in 0..3 {
        sender.send(value)?;
    }
    let mut seen = Vec::new();
    drain_frontier(&mut receiver, |event| -> TestResult {
        let value = event?;
        seen.push(value);
        if value < 3 {
            sender.send(value + 3)?;
        }
        Ok(())
    })?;
    assert_eq!(seen, [0, 1, 2]);
    assert_eq!(receiver.len(), 3);
    for expected in 3..6 {
        assert_eq!(receiver.try_recv()?, expected);
    }
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    Ok(())
}

#[test]
fn initial_lag_counts_skipped_events_without_consuming_later_publications() -> TestResult {
    let (sender, mut receiver) = broadcast::channel(2);
    for value in 0..4 {
        sender.send(value)?;
    }
    let mut seen = Vec::new();
    drain_frontier(&mut receiver, |event| -> TestResult {
        if event == Ok(3) {
            sender.send(4)?;
        }
        seen.push(event);
        Ok(())
    })?;
    assert_eq!(seen, [Err(TryRecvError::Lagged(2)), Ok(2), Ok(3)]);
    assert_eq!(receiver.try_recv()?, 4);
    Ok(())
}

#[test]
fn lag_crossing_the_frontier_does_not_consume_newer_retained_events() -> TestResult {
    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(0)?;
    sender.send(1)?;
    let mut seen = Vec::new();
    drain_frontier(&mut receiver, |event| -> TestResult {
        if event == Ok(0) {
            for value in 2..12 {
                sender.send(value)?;
            }
        }
        seen.push(event);
        Ok(())
    })?;
    assert_eq!(seen, [Ok(0), Err(TryRecvError::Lagged(9))]);
    assert_eq!(receiver.try_recv()?, 10);
    assert_eq!(receiver.try_recv()?, 11);
    Ok(())
}

#[test]
fn consumer_failure_leaves_subsequent_events_unconsumed() -> TestResult {
    let (sender, mut receiver) = broadcast::channel(2);
    sender.send(0)?;
    sender.send(1)?;
    let result = drain_frontier(&mut receiver, |event| {
        assert_eq!(event, Ok(0));
        Err("frontend reduction failed")
    });
    assert_eq!(result, Err("frontend reduction failed"));
    assert_eq!(receiver.try_recv()?, 1);
    Ok(())
}

#[test]
fn empty_and_closed_receivers_remain_owned_by_the_normal_event_loop() -> TestResult {
    let (sender, mut receiver) = broadcast::channel::<u8>(2);
    drain_frontier(&mut receiver, |event| -> TestResult {
        Err(format!("empty frontier unexpectedly consumed {event:?}").into())
    })?;
    sender.send(7)?;
    drop(sender);
    let mut seen = Vec::new();
    drain_frontier(&mut receiver, |event| -> TestResult {
        seen.push(event?);
        Ok(())
    })?;
    assert_eq!(seen, [7]);
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Closed));
    drain_frontier(&mut receiver, |event| -> TestResult {
        Err(format!("empty closed frontier unexpectedly consumed {event:?}").into())
    })?;
    Ok(())
}
