use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use super::{JsonRpcResponse, McpClient};
use crate::error::IntegrationError;

#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn request(
        &self,
        payload: String,
        request_id: u64,
    ) -> Result<JsonRpcResponse, IntegrationError>;
    async fn notify(&self, payload: String) -> Result<(), IntegrationError>;
    fn supports_protocol_version(&self, version: &str) -> bool;
    async fn set_protocol_version(&self, _version: &str) {}
    async fn invalidate(&self) {}

    /// Whether this transport can still carry another request.
    fn is_live(&self) -> bool {
        true
    }
}

pub(super) struct ClientRequestGuard<'live> {
    live: &'live AtomicBool,
    finished: bool,
}

impl<'live> ClientRequestGuard<'live> {
    pub(super) const fn new(live: &'live AtomicBool) -> Self {
        Self {
            live,
            finished: false,
        }
    }

    pub(super) const fn finish_if(&mut self, finished: bool) {
        self.finished = finished;
    }
}

impl Drop for ClientRequestGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.live.store(false, Ordering::Release);
        }
    }
}

pub(super) fn unusable_client_error() -> IntegrationError {
    IntegrationError::McpError {
        reason: "MCP client connection is no longer usable".to_owned(),
    }
}

impl McpClient {
    pub(crate) fn instance_id(&self) -> u64 {
        self.inner.instance_id
    }

    pub(crate) fn is_live(&self) -> bool {
        self.inner.is_live()
    }
}
