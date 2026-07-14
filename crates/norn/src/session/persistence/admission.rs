//! Descriptor admission for session persistence transactions.

use super::SessionPersistError;

pub(crate) fn acquire_private_fs() -> Result<crate::resource::DescriptorPermit, SessionPersistError>
{
    crate::resource::DescriptorGovernor::global()?
        .try_acquire(crate::resource::PRIVATE_FS_OPERATION_PEAK)
        .map_err(Into::into)
}
