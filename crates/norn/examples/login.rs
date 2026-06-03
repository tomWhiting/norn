//! OAuth login helper — opens a browser for the `OpenAI` PKCE flow.

use norn::provider::auth::{LoginConfig, login};

#[tokio::main]
async fn main() {
    eprintln!("Opening browser for OpenAI OAuth login...");
    match login(LoginConfig::default()).await {
        Ok(()) => eprintln!("Login successful. Tokens saved to ~/.codex/auth.json"),
        Err(e) => {
            eprintln!("Login failed: {e}");
            std::process::exit(1);
        }
    }
}
