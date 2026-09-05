//! Strict inline channel policy parsing with document contents withheld from errors.

use norn::config::ChannelSettings;

use crate::cli::BuildError;

pub(super) fn parse_channel_overrides(value: &str) -> Result<ChannelSettings, BuildError> {
    serde_json::from_str(value).map_err(|error| {
        BuildError::Argument(format!(
            "invalid -c channels JSON at line {}, column {}; expected channel policy, sources, positive retention limits and reject-new overflow (values withheld)",
            error.line(), error.column(),
        ))
    })
}

#[cfg(test)]
#[path = "channel_overrides_tests.rs"]
mod tests;
