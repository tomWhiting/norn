//! Domain-agnostic post-tool-batch notification hook.

use std::sync::Arc;

use async_trait::async_trait;

/// Host-supplied hook invoked after a tool batch completes.
///
/// The Norn runtime owns the batch boundary, while the embedding service owns
/// the notification source, inbound sender, and delivery bookkeeping.
#[async_trait]
pub trait ToolBatchNotificationInjector: Send + Sync {
    /// Check for pending notifications and inject them through the host-owned
    /// inbound sender.
    async fn inject_after_tool_batch(&self);
}

/// Typed [`ToolContext`](crate::tool::context::ToolContext) extension carrying
/// the domain-specific notification injector.
#[derive(Clone)]
pub struct ToolBatchNotificationHook {
    injector: Arc<dyn ToolBatchNotificationInjector>,
}

impl ToolBatchNotificationHook {
    /// Wrap a domain-specific injector for publication as a typed extension.
    #[must_use]
    pub fn new(injector: Arc<dyn ToolBatchNotificationInjector>) -> Self {
        Self { injector }
    }

    /// Run the wrapped injector.
    pub async fn inject_after_tool_batch(&self) {
        self.injector.inject_after_tool_batch().await;
    }
}
