//! Trust provenance for model-facing skill metadata.

/// Trust provenance assigned by discovery, never parsed from skill content.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SkillOrigin {
    /// A caller-trusted path supplied to the low-level compatibility API.
    Operator,
    /// A path proven to be beneath the immutable workspace root.
    Workspace,
}
