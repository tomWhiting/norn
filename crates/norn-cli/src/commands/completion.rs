//! `norn completion <SHELL>` subcommand (NC-008 R14).
//!
//! Generates a shell completion script for `bash`, `zsh`, or `fish`
//! using `clap_complete::generate`. The output is written to stdout so
//! it can be redirected straight into a shell's config tree
//! (`norn completion zsh > ~/.zsh/_norn`). Unsupported shells produce
//! an argument-error exit code (2).

use clap::CommandFactory;
use clap_complete::Shell;

use crate::cli::ExitCode;
use crate::cli::{Cli, CompletionArgs};

/// Dispatcher for `norn completion`.
pub fn run_completion(args: &CompletionArgs) -> ExitCode {
    let shell = match args.shell.as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        other => {
            eprintln!("norn: unsupported shell: {other}; supported: bash, zsh, fish");
            return ExitCode::ArgumentError;
        }
    };

    let mut cmd = Cli::command();
    let mut stdout = std::io::stdout();
    clap_complete::generate(shell, &mut cmd, "norn", &mut stdout);
    ExitCode::Success
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(shell: &str) -> CompletionArgs {
        CompletionArgs {
            shell: shell.to_owned(),
        }
    }

    #[test]
    fn bash_is_accepted() {
        // The function writes to stdout; the return code is the
        // observable behaviour we can assert on without capturing
        // stdout.
        let code = run_completion(&args("bash"));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn zsh_is_accepted() {
        let code = run_completion(&args("zsh"));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn fish_is_accepted() {
        let code = run_completion(&args("fish"));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn powershell_is_rejected_as_argument_error() {
        let code = run_completion(&args("powershell"));
        assert_eq!(code, ExitCode::ArgumentError);
    }

    #[test]
    fn unknown_shell_is_rejected_as_argument_error() {
        let code = run_completion(&args("tcsh"));
        assert_eq!(code, ExitCode::ArgumentError);
    }
}
