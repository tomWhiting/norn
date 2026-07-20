//! Driven-mode JSON-RPC 2.0 stdio transport.
//!
//! When Norn is invoked with `--protocol jsonrpc`, this module owns the
//! process's stdin+stdout as a single bidirectional JSON-RPC 2.0 channel.
//! It is entered ONLY when the flag is set; every existing render/TUI path
//! is byte-for-byte unreached without it.
//!
//! The normative wire contract — framing, `initialize` capabilities,
//! the one-shot `run/execute` lifecycle, `event/*` notifications,
//! `intervene/*` requests, the typed stop envelope, and the error-code
//! table — is specified in `docs/design/norn-cli/DRIVEN-PROTOCOL.md`.

pub mod capabilities;
pub mod emitter;
pub mod frames;
pub mod interventions;
pub mod run;
pub mod stdin;
pub mod writer;

pub use capabilities::{DRIVEN_PROTOCOL_VERSION, initialize_capabilities};
pub use emitter::{EventEmitterError, EventEmitterHandle, spawn_event_emitter};
pub use frames::{
    JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, TransportError,
};
pub use interventions::{
    InjectPriority, InterventionHandler, UnavailableInterventionHandler, drive_interventions,
};
pub use run::{
    DrivenRun, PreRunOutcome, RunDriver, SharedRunDriver, drive_pre_run, prompt_from_params,
    send_run_error, send_run_result,
};
pub use stdin::{StdinReader, stdin_reader};
pub use writer::{OutboundWriter, spawn_writer};
