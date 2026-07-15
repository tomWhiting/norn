use clap::Parser as _;

use super::{Cli, Command};
use crate::cli::{PolicyCmd, PolicyOutputFormat};

#[test]
fn policy_check_requires_and_parses_explicit_format() -> Result<(), clap::Error> {
    assert!(Cli::try_parse_from(["norn", "policy", "check"]).is_err());
    let cli = Cli::try_parse_from(["norn", "policy", "check", "--format", "json"])?;
    assert!(matches!(
        cli.command,
        Some(Command::Policy {
            command: PolicyCmd::Check {
                format: PolicyOutputFormat::Json,
            },
        })
    ));
    Ok(())
}
