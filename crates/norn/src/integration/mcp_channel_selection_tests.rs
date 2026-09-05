//! Immutable default negotiation and explicit named source validation.

use super::*;
use crate::config::{McpConfigState, McpServerSettings};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn settings(
    default: McpChannelSourcePolicy,
    named: BTreeMap<String, McpChannelSourcePolicy>,
) -> Result<McpChannelSettings, Box<dyn std::error::Error + Send + Sync>> {
    Ok(McpChannelSettings::new(
        McpChannelLimits::new(4, 4096)?,
        default,
        named,
        McpChannelOverflow::RejectNew,
    )?)
}

#[test]
fn default_negotiates_only_stdio_and_named_overrides_remain_strict() -> TestResult {
    let policy = settings(
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
        BTreeMap::from([
            ("quiet".to_owned(), McpChannelSourcePolicy::Off),
            (
                "explicit".to_owned(),
                McpChannelSourcePolicy::Delivery(McpChannelPolicy::NextTurn),
            ),
        ]),
    )?;
    assert_eq!(
        policy.selection("ordinary", true),
        McpChannelSelection::IfAdvertised(McpChannelPolicy::Wake)
    );
    assert_eq!(
        policy.selection("ordinary", false),
        McpChannelSelection::Off
    );
    for stdio in [true, false] {
        assert_eq!(policy.selection("quiet", stdio), McpChannelSelection::Off);
        assert_eq!(
            policy.selection("explicit", stdio),
            McpChannelSelection::Required(McpChannelPolicy::NextTurn)
        );
    }
    assert_eq!(
        policy.default_policy(),
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake)
    );
    assert_eq!(policy.clone(), policy);
    Ok(())
}

#[test]
fn empty_named_map_supports_declared_defaults_and_global_off() -> TestResult {
    for default in [
        McpChannelSourcePolicy::Off,
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
    ] {
        let policy = settings(default, BTreeMap::new())?;
        assert_eq!(policy.default_policy(), default);
        assert!(policy.sources().is_empty());
    }
    assert!(
        settings(
            McpChannelSourcePolicy::Off,
            BTreeMap::from([(" ".to_owned(), McpChannelSourcePolicy::Off)])
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn off_can_exclude_disabled_and_http_sources_but_not_a_misspelled_name() -> TestResult {
    let directory = tempfile::tempdir()?;
    let state = McpConfigState::from_layers(
        directory.path().to_path_buf(),
        std::array::from_fn(|_| BTreeMap::new()),
        BTreeMap::from([
            (
                "disabled".to_owned(),
                McpServerSettings {
                    enabled: Some(false),
                    ..McpServerSettings::default()
                },
            ),
            (
                "remote".to_owned(),
                McpServerSettings {
                    transport: Some("http".to_owned()),
                    url: Some("https://example.test/mcp".to_owned()),
                    ..McpServerSettings::default()
                },
            ),
        ]),
    )?;
    let snapshot = state.snapshot()?;
    settings(
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
        BTreeMap::new(),
    )?
    .validate_startup(&snapshot)?;
    for name in ["disabled", "remote"] {
        let off = settings(
            McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
            BTreeMap::from([(name.to_owned(), McpChannelSourcePolicy::Off)]),
        )?;
        off.validate_startup(&snapshot)?;
        let required = settings(
            McpChannelSourcePolicy::Off,
            BTreeMap::from([(
                name.to_owned(),
                McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
            )]),
        )?;
        let error = required
            .validate_startup(&snapshot)
            .err()
            .ok_or("invalid named delivery accepted")?;
        assert!(error.to_string().contains(name));
    }
    for policy in [
        McpChannelSourcePolicy::Off,
        McpChannelSourcePolicy::Delivery(McpChannelPolicy::Wake),
    ] {
        let error = settings(
            McpChannelSourcePolicy::Off,
            BTreeMap::from([("typo".to_owned(), policy)]),
        )?
        .validate_startup(&snapshot)
        .err()
        .ok_or("unknown source accepted")?;
        assert!(error.to_string().contains("typo"));
    }
    Ok(())
}
