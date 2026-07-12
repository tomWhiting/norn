use std::io;
use std::path::Path;

use super::{
    DescriptorExhaustionKind, classify_descriptor_error, descriptor_snapshot, format_limit,
};

#[cfg(unix)]
#[test]
fn classifies_process_and_system_exhaustion_without_matching_prose()
-> Result<(), Box<dyn std::error::Error>> {
    let process = io::Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error());
    let system = io::Error::from_raw_os_error(rustix::io::Errno::NFILE.raw_os_error());

    let process = classify_descriptor_error(&process, "opening a session", Some(Path::new("x")))
        .ok_or_else(|| io::Error::other("EMFILE was not classified"))?;
    let system = classify_descriptor_error(&system, "opening a session", None)
        .ok_or_else(|| io::Error::other("ENFILE was not classified"))?;

    assert_eq!(process.kind, DescriptorExhaustionKind::Process);
    assert_eq!(system.kind, DescriptorExhaustionKind::System);
    assert_eq!(process.path.as_deref(), Some(Path::new("x")));
    Ok(())
}

#[test]
fn unrelated_io_error_is_not_reclassified() {
    let error = io::Error::new(io::ErrorKind::PermissionDenied, "sentinel");
    assert!(classify_descriptor_error(&error, "reading", None).is_none());
}

#[test]
fn snapshot_never_invents_missing_observations() {
    let snapshot = descriptor_snapshot();
    assert_eq!(snapshot.limits.is_none(), snapshot.limits_error.is_some());
    assert_eq!(snapshot.open.is_none(), snapshot.open_error.is_some());
    if let Some(open) = snapshot.open {
        assert!(open.includes_observer);
        assert!(matches!(open.source, "/dev/fd" | "/proc/self/fd"));
    }
}

#[test]
fn limit_rendering_distinguishes_infinity_from_zero() {
    assert_eq!(format_limit(None), "unlimited");
    assert_eq!(format_limit(Some(0)), "0");
}

#[cfg(unix)]
#[test]
fn persistence_conversion_preserves_process_exhaustion_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let error = io::Error::from_raw_os_error(rustix::io::Errno::MFILE.raw_os_error());
    let persistence = crate::session::SessionPersistError::from(error);
    let session = crate::error::SessionError::from(persistence);

    let crate::error::SessionError::DescriptorExhausted(exhaustion) = session else {
        return Err(io::Error::other("session conversion flattened descriptor exhaustion").into());
    };
    assert_eq!(exhaustion.kind, DescriptorExhaustionKind::Process);
    Ok(())
}

#[cfg(unix)]
#[test]
fn process_failure_reaches_structured_tool_payload() {
    let error = io::Error::from_raw_os_error(rustix::io::Errno::NFILE.raw_os_error());
    let process = crate::process::ProcessError::from_io(
        "creating a process spool",
        Some(Path::new("spool.log")),
        &error,
    );
    let tool = crate::error::ToolError::from(process);
    let payload = crate::tool::failure::ToolErrorPayload::from(&tool);

    assert_eq!(
        payload.kind,
        crate::tool::failure::ToolErrorKind::ResourceExhausted,
    );
    assert_eq!(payload.detail["descriptor"]["kind"], "system");
    assert_eq!(payload.detail["descriptor"]["path"], "spool.log");
    assert!(payload.message.contains("norn doctor"));
}
