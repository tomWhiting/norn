//! Turn input sources and the initial state reset before execution.

use norn::agent_loop::inbound::ChannelMessage;

use crate::app::state::AppState;
use crate::render::streaming_indicator::StreamingIndicator;

pub(super) enum TurnSeed {
    Operator(crate::app::transcript::publication::SubmittedInput),
    ChildResult(String),
    AgentMessages(Vec<ChannelMessage>),
    McpChannelWake,
}

pub(super) fn reset_turn_state(state: &mut AppState) {
    state.turn_start = None;
    state.complete_at = None;
    state.streaming_indicator = StreamingIndicator::Idle;
    state.reset_live_usage();
}
