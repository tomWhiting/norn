use std::path::PathBuf;
use std::time::Duration;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn login_config_default_is_browser_pkce() {
    let config = LoginConfig::default();
    assert!(config.auth_root.is_none());
    assert!(!config.device_code);
    assert!(config.device_code_timeout.is_none());
}

#[tokio::test]
async fn device_login_requires_an_explicit_prompt_presenter() -> TestResult {
    let result = login(LoginConfig {
        auth_root: Some(PathBuf::from("/tmp/norn-auth-test-nx")),
        device_code: true,
        ..LoginConfig::default()
    })
    .await;
    let Err(NornError::Config(ConfigError::InvalidConfig { reason })) = result else {
        return Err(std::io::Error::other("device login did not return a config error").into());
    };
    assert!(reason.contains("device-code"));
    Ok(())
}

#[test]
fn login_config_exposes_device_authorization_deadline() {
    let config = LoginConfig::default().with_device_code_timeout(Duration::from_secs(90));

    assert_eq!(config.device_code_timeout, Some(Duration::from_secs(90)));
}

#[tokio::test]
async fn zero_device_authorization_deadline_fails_before_auth_root_access() -> TestResult {
    let result = login(LoginConfig {
        auth_root: Some(PathBuf::from("relative-auth-root")),
        device_code: true,
        device_code_timeout: Some(Duration::ZERO),
        ..LoginConfig::default()
    })
    .await;
    let Err(NornError::Config(ConfigError::InvalidConfig { reason })) = result else {
        return Err(std::io::Error::other("zero device deadline did not fail as config").into());
    };

    assert!(reason.contains("non-zero authorization deadline"));
    assert!(!reason.contains("absolute"));
    Ok(())
}

#[tokio::test]
async fn login_rejects_relative_auth_root_before_starting_browser_flow() -> TestResult {
    let result = login(LoginConfig {
        auth_root: Some(PathBuf::from("relative-auth-root")),
        device_code: false,
        ..LoginConfig::default()
    })
    .await;
    let Err(NornError::Config(ConfigError::InvalidConfig { reason })) = result else {
        return Err(std::io::Error::other(
            "relative login root did not fail at the typed auth boundary",
        )
        .into());
    };
    assert!(reason.contains("must be absolute"));
    Ok(())
}
