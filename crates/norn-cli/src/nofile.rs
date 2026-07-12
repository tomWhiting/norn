//! Official-CLI `RLIMIT_NOFILE` initialization.

use std::sync::OnceLock;

use norn::resource::DescriptorLimits;

static INITIALIZATION: OnceLock<NofileInitReport> = OnceLock::new();

/// Finite OS-provided ceiling selected for the soft limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NofileCeiling {
    /// Ceiling value.
    pub value: u64,
    /// OS source that supplied the value.
    pub source: &'static str,
}

/// Result of the official CLI's one startup hardening attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NofileOutcome {
    /// Soft limit was raised to `target`.
    Raised,
    /// Inherited soft limit was already at or above the selected ceiling.
    Unchanged,
    /// Ceiling discovery, mutation, or verification failed.
    Failed {
        /// Locally authored failure detail retained for startup and doctor.
        reason: String,
    },
}

/// Immutable before/after evidence retained for `norn doctor`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NofileInitReport {
    /// Limits inherited from the launching environment.
    pub inherited: DescriptorLimits,
    /// Finite OS ceiling, when discovery succeeded.
    pub ceiling: Option<NofileCeiling>,
    /// Soft-limit value selected without lowering the inherited value.
    pub target: Option<u64>,
    /// Limits observed after the attempt.
    pub effective: DescriptorLimits,
    /// Mutation result.
    pub outcome: NofileOutcome,
}

/// Run the official CLI's one initialization attempt and retain its report.
///
/// Library embedders never call this implicitly; the `norn` binary invokes it
/// before argument parsing so every subcommand and spawned child inherits the
/// effective limit.
pub fn initialize() -> &'static NofileInitReport {
    INITIALIZATION.get_or_init(|| initialize_with(&SystemNofileBackend))
}

/// Report captured by [`initialize`], if the binary initialized it.
#[must_use]
pub fn initialization_report() -> Option<&'static NofileInitReport> {
    INITIALIZATION.get()
}

trait NofileBackend {
    fn limits(&self) -> DescriptorLimits;
    fn ceiling(&self, inherited: DescriptorLimits) -> Result<NofileCeiling, String>;
    fn set_soft(&self, target: u64, hard: Option<u64>) -> Result<(), String>;
}

fn initialize_with(backend: &impl NofileBackend) -> NofileInitReport {
    let inherited = backend.limits();
    let ceiling = match backend.ceiling(inherited) {
        Ok(ceiling) => ceiling,
        Err(reason) => {
            return NofileInitReport {
                inherited,
                ceiling: None,
                target: None,
                effective: inherited,
                outcome: NofileOutcome::Failed { reason },
            };
        }
    };
    let target = inherited
        .hard
        .map_or(ceiling.value, |hard| hard.min(ceiling.value));
    if inherited.soft.is_none_or(|soft| soft >= target) {
        return NofileInitReport {
            inherited,
            ceiling: Some(ceiling),
            target: Some(target),
            effective: inherited,
            outcome: NofileOutcome::Unchanged,
        };
    }
    if let Err(reason) = backend.set_soft(target, inherited.hard) {
        return NofileInitReport {
            inherited,
            ceiling: Some(ceiling),
            target: Some(target),
            effective: backend.limits(),
            outcome: NofileOutcome::Failed { reason },
        };
    }
    let effective = backend.limits();
    let outcome = if effective.soft.is_none_or(|soft| soft >= target) {
        NofileOutcome::Raised
    } else {
        NofileOutcome::Failed {
            reason: format!(
                "RLIMIT_NOFILE verification observed soft limit {} below requested target {target}",
                format_limit(effective.soft),
            ),
        }
    };
    NofileInitReport {
        inherited,
        ceiling: Some(ceiling),
        target: Some(target),
        effective,
        outcome,
    }
}

struct SystemNofileBackend;

#[cfg(unix)]
impl NofileBackend for SystemNofileBackend {
    fn limits(&self) -> DescriptorLimits {
        let limits = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        DescriptorLimits {
            soft: limits.current,
            hard: limits.maximum,
        }
    }

    fn ceiling(&self, inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
        platform_ceiling(inherited)
    }

    fn set_soft(&self, target: u64, hard: Option<u64>) -> Result<(), String> {
        rustix::process::setrlimit(
            rustix::process::Resource::Nofile,
            rustix::process::Rlimit {
                current: Some(target),
                maximum: hard,
            },
        )
        .map_err(|error| format!("failed to raise RLIMIT_NOFILE: {error}"))
    }
}

#[cfg(not(unix))]
impl NofileBackend for SystemNofileBackend {
    fn limits(&self) -> DescriptorLimits {
        DescriptorLimits {
            soft: None,
            hard: None,
        }
    }

    fn ceiling(&self, _inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
        Err("RLIMIT_NOFILE is unavailable on this platform".to_owned())
    }

    fn set_soft(&self, _target: u64, _hard: Option<u64>) -> Result<(), String> {
        Err("RLIMIT_NOFILE is unavailable on this platform".to_owned())
    }
}

#[cfg(target_os = "macos")]
fn platform_ceiling(_inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.maxfilesperproc"])
        .output()
        .map_err(|error| format!("failed to query kern.maxfilesperproc: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "kern.maxfilesperproc query exited with status {}",
            output.status,
        ));
    }
    let raw = String::from_utf8(output.stdout)
        .map_err(|error| format!("kern.maxfilesperproc was not UTF-8: {error}"))?;
    let value = raw
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("kern.maxfilesperproc was not an integer: {error}"))?;
    Ok(NofileCeiling {
        value,
        source: "kern.maxfilesperproc",
    })
}

#[cfg(target_os = "linux")]
fn platform_ceiling(_inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
    let raw = std::fs::read_to_string("/proc/sys/fs/nr_open")
        .map_err(|error| format!("failed to read /proc/sys/fs/nr_open: {error}"))?;
    let value = raw
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("/proc/sys/fs/nr_open was not an integer: {error}"))?;
    Ok(NofileCeiling {
        value,
        source: "/proc/sys/fs/nr_open",
    })
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn platform_ceiling(inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
    inherited
        .hard
        .map(|value| NofileCeiling {
            value,
            source: "RLIMIT_NOFILE hard limit",
        })
        .ok_or_else(|| {
            "RLIMIT_NOFILE hard limit is unlimited and no finite platform ceiling is available"
                .to_owned()
        })
}

fn format_limit(value: Option<u64>) -> String {
    value.map_or_else(|| "unlimited".to_owned(), |limit| limit.to_string())
}

#[cfg(test)]
#[path = "nofile_tests.rs"]
mod tests;
