//! Typed file-descriptor exhaustion and live process observations.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Whether the process limit or the system-wide file table was exhausted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorExhaustionKind {
    /// `EMFILE`: this process reached its open-file descriptor limit.
    Process,
    /// `ENFILE`: the operating system's file table is exhausted.
    System,
}

/// Soft and hard `RLIMIT_NOFILE` values; `None` means OS-reported infinity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DescriptorLimits {
    /// Effective per-process limit.
    pub soft: Option<u64>,
    /// Maximum value the process may set without additional privilege.
    pub hard: Option<u64>,
}

/// A labelled open-descriptor count.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DescriptorOpenCount {
    /// Entries observed in the platform descriptor directory.
    pub count: u64,
    /// Fixed platform observation path (`/dev/fd` or `/proc/self/fd`).
    pub source: &'static str,
    /// Directory enumeration temporarily owns one descriptor included in the count.
    pub includes_observer: bool,
}

/// Best-effort observations captured when diagnosing descriptor pressure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DescriptorSnapshot {
    /// Current limits, absent only on unsupported platforms.
    pub limits: Option<DescriptorLimits>,
    /// Why limits could not be observed.
    pub limits_error: Option<String>,
    /// Current open count when the platform observation directory was readable.
    pub open: Option<DescriptorOpenCount>,
    /// Why the open count could not be observed.
    pub open_error: Option<String>,
}

/// A self-diagnosing typed `EMFILE` or `ENFILE` failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DescriptorExhaustion {
    /// Which descriptor pool was exhausted.
    pub kind: DescriptorExhaustionKind,
    /// Locally authored operation label.
    pub operation: String,
    /// Relevant path, when the caller has one.
    pub path: Option<PathBuf>,
    /// Limits and usage observed immediately after the failure.
    pub snapshot: DescriptorSnapshot,
}

impl fmt::Display for DescriptorExhaustion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scope = match self.kind {
            DescriptorExhaustionKind::Process => "process file-descriptor limit",
            DescriptorExhaustionKind::System => "system-wide open-file table",
        };
        write!(f, "{scope} exhausted while {}", self.operation)?;
        if let Some(path) = &self.path {
            write!(f, " at {}", path.display())?;
        }
        if let Some(open) = &self.snapshot.open {
            write!(f, "; observed {} open descriptors", open.count)?;
        }
        if let Some(limits) = self.snapshot.limits {
            write!(
                f,
                "; soft limit {}, hard limit {}",
                format_limit(limits.soft),
                format_limit(limits.hard),
            )?;
        }
        f.write_str("; run `norn doctor` for descriptor diagnostics")
    }
}

impl std::error::Error for DescriptorExhaustion {}

/// Convert only `EMFILE` and `ENFILE` into a typed diagnostic.
#[must_use]
pub fn classify_descriptor_error(
    error: &io::Error,
    operation: impl Into<String>,
    path: Option<&Path>,
) -> Option<DescriptorExhaustion> {
    #[cfg(unix)]
    {
        let errno = rustix::io::Errno::from_raw_os_error(error.raw_os_error()?);
        let kind = match errno {
            rustix::io::Errno::MFILE => DescriptorExhaustionKind::Process,
            rustix::io::Errno::NFILE => DescriptorExhaustionKind::System,
            _ => return None,
        };
        Some(DescriptorExhaustion {
            kind,
            operation: operation.into(),
            path: path.map(Path::to_path_buf),
            snapshot: descriptor_snapshot(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (error, operation, path);
        None
    }
}

/// Observe current descriptor limits and open count without mutating either.
#[must_use]
pub fn descriptor_snapshot() -> DescriptorSnapshot {
    let (limits, limits_error) = current_limits();
    let (open, open_error) = match current_open_count() {
        Ok(observation) => (Some(observation), None),
        Err(error) => (None, Some(error.to_string())),
    };
    DescriptorSnapshot {
        limits,
        limits_error,
        open,
        open_error,
    }
}

#[cfg(unix)]
fn current_limits() -> (Option<DescriptorLimits>, Option<String>) {
    let value = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    (
        Some(DescriptorLimits {
            soft: value.current,
            hard: value.maximum,
        }),
        None,
    )
}

#[cfg(not(unix))]
fn current_limits() -> (Option<DescriptorLimits>, Option<String>) {
    (
        None,
        Some("RLIMIT_NOFILE is unavailable on this platform".to_owned()),
    )
}

fn current_open_count() -> io::Result<DescriptorOpenCount> {
    #[cfg(target_os = "macos")]
    const SOURCE: &str = "/dev/fd";
    #[cfg(target_os = "linux")]
    const SOURCE: &str = "/proc/self/fd";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "open-descriptor counting is unavailable on this platform",
    ));

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut count = 0_u64;
        for entry in std::fs::read_dir(SOURCE)? {
            let _ = entry?;
            count = count.checked_add(1).ok_or_else(|| {
                io::Error::other("open-descriptor count exceeded the u64 representation")
            })?;
        }
        Ok(DescriptorOpenCount {
            count,
            source: SOURCE,
            includes_observer: true,
        })
    }
}

fn format_limit(limit: Option<u64>) -> String {
    limit.map_or_else(|| "unlimited".to_owned(), |value| value.to_string())
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod tests;
