use crate::error::ConfigError;

use super::SessionManager;

impl SessionManager {
    /// Create a manager for the checked standard user session store.
    ///
    /// This resolves the trusted absolute Norn root and enforces the bounded
    /// legacy cutover guard before returning a manager. Use [`Self::new`] only
    /// when the caller deliberately owns a custom storage directory.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidConfig`] when the standard root is not
    /// trusted or legacy data exists without a complete cutover proof.
    pub fn standard() -> Result<Self, ConfigError> {
        Ok(Self::new(
            crate::config::paths::resolve_standard_session_data_dir()?,
        ))
    }
}

#[cfg(test)]
#[path = "standard_tests.rs"]
mod tests;
