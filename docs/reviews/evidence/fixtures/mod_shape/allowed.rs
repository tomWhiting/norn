//! Module declarations and re-exports are the complete production surface.

mod internal;
pub(crate) mod scoped;

pub use internal::PublicType;
pub(crate) use scoped::CrateType;

#[cfg(test)]
fn test_only_logic_is_excluded() {}
