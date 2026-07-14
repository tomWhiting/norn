//! Process-resource diagnostics and narrow private user-level storage.

mod admission;
mod descriptor;
mod descriptor_governor;
mod private_line_log;

pub(crate) use admission::acquire_private_fs;
pub use admission::{
    FilesystemOperationPermit, HttpRequestPermit, SubprocessOperationPermit,
    acquire_filesystem_operation, acquire_http_request, acquire_null_stdio_subprocess,
    acquire_output_subprocess, acquire_recursive_walk,
};
pub use descriptor::{
    DescriptorExhaustion, DescriptorExhaustionKind, DescriptorLimits, DescriptorOpenCount,
    DescriptorSnapshot, classify_descriptor_error, descriptor_snapshot,
};
pub use descriptor_governor::DescriptorAdmissionError;
pub(crate) use descriptor_governor::{DescriptorGovernor, DescriptorPermit};
pub(crate) use descriptor_governor::{
    HTTP_REQUEST_PEAK, NULL_STDIO_SUBPROCESS_PEAK, ONE_PIPE_SPAWN_PEAK, OUTPUT_SUBPROCESS_PEAK,
    PRIVATE_FS_OPERATION_PEAK, RECURSIVE_WALK_PEAK, STDIN_PIPE_NULL_OUTPUT_SPAWN_PEAK,
    THREE_PIPE_RETAINED, THREE_PIPE_SPAWN_PEAK, TWO_PIPE_SPAWN_PEAK,
};
pub use private_line_log::PrivateLineLog;
