//! Explicit checkpoint barrier for the actual-App terminal responsiveness fixture.

use std::io;
use std::sync::{Arc, Condvar, Mutex};

use norn::session::events::SessionEvent;
use norn::session::persistence::SessionPersistError;
use norn::session::store::PersistenceSink;

#[derive(Default)]
struct State {
    entered: bool,
    released: bool,
    finished: bool,
}

/// Test-owned barrier; notifications, not sleeps, establish checkpoint entry.
#[derive(Default)]
pub struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

impl Gate {
    /// Wait until the actual store checkpoint has entered the sink.
    pub fn wait_entered(&self) -> io::Result<()> {
        self.wait_for(|state| state.entered)
    }

    /// Release this and all subsequent checkpoint calls.
    pub fn release(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(poison)?;
        state.released = true;
        self.changed.notify_all();
        Ok(())
    }

    /// Refuse a false responsiveness proof after the checkpoint timed out or ended.
    pub fn confirm_held(&self) -> io::Result<()> {
        let state = self.state.lock().map_err(poison)?;
        if !state.entered || state.released || state.finished {
            return Err(io::Error::other("checkpoint is not still held"));
        }
        Ok(())
    }

    fn checkpoint(&self) -> io::Result<()> {
        {
            let mut state = self.state.lock().map_err(poison)?;
            state.entered = true;
            self.changed.notify_all();
        }
        let result = self.wait_for(|state| state.released);
        self.state.lock().map_err(poison)?.finished = true;
        self.changed.notify_all();
        result
    }

    fn wait_for(&self, ready: impl Fn(&State) -> bool) -> io::Result<()> {
        let state = self.state.lock().map_err(poison)?;
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, super::DEADLINE, |state| !ready(state))
            .map_err(poison)?;
        if timeout.timed_out() && !ready(&state) {
            return Err(io::Error::other("completion checkpoint barrier timed out"));
        }
        Ok(())
    }
}

fn poison(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("completion checkpoint barrier lock: {error}"))
}

/// No durability claim: only the fixture's checkpoint is deliberately held.
pub struct FixtureSink(pub Arc<Gate>);

impl PersistenceSink for FixtureSink {
    fn persist(&mut self, _: &SessionEvent) -> Result<(), SessionPersistError> {
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), SessionPersistError> {
        self.0.checkpoint().map_err(SessionPersistError::Io)
    }
}

/// First opening-input publication is held; one optional rejection precedes ordinary admission.
pub struct OpeningSink {
    pub gate: Arc<Gate>,
    pub reject: bool,
    pub entered: bool,
}

impl PersistenceSink for OpeningSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        if !self.entered && matches!(event, SessionEvent::UserMessage { .. }) {
            self.entered = true;
            self.gate.checkpoint().map_err(SessionPersistError::Io)?;
            if self.reject {
                return Err(SessionPersistError::Io(io::Error::other(
                    "fixture rejects opening input",
                )));
            }
        }
        Ok(())
    }
}
