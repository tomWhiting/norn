use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};

use super::*;
use crate::provider::openai_oauth::{AuthDotJson, ChatGptTokens, IdTokenInfo};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn auth(account_id: &str) -> AuthDotJson {
    AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing(account_id),
        access_token: "access-token".to_owned(),
        refresh_token: "refresh-token".to_owned(),
        account_id: Some(account_id.to_owned()),
        additional_fields: BTreeMap::new(),
    })
}

#[test]
fn legacy_account_id_hash_and_serde_shape_are_unchanged() -> TestResult {
    let account_id = "account-identity-sentinel";
    let mut legacy = Sha256::new();
    legacy.update(b"norn.named-account.identity.v1\0");
    legacy.update(account_id.as_bytes());
    let expected: [u8; 32] = legacy.finalize().into();
    let current = AccountIdentityFingerprint::from_auth(&auth(account_id))
        .ok_or_else(|| std::io::Error::other("account identity was absent"))?;

    assert_eq!(current, AccountIdentityFingerprint(expected));
    assert_eq!(
        serde_json::to_value(current)?,
        serde_json::to_value(expected)?
    );
    assert_eq!(
        format!("{current:?}"),
        "AccountIdentityFingerprint([REDACTED])"
    );
    Ok(())
}
