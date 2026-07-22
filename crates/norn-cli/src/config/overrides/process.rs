use crate::cli::{BuildError, Cli};

/// Switch the process working directory when `--working-dir` is set.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when the directory cannot be entered.
pub fn apply_working_dir(cli: &Cli) -> Result<(), BuildError> {
    if let Some(dir) = cli.working_dir.as_deref() {
        std::env::set_current_dir(dir).map_err(|err| {
            BuildError::Argument(format!(
                "failed to set working directory {}: {err}",
                dir.display(),
            ))
        })?;
    }
    Ok(())
}
