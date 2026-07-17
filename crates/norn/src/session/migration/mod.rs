//! Explicit offline migration from legacy sessions to the strict store.
//!
//! The normal runtime does not call this module and it never resolves a path
//! from environment state. Callers supply the absolute Norn root explicitly.

mod classify;
mod classify_relationships;
mod cutover;
mod error;
mod inspect;
mod json;
mod legacy_index;
mod manifest_sources;
mod stage;
mod stage_ownership;
mod transaction;
mod tree;
mod types;

pub use error::SessionMigrationError;
pub use inspect::{
    export_legacy_session_raw, read_legacy_migration_manifest, verify_legacy_session_cutover,
    verify_legacy_session_migration,
};
pub use transaction::migrate_legacy_sessions;
pub use types::{
    LEGACY_SESSION_DIRECTORY, LegacyClassificationReason, LegacySessionMigrationRecord,
    MIGRATION_MANIFEST_FILE, MigrationCounts, STRICT_SESSION_DIRECTORY, SessionMigrationManifest,
    SessionMigrationOutcome,
};

#[cfg(test)]
mod hardening_tests;
#[cfg(test)]
mod tests;
