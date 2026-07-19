//! Credentialed P2 probe that retains only a fixed success marker.

use norn::provider::auth::{AuthProvider, OAuthAuthProvider, provider_account_root};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let alias = std::env::var("NORN_P2_LIVE_ACCOUNT_ALIAS")?;
    let root = provider_account_root(Some(&alias))?;
    let provider = OAuthAuthProvider::new(Some(root.clone())).await?;
    if !provider.on_unauthorized().await? {
        return Err(std::io::Error::other("OAuth refresh was unavailable").into());
    }
    drop(provider);

    let reloaded = OAuthAuthProvider::new(Some(root)).await?;
    let request = reloaded
        .apply_auth(reqwest::Client::new().get("http://127.0.0.1/"))
        .await?
        .build()?;
    let has_bearer = request
        .headers()
        .contains_key(reqwest::header::AUTHORIZATION);
    let has_account = request.headers().contains_key("chatgpt-account-id");
    if !(has_bearer && has_account) {
        return Err(std::io::Error::other("reloaded OAuth headers were incomplete").into());
    }
    println!("P2_LIVE_REFRESH_PROBE_PASS");
    Ok(())
}
