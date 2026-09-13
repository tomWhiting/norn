//! Keep the terminal owner responsive while joined execution settles persistent history.

use std::future::Future;
use std::time::Instant;

use norn::agent_loop::ActiveInputSender;
use termina::Event;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::TuiError;
use crate::app::event_loop::is_ctrl_c;
use crate::app::render::{redraw_all, redraw_streaming_tick};
use crate::app::state::AppState;
use crate::terminal::setup::TerminalGuard;

/// Uses the finished turn's existing input semantics, with its receiver closed.
/// A Submit therefore enters the existing follow-up queue, never a dead runner.
pub(super) struct TerminalCompletion<'a> {
    pub guard: &'a mut TerminalGuard,
    pub terminal: &'a mut mpsc::UnboundedReceiver<std::io::Result<Event>>,
    pub active_input: &'a ActiveInputSender,
    pub cancel: &'a CancellationToken,
    pub root_cancel: &'a CancellationToken,
    pub cancel_requested: &'a mut bool,
    pub closed: bool,
    pub tick: &'a mut tokio::time::Interval,
}

impl TerminalCompletion<'_> {
    /// Preserve the settlement future across input, resize and rendering wakeups.
    pub async fn wait<F: Future>(
        &mut self,
        state: &mut AppState,
        future: F,
    ) -> Result<F::Output, TuiError> {
        // Even immediately ready history pages must give queued input a turn.
        // This is one read of already queued input, not a polling loop or timer.
        if !self.closed {
            match self.terminal.try_recv() {
                Ok(event) => self.input(state, Some(event))?,
                Err(mpsc::error::TryRecvError::Disconnected) => self.input(state, None)?,
                Err(mpsc::error::TryRecvError::Empty) => {}
            }
        }
        tokio::pin!(future);
        loop {
            redraw_all(state, self.guard)?;
            tokio::select! {
                output = &mut future => return Ok(output),
                event = self.terminal.recv(), if !self.closed => self.input(state, event)?,
                result = crate::app::frontend_preferences::wait(&mut state.preferences) => {
                    crate::app::frontend_preferences::finish(state, result)?;
                }
                diagnostic = crate::app::diagnostics::wait(&mut state.diagnostics) => {
                    crate::app::diagnostics::finish(state, diagnostic)?;
                }
                update = crate::app::voice::wait(&mut state.voice) => {
                    crate::app::voice::finish(state, update)?;
                }
                Some(result) = state.export_tasks.join_next() => {
                    crate::app::view_actions::reading::finish_export(state, result)?;
                }
                Some(result) = state.screen.changes.jobs.join_next() => {
                    crate::app::render::changes::finish(state, result)?;
                }
                Some(result) = state.agent_conversations.opening.join_next() => {
                    crate::app::agent_conversations::finish(state, result)?;
                }
                Some(result) = state.read_tasks.history.join_next() => {
                    crate::app::view_actions::reading::finish_history(state, result)?;
                }
                Some(result) = state.read_tasks.bodies.join_next() => {
                    crate::app::read_tasks::finish_body(state, result)?;
                }
                _ = self.tick.tick() => {
                    redraw_streaming_tick(state, self.guard, Instant::now())?;
                }
            }
        }
    }

    fn input(
        &mut self,
        state: &mut AppState,
        event: Option<std::io::Result<Event>>,
    ) -> Result<(), TuiError> {
        match event {
            Some(Ok(event)) => {
                crate::app::composer_submission::resolve(state)?;
                state.screen.terminal_event(self.terminal.len());
                state.screen.dirty |= state.exit_confirmation.observe_input(&event);
                if is_ctrl_c(&event) {
                    *self.cancel_requested = true;
                    state.exit_confirmation.interrupt(
                        Instant::now(),
                        self.cancel,
                        self.root_cancel,
                    );
                    state.screen.dirty = true;
                } else {
                    super::super::mid::handle_mid_turn_event(
                        event,
                        state,
                        self.guard,
                        self.active_input,
                        self.cancel,
                        self.cancel_requested,
                    )?;
                }
            }
            Some(Err(error)) => return Err(TuiError::Io(error)),
            None => {
                self.closed = true;
                *self.cancel_requested = true;
                self.cancel.cancel();
            }
        }
        Ok(())
    }
}
