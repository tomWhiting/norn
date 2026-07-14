//! Cancellation guard for one in-flight MCP stdio request.

use std::sync::Arc;

use super::StdioShared;

pub(super) struct RequestGuard {
    shared: Arc<StdioShared>,
    request_id: u64,
    complete: bool,
}

impl RequestGuard {
    pub(super) fn new(shared: Arc<StdioShared>, request_id: u64) -> Self {
        Self {
            shared,
            request_id,
            complete: false,
        }
    }

    pub(super) fn finish(&mut self) {
        self.complete = true;
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        if let Ok(mut pending) = self.shared.pending() {
            pending.remove(&self.request_id);
        }
        self.shared
            .invalidate("MCP stdio request was cancelled before its response");
    }
}
