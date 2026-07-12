//! Descriptor-free process-group signalling for cleanup paths.

use std::io;

/// Send `SIGKILL` to the Unix process group identified by `pid`.
#[cfg(unix)]
pub(crate) fn kill_process_group(pid: u32) -> io::Result<()> {
    let raw_pid = i32::try_from(pid).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("process id {pid} is outside the platform range: {error}"),
        )
    })?;
    let pid = rustix::process::Pid::from_raw(raw_pid).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group id must be non-zero",
        )
    })?;
    rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).map_err(io::Error::from)
}
