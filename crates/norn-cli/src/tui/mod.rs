//! TUI execution mode — live agent interaction through norn-tui primitives.

pub mod driver;
mod startup_trace;

pub use driver::run;
