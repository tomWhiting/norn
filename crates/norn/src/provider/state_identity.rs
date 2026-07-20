//! Opaque identities for credential-scoped provider state.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const OAUTH_PRINCIPAL_DOMAIN: &[u8] = b"norn.oauth-principal.identity.v1\0";
const API_KEY_DOMAIN: &[u8] = b"norn.api-key.identity.v1\0";
const STATIC_CODEX_PRINCIPAL_DOMAIN: &[u8] = b"norn.static-codex-principal.identity.v1\0";
const CREDENTIAL_IDENTITY_DOMAIN: &[u8] = b"norn.credential.identity.v1\0";
const PROVIDER_STATE_DOMAIN: &[u8] = b"norn.provider-state.identity.v1\0";

/// Stable opaque identity for one credential before provider authority scope is
/// applied.
///
/// This is private equality metadata, not a public or cryptographic commitment.
/// Low-entropy source material remains guessable. The type has no raw-byte
/// accessor and its debug representation never exposes the digest.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct CredentialIdentity([u8; 32]);

impl CredentialIdentity {
    /// Derives an identity from an embedder-owned authentication namespace and
    /// stable, high-entropy opaque credential identity.
    #[must_use]
    pub fn derive(authentication_namespace: &str, opaque_credential_identity: &[u8]) -> Self {
        Self(domain_separated_digest(
            CREDENTIAL_IDENTITY_DOMAIN,
            &[
                authentication_namespace.as_bytes(),
                opaque_credential_identity,
            ],
        ))
    }

    pub(crate) fn from_oauth_principal(account_id: &str, user_id: &str) -> Self {
        Self(domain_separated_digest(
            OAUTH_PRINCIPAL_DOMAIN,
            &[
                b"account",
                account_id.as_bytes(),
                b"user",
                user_id.as_bytes(),
            ],
        ))
    }

    pub(crate) fn from_api_key(api_key: &str) -> Self {
        Self(single_part_digest(API_KEY_DOMAIN, api_key.as_bytes()))
    }

    pub(crate) fn from_static_codex(access_token: &str, account_id: Option<&str>) -> Self {
        let digest = match account_id {
            Some(account_id) => domain_separated_digest(
                STATIC_CODEX_PRINCIPAL_DOMAIN,
                &[
                    b"account",
                    account_id.as_bytes(),
                    b"access-token",
                    access_token.as_bytes(),
                ],
            ),
            None => domain_separated_digest(
                STATIC_CODEX_PRINCIPAL_DOMAIN,
                &[b"account-absent", b"access-token", access_token.as_bytes()],
            ),
        };
        Self(digest)
    }

    pub(crate) fn scoped_to_openai_backend(
        self,
        backend: &str,
        normalized_endpoint: &str,
    ) -> ProviderStateIdentity {
        ProviderStateIdentity(domain_separated_digest(
            PROVIDER_STATE_DOMAIN,
            &[&self.0, backend.as_bytes(), normalized_endpoint.as_bytes()],
        ))
    }
}

impl std::fmt::Debug for CredentialIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialIdentity([REDACTED])")
    }
}

/// Stable, opaque identity for credential-scoped provider state.
///
/// The digest is private equality metadata, not a public or cryptographic
/// commitment: callers must not disclose it, and low-entropy source material
/// remains guessable. Its debug representation never exposes the digest.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProviderStateIdentity([u8; 32]);

impl ProviderStateIdentity {
    /// Derives an identity for an embedder-owned provider namespace and opaque
    /// credential identity.
    ///
    /// The namespace must distinguish the provider protocol and normalized
    /// authority. The caller owns the stability and high-entropy contracts for
    /// the opaque credential identity. Norn hashes each part independently
    /// before composition so their boundary is unambiguous.
    #[must_use]
    pub fn derive(provider_namespace: &str, opaque_credential_identity: &[u8]) -> Self {
        Self(domain_separated_digest(
            PROVIDER_STATE_DOMAIN,
            &[provider_namespace.as_bytes(), opaque_credential_identity],
        ))
    }
}

impl std::fmt::Debug for ProviderStateIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderStateIdentity([REDACTED])")
    }
}

fn single_part_digest(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(value);
    digest.finalize().into()
}

fn domain_separated_digest(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    for part in parts {
        digest.update(Sha256::digest(part));
    }
    digest.finalize().into()
}

#[cfg(test)]
#[path = "state_identity_tests.rs"]
mod tests;
