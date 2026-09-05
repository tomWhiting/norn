//! Inline channel settings reject ambiguous objects without printing their contents.

use std::error::Error;

use norn::config::ChannelPolicySetting;

use crate::config::ConfigOverrides;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn parses_partial_channels_and_redacts_debug() -> TestResult {
    let overrides = ConfigOverrides::parse(&[
        r#"channels={"default_policy":"wake","sources":{"private-name":"off"},"max_retained_messages":12}"#.to_owned(),
    ])?;
    let channels = overrides.channels.as_ref().ok_or("channels missing")?;
    assert_eq!(channels.default_policy, Some(ChannelPolicySetting::Wake));
    assert_eq!(
        channels
            .max_retained_messages
            .map(std::num::NonZeroUsize::get),
        Some(12)
    );
    assert!(!format!("{overrides:?}").contains("private-name"));
    Ok(())
}

#[test]
fn refuses_duplicate_unknown_or_invalid_objects_without_values() -> TestResult {
    for json in [
        r#"{"default_policy":null,"default_policy":"wake"}"#,
        r#"{"sources":{"private-name":"off","private-name":"wake"}}"#,
        r#"{"secret":"do-not-display"}"#,
        r#"{"default_policy":"do-not-display"}"#,
        r#"{"max_retained_messages":0}"#,
        r#"{"max_retained_bytes":0}"#,
        r#"{"overflow":"drop-oldest"}"#,
        "[]",
        "null",
        "{} {}",
    ] {
        let error = ConfigOverrides::parse(&[format!("channels={json}")])
            .err()
            .ok_or("invalid channels accepted")?;
        let message = error.to_string();
        assert!(message.contains("-c channels"), "{message}");
        assert!(!message.contains("do-not-display"), "{message}");
        assert!(!message.contains("private-name"), "{message}");
    }
    let error = ConfigOverrides::parse(&["channels={}".to_owned(), "channels={}".to_owned()])
        .err()
        .ok_or("repeated channels accepted")?;
    assert!(error.to_string().contains("specified more than once"));
    Ok(())
}
