//! Reclaimable in-process ownership of file-backed credential coordinators.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Weak};

use tokio::sync::Mutex;

use super::{AccountIdentity, AuthManager, AuthManagerBuildError};
use crate::provider::openai_oauth::{AuthCredentialsStoreMode, NornAuthRoot, OAuthHttpOptions};

#[derive(Clone, Eq, Hash, PartialEq)]
struct ManagerIdentity {
    root: NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    token_url: String,
    account: Option<AccountIdentity>,
}

struct RegistryEntry {
    manager: Weak<AuthManager>,
    http: OAuthHttpOptions,
}

static MANAGERS: LazyLock<Mutex<HashMap<ManagerIdentity, RegistryEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) async fn shared(
    root: NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    token_url: String,
    http: OAuthHttpOptions,
) -> Result<Arc<AuthManager>, AuthManagerBuildError> {
    // Inspect and construct outside the registry lock. The immutable account
    // pin is part of the identity, so a replacement login gets a new owner.
    let candidate = AuthManager::construct_file(root.clone(), token_url.clone(), http).await?;
    let identity = ManagerIdentity {
        root,
        mode,
        token_url,
        account: candidate.account_identity.clone(),
    };
    let mut managers = MANAGERS.lock().await;
    managers.retain(|_, entry| entry.manager.strong_count() > 0);
    if let Some(entry) = managers.get(&identity)
        && let Some(manager) = entry.manager.upgrade()
    {
        if entry.http != http {
            return Err(AuthManagerBuildError::ConfigurationConflict);
        }
        return Ok(manager);
    }

    managers.insert(
        identity,
        RegistryEntry {
            manager: Arc::downgrade(&candidate),
            http,
        },
    );
    Ok(candidate)
}
