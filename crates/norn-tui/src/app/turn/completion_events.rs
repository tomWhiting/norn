//! Drain only the live-event frontier present when execution finishes; retain later events.

use tokio::sync::broadcast::{Receiver, error::TryRecvError};

/// Later child publications remain in this receiver for its ordinary event owner.
/// Lag advances the receiver by the reported count, including past the frontier.
pub(super) fn drain_frontier<T: Clone, E>(
    receiver: &mut Receiver<T>,
    mut consume: impl FnMut(Result<T, TryRecvError>) -> Result<(), E>,
) -> Result<(), E> {
    let mut remaining = receiver.len();
    while remaining > 0 {
        let event = receiver.try_recv();
        remaining = match &event {
            Ok(_) => remaining - 1,
            Err(TryRecvError::Lagged(missed)) => match usize::try_from(*missed) {
                Ok(missed) => remaining.saturating_sub(missed),
                // A skip larger than usize necessarily crosses this usize frontier.
                Err(_) => 0,
            },
            Err(TryRecvError::Empty | TryRecvError::Closed) => 0,
        };
        consume(event)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "completion_events_tests.rs"]
mod tests;
