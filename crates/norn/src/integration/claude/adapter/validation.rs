use crate::error::ProviderError;
use crate::provider::request::ProviderRequest;

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
