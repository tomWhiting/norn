//! Typed settings-layer and overlay contracts for persisted channel selection.

use std::collections::BTreeMap;
use std::error::Error;
use std::num::NonZeroUsize;

use crate::config::loader::parse_settings_with_unknown_paths;
use crate::config::{NornSettings, merge_settings};

use super::{ChannelOverflowSetting, ChannelPolicySetting, ChannelSettings};

fn layer(document: &str) -> Result<NornSettings, serde_json::Error> {
    let (settings, unknown) = parse_settings_with_unknown_paths(document)?;
    assert!(unknown.is_empty());
    Ok(settings)
}

#[test]
fn channels_is_known_and_root_unknown_behavior_is_preserved() -> Result<(), Box<dyn Error>> {
    let (settings, unknown) = parse_settings_with_unknown_paths(
        r#"{
            "channels": {"default_policy":"wake", "sources":{"game":"off"}},
            "unrecognized_root": {"value":true}
        }"#,
    )?;
    assert_eq!(unknown, ["unrecognized_root"]);
    let channels = settings.channels.ok_or("channels section missing")?;
    assert_eq!(channels.default_policy, Some(ChannelPolicySetting::Wake));
    assert_eq!(
        channels
            .sources
            .as_ref()
            .and_then(|sources| sources.get("game")),
        Some(&ChannelPolicySetting::Off),
    );
    assert!(channels.max_retained_messages.is_none());
    assert!(channels.max_retained_bytes.is_none());
    assert!(channels.overflow.is_none());
    Ok(())
}

#[test]
fn actual_four_layer_merge_preserves_field_and_named_precedence() -> Result<(), Box<dyn Error>> {
    let mut user = layer(
        r#"{"channels":{
        "default_policy":"wake",
        "sources":{"shared":"wake","untouched":"next-turn"},
        "max_retained_messages":17,"max_retained_bytes":1000,"overflow":"reject-new"
    }}"#,
    )?;
    let mut project = layer(
        r#"{"channels":{
        "default_policy":"next-turn",
        "sources":{"shared":"off","project":"wake"},"max_retained_messages":23
    }}"#,
    )?;
    let mut local = layer(
        r#"{"channels":{
        "default_policy":"off",
        "sources":{"project":"off","local":"wake"},"max_retained_bytes":222
    }}"#,
    )?;
    let mut cli = layer(
        r#"{"channels":{
        "sources":{"shared":"wake","local":"off"},"max_retained_messages":31
    }}"#,
    )?;
    let result = merge_settings(&mut user, &mut project, &mut local, &mut cli);
    let channels = result.channels.ok_or("merged channels section missing")?;
    assert_eq!(channels.default_policy, Some(ChannelPolicySetting::Off));
    assert_eq!(channels.max_retained_messages, NonZeroUsize::new(31));
    assert_eq!(channels.max_retained_bytes, NonZeroUsize::new(222));
    assert_eq!(channels.overflow, Some(ChannelOverflowSetting::RejectNew));
    assert_eq!(
        channels.sources,
        Some(BTreeMap::from([
            ("shared".to_owned(), ChannelPolicySetting::Wake),
            ("untouched".to_owned(), ChannelPolicySetting::NextTurn),
            ("project".to_owned(), ChannelPolicySetting::Off),
            ("local".to_owned(), ChannelPolicySetting::Off),
        ]))
    );
    assert!(user.channels.is_none());
    assert!(project.channels.is_none());
    assert!(local.channels.is_none());
    assert!(cli.channels.is_none());
    Ok(())
}

