//! Process-resource diagnostics and narrow private user-level storage.

mod descriptor;
mod private_line_log;

pub use descriptor::{
    DescriptorExhaustion, DescriptorExhaustionKind, DescriptorLimits, DescriptorOpenCount,
    DescriptorSnapshot, classify_descriptor_error, descriptor_snapshot,
};
pub use private_line_log::PrivateLineLog;
