//! Reject requests the Claude model-only adapter cannot represent faithfully.

use claude_runner::EffortLevel;

use crate::error::ProviderError;
use crate::provider::request::{ProviderRequest, ReasoningEffort};

pub(super) fn reject_canonical_response_items(
    request: &ProviderRequest,
) -> Result<(), ProviderError> {
    if request
        .messages
        .iter()
        .any(|message| !message.response_items.is_empty())
    {
        return Err(ProviderError::UnsupportedFeature {
            feature: "canonical Responses item replay through Claude Runner".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn reject_unbound_tools(request: &ProviderRequest) -> Result<(), ProviderError> {
    if request.tools.is_empty() {
        return Ok(());
    }
    Err(ProviderError::UnsupportedFeature {
        feature: "Norn tool schemas through the model-only Claude Runner adapter; use \
                  NornWrappedClaudeSession with a strict Norn MCP server"
            .to_owned(),
    })
}

pub(super) fn claude_effort(
    request: &ProviderRequest,
) -> Result<Option<EffortLevel>, ProviderError> {
    match request.reasoning_effort {
        None => Ok(None),
        Some(effort @ (ReasoningEffort::None | ReasoningEffort::Ultra)) => {
            Err(ProviderError::UnsupportedFeature {
                feature: format!(
                    "reasoning effort '{}' through Claude Runner",
                    effort.as_str()
                ),
            })
        }
        Some(ReasoningEffort::Low) => Ok(Some(EffortLevel::Low)),
        Some(ReasoningEffort::Medium) => Ok(Some(EffortLevel::Medium)),
        Some(ReasoningEffort::High) => Ok(Some(EffortLevel::High)),
        Some(ReasoningEffort::XHigh) => Ok(Some(EffortLevel::XHigh)),
        Some(ReasoningEffort::Max) => Ok(Some(EffortLevel::Max)),
    }
}
