//! Public named-account value types.

use super::{AccountCatalogError, DEFAULT_ACCOUNT_ALIAS};

/// Validated user-facing name for one Norn-owned OAuth account.
#[derive(Clone, Eq, PartialEq)]
pub struct AccountAlias {
    display: String,
    key: String,
}

impl AccountAlias {
    /// Validate a shell-friendly account alias.
    pub fn parse(value: &str) -> Result<Self, AccountCatalogError> {
        let mut bytes = value.bytes();
        let Some(first) = bytes.next() else {
            return Err(AccountCatalogError::InvalidAlias);
        };
        if !first.is_ascii_alphanumeric()
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(AccountCatalogError::InvalidAlias);
        }
        let key = value.to_ascii_lowercase();
        if key == DEFAULT_ACCOUNT_ALIAS {
            return Err(AccountCatalogError::ReservedAlias);
        }
        Ok(Self {
            display: value.to_owned(),
            key,
        })
    }

    /// Original spelling supplied when the alias was created.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.display
    }

    pub(super) fn key(&self) -> &str {
        &self.key
    }
}

impl std::fmt::Debug for AccountAlias {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccountAlias([REDACTED])")
    }
}

/// One account shown by `norn auth list`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountSummary {
    /// User-facing account alias.
    pub alias: String,
    /// Whether new OAuth providers select this account by default.
    pub active: bool,
    /// Whether this is the legacy `$NORN_HOME/auth/auth.json` slot.
    pub legacy_default: bool,
}
