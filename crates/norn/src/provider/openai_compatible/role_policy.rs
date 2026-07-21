//! Developer-message compatibility policy for Chat Completions backends.

use serde_json::Value;

use crate::error::ProviderError;

pub(super) const OPTION_KEY: &str = "norn_developer_role_policy";

/// How an OpenAI-compatible Chat backend represents developer authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum DeveloperRolePolicy {
    /// Emit the current Chat Completions `developer` role without alteration.
    #[default]
    Native,
    /// Reject developer messages locally for a backend known not to support them.
    Reject,
    /// Explicitly lower developer messages to user authority for a legacy backend.
    DowngradeToUser,
}

impl DeveloperRolePolicy {
    /// Resolve the provider-pinned policy from provider options.
    pub(super) fn from_provider_options(options: Option<&Value>) -> Result<Self, ProviderError> {
        let Some(object) = options.and_then(Value::as_object) else {
            return Ok(Self::Native);
        };
        let direct = object.get(OPTION_KEY);
        let scoped = selected_chat_options(object).and_then(|options| options.get(OPTION_KEY));
        let selected = match (direct, scoped) {
            (Some(_), Some(_)) => {
                return Err(invalid_policy(
                    "must be configured once, either at provider.options or inside the selected Chat Completions options",
                ));
            }
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        let Some(value) = selected else {
            return Ok(Self::Native);
        };
        match value.as_str() {
            Some("native") => Ok(Self::Native),
            Some("reject") => Ok(Self::Reject),
            Some("downgrade_to_user") => Ok(Self::DowngradeToUser),
            _ => Err(invalid_policy(
                "must be one of native, reject, or downgrade_to_user",
            )),
        }
    }

    /// Resolve the exact wire role for a developer message.
    pub(super) fn wire_role(self) -> Result<&'static str, ProviderError> {
        match self {
            Self::Native => Ok("developer"),
            Self::Reject => Err(ProviderError::UnsupportedFeature {
                feature: "developer messages on this legacy Chat Completions backend".to_owned(),
            }),
            Self::DowngradeToUser => Ok("user"),
        }
    }
}

fn selected_chat_options(
    options: &serde_json::Map<String, Value>,
) -> Option<&serde_json::Map<String, Value>> {
    options
        .get("api_options")
        .and_then(|value| value.get("openai_chat_completions"))
        .and_then(Value::as_object)
        .or_else(|| {
            options
                .get("openai_chat_completions")
                .and_then(Value::as_object)
        })
}

fn invalid_policy(reason: &str) -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!("provider option {OPTION_KEY} {reason}"),
    }
}

#[cfg(test)]
#[path = "role_policy_tests.rs"]
mod tests;
