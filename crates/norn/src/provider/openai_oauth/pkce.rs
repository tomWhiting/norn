//! PKCE S256 verifier/challenge generation.

use base64::Engine as _;
use rand::RngCore as _;
use sha2::Digest as _;

/// PKCE verifier and S256 challenge.
#[derive(Clone, Debug)]
pub struct PkcePair {
    /// Base64url-no-pad verifier derived from 64 random bytes.
    pub verifier: String,
    /// Base64url-no-pad SHA-256 challenge over the verifier string.
    pub challenge: String,
}

/// Generates a PKCE verifier/challenge pair.
#[must_use]
pub fn generate() -> PkcePair {
    let mut bytes = [0_u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    PkcePair {
        verifier,
        challenge,
    }
}
