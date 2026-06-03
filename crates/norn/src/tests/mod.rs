//! Integration tests for the Norn crate.
//!
//! These tests exercise real, integrated code paths through multiple
//! subsystems: provider → loop → tool dispatch → real tools → session store.
//! They use [`MockProvider`] for scripted provider responses and real
//! filesystem operations via `tempfile`.

pub mod integration;
