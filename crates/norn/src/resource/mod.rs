//! Process-resource diagnostics shared by runtime and CLI surfaces.

mod descriptor;

pub use descriptor::{
    DescriptorExhaustion, DescriptorExhaustionKind, DescriptorLimits, DescriptorOpenCount,
    DescriptorSnapshot, classify_descriptor_error, descriptor_snapshot,
};
