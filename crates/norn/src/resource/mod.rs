//! Process-resource diagnostics and narrow private user-level storage.

mod descriptor;
mod descriptor_governor;
mod private_line_log;

pub use descriptor::{
    DescriptorExhaustion, DescriptorExhaustionKind, DescriptorLimits, DescriptorOpenCount,
    DescriptorSnapshot, classify_descriptor_error, descriptor_snapshot,
};
pub use descriptor_governor::DescriptorAdmissionError;
pub(crate) use descriptor_governor::{DescriptorGovernor, DescriptorPermit};
pub(crate) use descriptor_governor::{
    HTTP_REQUEST_PEAK, ONE_PIPE_SPAWN_PEAK, PRIVATE_FS_OPERATION_PEAK, THREE_PIPE_RETAINED,
    THREE_PIPE_SPAWN_PEAK, TWO_PIPE_SPAWN_PEAK,
};
pub use private_line_log::PrivateLineLog;
