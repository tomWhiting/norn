//! Complete, race-checked repository observations for policy evaluation.

mod adapter;
mod current;
mod error;
mod git;
mod git_batch;
mod workspace;

pub use adapter::{AcquiredCurrentSnapshot, AcquiredP1Repository, RepositorySnapshotAdapter};
pub use error::{GitEncodingIssue, GitOperation, SnapshotAdapterError, WorkspaceEntryIssue};

#[cfg(test)]
mod tests;
