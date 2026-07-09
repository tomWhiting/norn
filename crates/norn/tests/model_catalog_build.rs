//! Build-time model-catalog validation tests.

// The build script owns catalog validation. Including it here runs its focused
// unit tests under the normal Cargo test harness without duplicating that logic.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../build.rs"]
mod catalog_build;
