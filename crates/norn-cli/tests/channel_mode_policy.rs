//! Actual CLI policy refusals occur before stdin, provider or MCP startup.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

type TestResult = Result<(), Box<dyn Error>>;

struct Invocation {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    marker: PathBuf,
}

impl Invocation {
    fn new() -> Result<Self, Box<dyn Error>> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        let marker = project.path().join("channel-source-must-not-start");
        std::fs::write(
            home.path().join("settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "mcp_servers": {
                    "source": {"command": "/usr/bin/touch", "args": [&marker]}
                }
            }))?,
        )?;
        Ok(Self {
            home,
            project,
            marker,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_norn"));
        command
            .current_dir(self.project.path())
            .env("HOME", self.home.path())
            .env("NORN_HOME", self.home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn refuse(&self, mode: &[&str], policy: &str) -> Result<Output, Box<dyn Error>> {
        let output = self
            .command()
            .args(mode)
            .args([
                "--channel",
                &format!("source={policy}"),
                "--channel-max-retained-messages",
                "3",
                "--channel-max-retained-bytes",
                "2048",
                "--channel-overflow",
                "reject-new",
            ])
            .output()?;
        assert_argument_refusal(&output, &self.marker)?;
        Ok(output)
    }
}

fn assert_argument_refusal(output: &Output, marker: &Path) -> TestResult {
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    assert!(
        !marker.exists(),
        "MCP source started before policy validation"
    );
    Ok(())
}

#[test]
fn next_turn_is_refused_in_explicit_and_pipe_selected_print() -> TestResult {
    let invocation = Invocation::new()?;
    for mode in [
        &["-p", "-f", "json"][..],
        &["-p", "-f", "stream-json"][..],
        &["-f", "json"][..],
    ] {
        let output = invocation.refuse(mode, "next-turn")?;
        let stderr = std::str::from_utf8(&output.stderr)?;
        for referent in [
            "channel source 'source'",
            "next-turn",
            "one-shot print mode",
            "no later turn",
        ] {
            assert!(stderr.contains(referent), "{stderr}");
        }
    }
    Ok(())
}

#[test]
fn driven_next_turn_is_refused_before_waiting_for_a_run_request() -> TestResult {
    let invocation = Invocation::new()?;
    // Stdin is already at EOF: a pre-run driven wait would exit successfully.
    // The mode refusal must happen first, without accepting any run/execute.
    let output = invocation.refuse(&["--protocol", "jsonrpc"], "next-turn")?;
    let stderr = std::str::from_utf8(&output.stderr)?;
    for referent in [
        "channel source 'source'",
        "next-turn",
        "one-shot driven JSON-RPC mode",
        "no later turn",
    ] {
        assert!(stderr.contains(referent), "{stderr}");
    }
    Ok(())
}

#[test]
fn hold_is_refused_by_name_before_any_cli_launch() -> TestResult {
    let invocation = Invocation::new()?;
    for mode in [
        &[][..],
        &["-p", "-f", "json"][..],
        &["--protocol", "jsonrpc"][..],
    ] {
        let output = invocation.refuse(mode, "hold")?;
        let stderr = std::str::from_utf8(&output.stderr)?;
        for referent in [
            "channel source 'source'",
            "hold",
            "every CLI mode",
            "release/deny",
        ] {
            assert!(stderr.contains(referent), "{stderr}");
        }
    }
    Ok(())
}

#[test]
fn help_states_the_cli_policy_lifetime_and_hold_boundary() -> TestResult {
    let invocation = Invocation::new()?;
    let output = invocation.command().arg("--help").output()?;
    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout)?;
    let help = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    for statement in [
        "NAME=off|next-turn|wake",
        "Next-turn requires the interactive TUI",
        "Wake in print/driven mode joins the active run only",
        "Hold is unavailable until CLI inbox release/deny controls exist",
    ] {
        assert!(help.contains(statement), "help: {help}");
    }
    assert!(!help.contains("NAME=hold"), "help: {help}");
    assert!(!invocation.marker.exists());
    Ok(())
}
