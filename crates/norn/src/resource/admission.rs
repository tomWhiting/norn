//! Typed admission guards for descriptor-owning operations.

use super::{
    DescriptorAdmissionError, DescriptorGovernor, DescriptorPermit, HTTP_REQUEST_PEAK,
    NULL_STDIO_SUBPROCESS_PEAK, OUTPUT_SUBPROCESS_PEAK, PRIVATE_FS_OPERATION_PEAK,
    RECURSIVE_WALK_PEAK,
};

/// Admit one descriptor-relative or ordinary filesystem operation.
pub(crate) fn acquire_private_fs() -> Result<DescriptorPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?.try_acquire(PRIVATE_FS_OPERATION_PEAK)
}

/// Opaque admission guard for one ordinary filesystem operation.
#[derive(Debug)]
#[must_use = "dropping the guard releases filesystem descriptor admission"]
pub struct FilesystemOperationPermit {
    _permit: DescriptorPermit,
}

/// Admit one ordinary filesystem operation without exposing permit internals.
///
/// The returned guard must remain alive until every file or directory handle
/// opened by the operation has closed.
///
/// # Errors
///
/// Returns a self-diagnosing admission error when the process-wide safe
/// descriptor budget cannot cover the operation.
pub fn acquire_filesystem_operation() -> Result<FilesystemOperationPermit, DescriptorAdmissionError>
{
    acquire_private_fs().map(|permit| FilesystemOperationPermit { _permit: permit })
}

/// Admit one serial `.gitignore`-aware recursive walk.
///
/// # Errors
///
/// Returns a self-diagnosing admission error when the process-wide budget
/// cannot cover the dependency's source-derived open-handle peak.
pub fn acquire_recursive_walk() -> Result<FilesystemOperationPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?
        .try_acquire(RECURSIVE_WALK_PEAK)
        .map(|permit| FilesystemOperationPermit { _permit: permit })
}

/// Opaque admission guard for one synchronous subprocess operation.
#[derive(Debug)]
#[must_use = "dropping the guard releases subprocess descriptor admission"]
pub struct SubprocessOperationPermit {
    _permit: DescriptorPermit,
}

/// Admit a subprocess with null stdin and captured stdout/stderr.
///
/// # Errors
///
/// Returns a self-diagnosing admission error when the process-wide budget
/// cannot cover the subprocess launch peak.
pub fn acquire_output_subprocess() -> Result<SubprocessOperationPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?
        .try_acquire(OUTPUT_SUBPROCESS_PEAK)
        .map(|permit| SubprocessOperationPermit { _permit: permit })
}

/// Admit a subprocess with all three standard streams attached to `/dev/null`.
///
/// # Errors
///
/// Returns a self-diagnosing admission error when the process-wide budget
/// cannot cover the subprocess launch peak.
pub fn acquire_null_stdio_subprocess() -> Result<SubprocessOperationPermit, DescriptorAdmissionError>
{
    DescriptorGovernor::global()?
        .try_acquire(NULL_STDIO_SUBPROCESS_PEAK)
        .map(|permit| SubprocessOperationPermit { _permit: permit })
}

/// Opaque admission guard for one active HTTP request.
#[derive(Debug)]
#[must_use = "dropping the guard releases HTTP descriptor admission"]
pub struct HttpRequestPermit {
    _permit: DescriptorPermit,
}

/// Admit one HTTP request through response-body or stream completion.
///
/// Clients used with this guard must disable idle connection pooling so no
/// socket can outlive the guard merely by returning to a pool.
///
/// # Errors
///
/// Returns a self-diagnosing admission error when the process-wide budget
/// cannot cover the request's resolver and connection peak.
pub fn acquire_http_request() -> Result<HttpRequestPermit, DescriptorAdmissionError> {
    DescriptorGovernor::global()?
        .try_acquire(HTTP_REQUEST_PEAK)
        .map(|permit| HttpRequestPermit { _permit: permit })
}
