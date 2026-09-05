//! Named test-prerequisite errors reported by the test harness without tracing.

use std::io;

/// Marker declared by `gates.json` for an unmet test prerequisite.
pub const MARKER: &str = "NORN_TEST_PREREQUISITE_UNMET";

/// Build an observable prerequisite error naming its test and missing condition.
#[must_use]
pub fn missing(test: &str, condition: &str) -> io::Error {
    io::Error::other(format!("{MARKER}: {test}: {condition}"))
}

/// Require a test's actual prerequisite instead of returning a passing skip.
///
/// # Errors
/// Returns a named error when `satisfied` is false.
pub fn require(satisfied: bool, test: &str, condition: &str) -> io::Result<()> {
    if satisfied {
        Ok(())
    } else {
        Err(missing(test, condition))
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::process::Command;

    use super::{MARKER, require};

    const PROBE: &str = "NORN_TEST_PREREQUISITE_FAILURE_PROBE";

    /// A subprocess probe driven by `missing_prerequisite_fails_without_tracing`.
    #[test]
    fn prerequisite_child_probe() -> Result<(), Box<dyn Error>> {
        if std::env::var_os(PROBE).is_some() {
            require(
                false,
                "prerequisite_child_probe",
                "fixture prerequisite absent",
            )?;
        }
        Ok(())
    }

    #[test]
    fn missing_prerequisite_fails_without_tracing() -> Result<(), Box<dyn Error>> {
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "test_prerequisite::tests::prerequisite_child_probe",
                "--nocapture",
            ])
            .env(PROBE, "missing")
            .output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains(MARKER),
            "missing prerequisite marker: {stderr}"
        );
        assert!(stderr.contains("fixture prerequisite absent"));
        Ok(())
    }

    #[test]
    fn satisfied_prerequisite_succeeds() -> Result<(), Box<dyn Error>> {
        require(true, "satisfied_prerequisite_succeeds", "fixture available")?;
        Ok(())
    }

    #[test]
    fn release_declaration_uses_the_emitted_marker() -> Result<(), Box<dyn Error>> {
        let declaration: serde_json::Value =
            serde_json::from_str(include_str!("../../../gates.json"))?;
        assert_eq!(declaration["vacuity_marker"].as_str(), Some(MARKER));
        Ok(())
    }
}
