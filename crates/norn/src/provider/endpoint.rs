//! Validation for credential-bearing custom provider endpoints.

use url::{Host, Url};

use crate::error::ProviderError;

/// Validates and normalizes an HTTP(S) provider base URL.
///
/// The returned URL has no trailing slash. Rejected values are never included
/// in the error because userinfo or query data could itself be secret.
pub(crate) fn validated_credential_base_url(candidate: &str) -> Result<String, ProviderError> {
    let candidate = candidate.trim();
    let url = match Url::parse(candidate) {
        Ok(url) => url,
        Err(parse_error) => {
            tracing::debug!(
                error = %parse_error,
                "rejected malformed credential-bearing provider base URL"
            );
            return Err(invalid_base_url_error());
        }
    };
    let is_valid = transport_is_credential_safe(&url)
        && raw_authority(candidate).is_some_and(|authority| !authority.is_empty())
        && url.host_str().is_some()
        && !has_explicit_userinfo(candidate)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    if !is_valid {
        return Err(invalid_base_url_error());
    }

    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Rejects API-key destinations on the private `chatgpt.com` authority.
///
/// The terminal DNS dot is ignored deliberately: `chatgpt.com.` and
/// `chatgpt.com` identify the same DNS name. Rejecting the entire authority
/// also covers percent-encoded or otherwise ambiguous private endpoint paths.
pub(crate) fn reject_chatgpt_api_key_destination(candidate: &str) -> Result<(), ProviderError> {
    let is_chatgpt = Url::parse(candidate)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host.trim_end_matches('.')
                .eq_ignore_ascii_case("chatgpt.com")
        });
    if is_chatgpt {
        return Err(ProviderError::InvalidRequest {
            message: "ChatGPT private endpoints require OAuth authentication; API keys must use the public Responses API or another compatible authority"
                .to_owned(),
        });
    }
    Ok(())
}

fn transport_is_credential_safe(url: &Url) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(Host::Domain(host)) => host == "localhost",
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Returns whether the raw URL authority contains a userinfo delimiter.
///
/// `url::Url` represents `https://@example.com` with an empty username, so
/// checking the parsed username alone would accept an explicitly supplied
/// userinfo component.
pub(crate) fn has_explicit_userinfo(candidate: &str) -> bool {
    raw_authority(candidate).is_some_and(|authority| authority.contains('@'))
}

fn raw_authority(candidate: &str) -> Option<&str> {
    let remainder = candidate.split_once("://")?.1;
    remainder.split(['/', '?', '#']).next()
}

fn invalid_base_url_error() -> ProviderError {
    ProviderError::InvalidRequest {
        message: "credential-bearing provider base URL must use HTTPS or loopback HTTP, include an authority, and omit userinfo, query, and fragment"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_http_and_https_urls_are_normalized() -> Result<(), ProviderError> {
        for (candidate, expected) in [
            ("http://localhost:11434/v1/", "http://localhost:11434/v1"),
            ("https://api.openai.com/v1", "https://api.openai.com/v1"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("http://[::1]:8080/v1", "http://[::1]:8080/v1"),
        ] {
            assert_eq!(validated_credential_base_url(candidate)?, expected);
        }
        Ok(())
    }

    #[test]
    fn ambiguous_or_non_http_urls_are_rejected_without_echoing_values() {
        for candidate in [
            "ftp://example.com/v1",
            "http://example.com/v1",
            "http://192.168.1.10:8080/v1",
            "http://localhost.evil.example/v1",
            "https://@example.com/v1",
            "https://user:secret@example.com/v1",
            "https://example.com/v1?token=secret",
            "https://example.com/v1#secret",
            "https:///v1",
            "not a URL",
            "",
        ] {
            let result = validated_credential_base_url(candidate);
            assert!(
                matches!(result, Err(ProviderError::InvalidRequest { .. })),
                "credential-bearing endpoint should be rejected: {candidate}",
            );
            let rendered = format!("{result:?}");
            assert!(!rendered.contains("user:secret"));
            assert!(!rendered.contains("token=secret"));
        }
    }

    #[test]
    fn api_keys_reject_all_chatgpt_authority_spellings() {
        for candidate in [
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com./backend-api/codex",
            "https://chatgpt.com/backend-api/%63odex",
            "HTTPS://CHATGPT.COM/another-path",
        ] {
            assert!(reject_chatgpt_api_key_destination(candidate).is_err());
        }
        assert!(reject_chatgpt_api_key_destination("https://api.openai.com/v1").is_ok());
    }
}
