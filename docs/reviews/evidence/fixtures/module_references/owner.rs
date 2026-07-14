//! Test-only classification must retain reference provenance across path aliases.

#[path = "production_only/mod.rs"]
mod production_only;

#[cfg(test)]
#[path = "shared/mod.rs"]
mod shared_test_alias;

#[path = "shared/mod.rs"]
mod shared_production_alias;

#[cfg(test)]
#[path = "test_only/mod.rs"]
mod test_only_alias;

include !("included/mod.rs");

#[cfg(test)]
include!("test_included/mod.rs");
