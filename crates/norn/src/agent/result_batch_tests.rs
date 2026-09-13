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

#[test]
fn unbounded_refill_stays_after_original_frontier() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(1)?;
    sender.send(2)?;
    {
        let mut batch = super::ready_unbounded_frontier(&mut receiver);
        assert_eq!(batch.next(), Some(1));
        sender.send(3)?;
        assert_eq!(batch.next(), Some(2));
        sender.send(4)?;
        assert_eq!(batch.next(), None);
    }
    drop(sender);
    assert_eq!(
        super::ready_unbounded_frontier(&mut receiver).collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(
        super::ready_unbounded_frontier(&mut receiver)
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn unbounded_empty_frontier_keeps_later_input() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut batch = super::ready_unbounded_frontier(&mut receiver);
        sender.send(1)?;
        assert_eq!(batch.next(), None);
    }
    assert_eq!(receiver.try_recv()?, 1);
    Ok(())
}
