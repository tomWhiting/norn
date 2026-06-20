//! Active-turn human input.
//!
//! This channel is distinct from [`super::inbound`]: inbound messages are
//! harness-framed inter-agent traffic, while active input is trusted operator
//! text from the current surface. The runner drains active input only at safe
//! provider boundaries and persists it as ordinary `UserMessage` events before
//! acknowledging delivery to the surface.

use std::error::Error;
use std::fmt;

use tokio::sync::mpsc;
use uuid::Uuid;

/// Operator input accepted for the currently running logical turn.
#[derive(Debug)]
pub struct ActiveInput {
    id: Uuid,
    content: String,
    delivery_tx: mpsc::UnboundedSender<ActiveInputDelivery>,
}

impl ActiveInput {
    /// Unique id for this pending input.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// User-authored content to inject as a model-visible user message.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Notify the originating surface that the input was durably persisted.
    pub fn mark_delivered(&self) {
        if self
            .delivery_tx
            .send(ActiveInputDelivery {
                id: self.id,
                content: self.content.clone(),
            })
            .is_err()
        {
            tracing::debug!(
                active_input_id = %self.id,
                "active input delivery receiver was dropped before acknowledgement",
            );
        }
    }
}

/// Delivery acknowledgement for active-turn input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveInputDelivery {
    /// Unique id of the delivered input.
    pub id: Uuid,
    /// The exact content persisted as the model-visible user message.
    pub content: String,
}

/// Error returned when active input cannot be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveInputError {
    /// Empty or whitespace-only input is not accepted.
    Empty,
    /// The active turn has already ended and no receiver remains.
    Closed,
}

impl fmt::Display for ActiveInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("active input must not be empty"),
            Self::Closed => f.write_str("active turn input channel is closed"),
        }
    }
}

impl Error for ActiveInputError {}

/// Sender cloned by the TUI or embedder while a turn is running.
#[derive(Clone, Debug)]
pub struct ActiveInputSender {
    tx: mpsc::UnboundedSender<ActiveInput>,
    delivery_tx: mpsc::UnboundedSender<ActiveInputDelivery>,
}

impl ActiveInputSender {
    /// Accept a human steer for the active turn.
    ///
    /// The returned id remains pending until an [`ActiveInputDelivery`] with the
    /// same id is received. A successful send means the runner has accepted the
    /// input into its in-memory active-turn queue, not that the model has seen
    /// it yet.
    ///
    /// # Errors
    ///
    /// Returns [`ActiveInputError::Empty`] for whitespace-only text and
    /// [`ActiveInputError::Closed`] when the active turn has already ended.
    pub fn send_steer(&self, content: impl Into<String>) -> Result<Uuid, ActiveInputError> {
        let content = content.into();
        if content.trim().is_empty() {
            return Err(ActiveInputError::Empty);
        }
        let id = Uuid::new_v4();
        let input = ActiveInput {
            id,
            content,
            delivery_tx: self.delivery_tx.clone(),
        };
        if let Err(err) = self.tx.send(input) {
            let input = err.0;
            tracing::debug!(
                active_input_id = %input.id,
                "active input receiver was closed before accepting input",
            );
            return Err(ActiveInputError::Closed);
        }
        Ok(id)
    }
}

/// Receiver owned by the active agent loop.
#[derive(Debug)]
pub struct ActiveInputReceiver {
    rx: mpsc::UnboundedReceiver<ActiveInput>,
}

impl ActiveInputReceiver {
    /// Drain every currently buffered active input without awaiting.
    #[must_use]
    pub fn drain(&mut self) -> Vec<ActiveInput> {
        let mut drained = Vec::new();
        while let Ok(input) = self.rx.try_recv() {
            drained.push(input);
        }
        drained
    }
}

/// Receiver for delivery acknowledgements emitted after persistence.
#[derive(Debug)]
pub struct ActiveInputDeliveryReceiver {
    rx: mpsc::UnboundedReceiver<ActiveInputDelivery>,
}

impl ActiveInputDeliveryReceiver {
    /// Receive the next delivery acknowledgement.
    pub async fn recv(&mut self) -> Option<ActiveInputDelivery> {
        self.rx.recv().await
    }

    /// Attempt to receive a delivery acknowledgement without waiting.
    #[must_use]
    pub fn try_recv(&mut self) -> Option<ActiveInputDelivery> {
        self.rx.try_recv().ok()
    }
}

/// Create a fresh active-input channel for one running turn.
#[must_use]
pub fn active_input_channel() -> (
    ActiveInputSender,
    ActiveInputReceiver,
    ActiveInputDeliveryReceiver,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (delivery_tx, delivery_rx) = mpsc::unbounded_channel();
    (
        ActiveInputSender { tx, delivery_tx },
        ActiveInputReceiver { rx },
        ActiveInputDeliveryReceiver { rx: delivery_rx },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_steer_rejects_empty_input() {
        let (tx, _rx, _delivery_rx) = active_input_channel();

        assert_eq!(tx.send_steer("  \n\t"), Err(ActiveInputError::Empty));
    }

    #[tokio::test]
    async fn delivery_ack_round_trips_content() -> Result<(), Box<dyn Error>> {
        let (tx, mut rx, mut delivery_rx) = active_input_channel();
        let id = tx.send_steer("please adjust")?;
        let drained = rx.drain();

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].id(), id);
        assert_eq!(drained[0].content(), "please adjust");

        drained[0].mark_delivered();
        assert_eq!(
            delivery_rx.recv().await,
            Some(ActiveInputDelivery {
                id,
                content: "please adjust".to_string(),
            }),
        );
        Ok(())
    }
}
