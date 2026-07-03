//! Background-process manager: spawn, spool, track, tear down.
//!
//! Norn's bash tool is strictly synchronous — the drain-grace mechanism exists
//! precisely to stop shell-backgrounded children from holding the tool's pipes
//! past process exit. This module adds the one genuinely new primitive
//! everything long-running depends on (INTERNAL-AGENTS §3): a manager that
//! owns a background process's pipes for its whole life, spools its output to
//! disk, and tracks it under a stable short id with **no timeout, no turn
//! limit, and no cap** on process count or spool size — a process lives until
//! it exits or is killed (owner ruling).
//!
//! - [`manager`] — [`ProcessManager`]: spawn/adopt, the registry, shutdown, and
//!   the completion-delivery sink trait.
//! - [`spool`] — [`Spool`]/[`SpoolReader`]: the file-backed, arrival-ordered,
//!   stream-tagged output store with incremental cursor reads and the
//!   committed-length watch.
//! - [`handle`] — [`ProcessHandle`]: status, process-group kill, exit
//!   notification, and the subscription seam.
//! - [`watch`]/[`watch_exec`] — [`Watch`] records, the per-manager
//!   [`WatchRegistry`](watch::WatchRegistry), and the deterministic incremental
//!   filter execution (NP-002) that consumes the subscription seam.
//!
//! ## Watch attach seam (NP-002 / INTERNAL-AGENTS §5)
//!
//! [`ProcessHandle::subscribe`] returns the spool's committed-length watch plus
//! a fresh independent [`SpoolReader`], and [`ProcessHandle::exit_receiver`]
//! the exit-notification watch. Together these are the attach point the
//! deterministic watches of NP-002 consume — a subscriber reacts to new output
//! and to exit without polling and without reaching into manager or spool
//! internals. NP-001 designed the seam; the [`watch`]/[`watch_exec`] modules
//! (NP-002) consume it — the deterministic watch layer attaches here and never
//! reaches into manager or spool internals.

pub mod handle;
pub mod manager;
pub mod spool;
pub mod watch;
pub mod watch_exec;

pub use handle::{ProcessCompletion, ProcessHandle, ProcessStatus};
pub use manager::{ProcessManager, ProcessManagerGuard, ProcessNotifier, SIGNAL_EXIT_CODE};
pub use spool::{Spool, SpoolReader, StreamTag};
pub use watch::{Watch, WatchAlert, WatchAlertKind, WatchAttachError, WatchError};
