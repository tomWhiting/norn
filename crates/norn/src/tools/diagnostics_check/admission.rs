//! Descriptor admission for diagnostic subprocesses and sockets.

use crate::resource::{
    DescriptorAdmissionError, DescriptorGovernor, DescriptorPermit, TWO_PIPE_SPAWN_PEAK,
};

const DIAGNOSTIC_SOCKET_WEIGHT: u32 = 1;

pub(super) fn acquire_diagnostic_spawn() -> Result<DescriptorPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?.try_acquire(TWO_PIPE_SPAWN_PEAK)
}

pub(super) fn acquire_diagnostic_socket() -> Result<DescriptorPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?.try_acquire(DIAGNOSTIC_SOCKET_WEIGHT)
}