#[test]
fn higher_partial_settings_complete_lower_policy_without_defaults() -> Result<(), Box<dyn Error>> {
    let mut lower: ChannelSettings =
        serde_json::from_str(r#"{"sources":{"game":"wake"},"max_retained_messages":9}"#)?;
    assert!(lower.default_policy.is_none());
    assert!(lower.max_retained_bytes.is_none());
    assert!(lower.overflow.is_none());
    lower.overlay(serde_json::from_str(
        r#"{"max_retained_bytes":456,"overflow":"reject-new"}"#,
    )?);
    assert_eq!(lower.max_retained_messages, NonZeroUsize::new(9));
    assert_eq!(lower.max_retained_bytes, NonZeroUsize::new(456));
    assert_eq!(lower.overflow, Some(ChannelOverflowSetting::RejectNew));
    assert!(lower.default_policy.is_none());
    assert_eq!(
        lower
            .sources
            .as_ref()
            .and_then(|sources| sources.get("game")),
        Some(&ChannelPolicySetting::Wake),
    );
    Ok(())
}

#[test]
fn null_fields_and_empty_higher_source_map_do_not_clear_lower_values() -> Result<(), Box<dyn Error>>
{
    let mut lower: ChannelSettings = serde_json::from_str(
        r#"{
        "default_policy":"next-turn","sources":{"game":"off"},
        "max_retained_messages":3,"max_retained_bytes":123,"overflow":"reject-new"
    }"#,
    )?;
    let expected = lower.clone();
    lower.overlay(serde_json::from_str(
        r#"{
        "default_policy":null,"sources":{},"max_retained_messages":null,
        "max_retained_bytes":null,"overflow":null
    }"#,
    )?);
    assert_eq!(lower, expected);
    lower.overlay(serde_json::from_str(r#"{"sources":null}"#)?);
    assert_eq!(lower, expected);
    lower.overlay(ChannelSettings::default());
    assert_eq!(lower, expected);
    Ok(())
}

#[test]
fn absent_sections_stay_absent_and_an_explicit_empty_section_stays_partial()
-> Result<(), Box<dyn Error>> {
    let mut user = layer("{}")?;
    let mut project = layer(r#"{"channels":null}"#)?;
    let mut local = NornSettings::default();
    let mut cli = NornSettings::default();
    assert!(
        merge_settings(&mut user, &mut project, &mut local, &mut cli)
            .channels
            .is_none()
    );
    project = layer(r#"{"channels":{}}"#)?;
    assert_eq!(
        merge_settings(&mut user, &mut project, &mut local, &mut cli).channels,
        Some(ChannelSettings::default()),
    );
    let mut settings = ChannelSettings::default();
    settings.overlay(serde_json::from_str(r#"{"sources":{}}"#)?);
    assert_eq!(settings.sources, Some(BTreeMap::new()));
    Ok(())
}

#[test]
fn strict_fields_and_source_keys_refuse_duplicates_before_merging() -> Result<(), Box<dyn Error>> {
    for fields in [
        r#""default_policy":null,"default_policy":"wake""#,
        r#""sources":null,"sources":{}"#,
        r#""max_retained_messages":null,"max_retained_messages":1"#,
        r#""max_retained_bytes":null,"max_retained_bytes":1"#,
        r#""overflow":null,"overflow":"reject-new""#,
        r#""sources":{"game":"wake","game":"off"}"#,
        r#""sources":{"game":"wake","\u0067ame":"off"}"#,
    ] {
        let document = format!(r#"{{"channels":{{{fields}}}}}"#);
        let Err(error) = layer(&document) else {
            return Err("duplicate channel setting was accepted".into());
        };
        assert!(error.to_string().contains("duplicate channels"));
    }
    assert!(layer(r#"{"channels":{"unknown":"SECRET"}}"#).is_err());
    assert!(layer(r#"{"channels":{},"channels":{}}"#).is_err());
    Ok(())
}

#[test]
fn channel_objects_and_policy_spellings_are_strict() {
    for document in [
        r#"{"channels":[]}"#,
        r#"{"channels":[null,null,null,null,null]}"#,
        r#"{"channels":{"sources":[]}}"#,
        r#"{"channels":{"sources":{"game":null}}}"#,
        r#"{"channels":{"default_policy":"hold"}}"#,
        r#"{"channels":{"default_policy":"next_turn"}}"#,
        r#"{"channels":{"default_policy":"Wake"}}"#,
        r#"{"channels":{"sources":{"game":"hold"}}}"#,
        r#"{"channels":{"overflow":"drop-oldest"}}"#,
        r#"{"channels":{"overflow":"RejectNew"}}"#,
    ] {
        assert!(layer(document).is_err());
    }
}

#[test]
fn caps_are_positive_integers_and_have_no_additional_product_ceiling() -> Result<(), Box<dyn Error>>
{
    for field in ["max_retained_messages", "max_retained_bytes"] {
        for value in ["0", "-1", "1.5", "true", r#""SECRET""#] {
            let document = format!(r#"{{"channels":{{"{field}":{value}}}}}"#);
            let Err(error) = layer(&document) else {
                return Err("invalid channel limit was accepted".into());
            };
            assert!(error.to_string().contains(field));
            assert!(!error.to_string().contains("SECRET"));
        }
        let too_large = format!(r#"{{"channels":{{"{field}":{}0}}}}"#, usize::MAX);
        assert!(layer(&too_large).is_err());
    }
    let maximum = format!(
        r#"{{"channels":{{"max_retained_messages":{},"max_retained_bytes":{}}}}}"#,
        usize::MAX,
        usize::MAX
    );
    let channels = layer(&maximum)?.channels.ok_or("maximum limits missing")?;
    assert_eq!(
        channels.max_retained_messages,
        NonZeroUsize::new(usize::MAX)
    );
    assert_eq!(channels.max_retained_bytes, NonZeroUsize::new(usize::MAX));
    Ok(())
}

#[test]
fn serialization_roundtrips_the_public_wire_names() -> Result<(), Box<dyn Error>> {
    let channels: ChannelSettings = serde_json::from_str(
        r#"{
        "default_policy":"next-turn","sources":{"game":"off","other":"wake"},
        "max_retained_messages":2,"max_retained_bytes":512,"overflow":"reject-new"
    }"#,
    )?;
    let settings = NornSettings {
        channels: Some(channels.clone()),
        ..NornSettings::default()
    };
    let encoded = serde_json::to_string(&settings)?;
    assert!(encoded.contains(r#""default_policy":"next-turn""#));
    assert!(encoded.contains(r#""overflow":"reject-new""#));
    assert_eq!(layer(&encoded)?.channels, Some(channels));
    assert_eq!(serde_json::to_string(&ChannelSettings::default())?, "{}");
    Ok(())
}

#[test]
fn debug_withholds_raw_source_keys_and_invalid_policy_errors_withhold_values()
-> Result<(), Box<dyn Error>> {
    let settings = layer(r#"{"channels":{"sources":{"{\"token\":\"SECRET\"}":"wake"}}}"#)?;
    let debug = format!("{settings:?}");
    assert!(!debug.contains("SECRET"));
    assert!(debug.contains("source_entries: Some(1)"));
    for document in [
        r#"{"channels":{"default_policy":"SECRET"}}"#,
        r#"{"channels":{"sources":{"game":"SECRET"}}}"#,
        r#"{"channels":{"overflow":"SECRET"}}"#,
        r#"{"channels":{"SECRET":true}}"#,
    ] {
        let Err(error) = layer(document) else {
            return Err("invalid channel scalar was accepted".into());
        };
        assert!(!error.to_string().contains("SECRET"));
    }
    Ok(())
}
