//! Deterministic queue-refill and disconnect tests for finite result batches.

use super::ready_frontier;

#[test]
fn refilling_producer_cannot_extend_the_captured_batch() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    sender.try_send(1)?;
    sender.try_send(2)?;
    {
        let mut batch = ready_frontier(&mut receiver);
        assert_eq!(batch.next(), Some(1));
        sender.try_send(3)?;
        assert_eq!(batch.next(), Some(2));
        sender.try_send(4)?;
        assert_eq!(batch.next(), None);
    }
    assert_eq!(receiver.try_recv()?, 3);
    assert_eq!(receiver.try_recv()?, 4);
    Ok(())
}

#[test]
fn empty_frontier_does_not_consume_later_arrival() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    {
        let mut batch = ready_frontier(&mut receiver);
        sender.try_send(1)?;
        assert_eq!(batch.next(), None);
    }
    assert_eq!(receiver.try_recv()?, 1);
    Ok(())
}

#[test]
fn disconnected_frontier_retains_every_buffered_result() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    sender.try_send(1)?;
    sender.try_send(2)?;
    drop(sender);
    assert_eq!(ready_frontier(&mut receiver).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(
        receiver.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
    );
    Ok(())
}
