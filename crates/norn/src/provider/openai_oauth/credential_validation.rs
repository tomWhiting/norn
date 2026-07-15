//! Non-disclosing validation for OAuth credential fields.

/// Credential field being validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CredentialField {
    /// Bearer token sent in the `Authorization` header.
    AccessToken,
    /// Refresh token sent to the token authority.
    RefreshToken,
    /// Account identifier sent in the `chatgpt-account-id` header.
    AccountId,
}

impl std::fmt::Display for CredentialField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::AccessToken => "access token",
            Self::RefreshToken => "refresh token",
            Self::AccountId => "account identifier",
        };
        formatter.write_str(name)
    }
}

/// Structural defect in a credential field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CredentialValueProblem {
    /// The field contained no bytes.
    Empty,
    /// Leading or trailing whitespace would change the credential on trimming.
    SurroundingWhitespace,
    /// The field contained an HTTP-unsafe control character.
    ControlCharacter,
    /// The field cannot be represented by the HTTP header implementation.
    InvalidHeaderValue,
}

impl std::fmt::Display for CredentialValueProblem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = match self {
            Self::Empty => "is empty",
            Self::SurroundingWhitespace => "contains surrounding whitespace",
            Self::ControlCharacter => "contains a control character",
            Self::InvalidHeaderValue => "is not a valid HTTP header value",
        };
        formatter.write_str(description)
    }
}

/// Non-disclosing credential-field validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OAuth {field} {problem}")]
pub(super) struct CredentialValueError {
    field: CredentialField,
    problem: CredentialValueProblem,
}

impl CredentialValueError {
    /// Returns the affected field without exposing its value.
    #[must_use]
    pub(super) fn field(self) -> CredentialField {
        self.field
    }
}

/// Validates a credential field before it reaches storage or an HTTP request.
///
/// # Errors
///
/// Returns [`CredentialValueError`] when the field is empty, contains unsafe
/// whitespace or controls, or cannot be represented as a required header.
pub(super) fn validate_credential_value(
    field: CredentialField,
    value: &str,
) -> Result<(), CredentialValueError> {
    let problem = if value.is_empty() {
        Some(CredentialValueProblem::Empty)
    } else if value.trim() != value {
        Some(CredentialValueProblem::SurroundingWhitespace)
    } else if value.chars().any(char::is_control) {
        Some(CredentialValueProblem::ControlCharacter)
    } else if matches!(
        field,
        CredentialField::AccessToken | CredentialField::AccountId
    ) && (!value.is_ascii() || reqwest::header::HeaderValue::try_from(value).is_err())
    {
        Some(CredentialValueProblem::InvalidHeaderValue)
    } else {
        None
    };

    problem.map_or(Ok(()), |problem| {
        Err(CredentialValueError { field, problem })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_values_are_rejected_without_disclosure() -> Result<(), std::io::Error> {
        for (value, problem) in [
            ("", CredentialValueProblem::Empty),
            (" secret", CredentialValueProblem::SurroundingWhitespace),
            ("secret ", CredentialValueProblem::SurroundingWhitespace),
            ("sec\rret", CredentialValueProblem::ControlCharacter),
            ("sec\nret", CredentialValueProblem::ControlCharacter),
            ("sec\0ret", CredentialValueProblem::ControlCharacter),
            ("sec\u{7f}ret", CredentialValueProblem::ControlCharacter),
            (
                "secret-\u{1f600}",
                CredentialValueProblem::InvalidHeaderValue,
            ),
        ] {
            let result = validate_credential_value(CredentialField::AccessToken, value);
            let Err(error) = result else {
                return Err(std::io::Error::other(
                    "unsafe credential value unexpectedly passed validation",
                ));
            };
            let rendered = error.to_string();

            assert_eq!(error.field, CredentialField::AccessToken);
            assert_eq!(error.problem, problem);
            if !value.is_empty() {
                assert!(!rendered.contains(value));
            }
        }
        Ok(())
    }

    #[test]
    fn visible_internal_separators_are_not_rewritten() {
        assert!(validate_credential_value(CredentialField::AccountId, "account visible").is_ok());
    }
}
