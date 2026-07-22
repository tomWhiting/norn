//! Handle-returning Rhai agent operations.

use rhai::Engine;

use super::context::NornRhaiContext;

mod signal;
mod spawn;

pub(super) fn register_handle_returning(engine: &mut Engine, context: &NornRhaiContext) {
    spawn::register(engine, context);
    signal::register(engine, context);
}

#[cfg(test)]
mod tests;
