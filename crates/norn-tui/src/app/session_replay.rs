//! Explicit initial history demand into the retained semantic view, outside terminal painting.

use std::sync::Arc;

use norn::session::store::EventStore;

use crate::TuiError;

use super::state::AppState;
use super::transcript::read_history;

/// Load the declared initial tail without cloning full raw session history.
/// This is startup work, never a render/resize callback.
pub(super) async fn replay_visible_session_history(
    state: &mut AppState,
    store: &Arc<EventStore>,
) -> Result<(), TuiError> {
    let request = state.transcript.initial_history()?;
    let page = read_history(Arc::clone(store), request).await?;
    if !state.transcript.accept_history(&page)? {
        return Err(norn::session_view::ViewError::AttemptMismatch.into());
    }
    Ok(())
}
