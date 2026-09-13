//! Finite receiver batches for child results, inbound messages and active input.

/// Consume only the captured queue frontier, leaving later arrivals to the event
/// loop so a producer cannot extend this synchronous batch past other ready work.
pub fn ready_frontier<T>(
    receiver: &mut tokio::sync::mpsc::Receiver<T>,
) -> impl Iterator<Item = T> + '_ {
    let remaining = receiver.len();
    // Empty and disconnected both end this batch; buffered results remain readable
    // after sender disconnection, and the normal receiver owns future arrivals.
    std::iter::from_fn(move || receiver.try_recv().ok())
        .take(remaining)
        .fuse()
}

/// Capture an unbounded receiver's currently queued inputs without following new arrivals.
pub fn ready_unbounded_frontier<T>(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
) -> impl Iterator<Item = T> + '_ {
    let remaining = receiver.len();
    std::iter::from_fn(move || receiver.try_recv().ok())
        .take(remaining)
        .fuse()
}

#[cfg(test)]
#[path = "result_batch_tests.rs"]
mod tests;
