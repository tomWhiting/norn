//! Typed preference schema acceptance without environment or filesystem mutation.

use super::*;
use serde_json::json;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn absent_fields_use_the_existing_declared_defaults() -> TestResult {
    let defaults = FrontendPreferences::decode(None)?;
    let owned = defaults.projection()?;
    assert_eq!(
        owned["view"],
        json!({"changes_open":false,"split":{"conversation":1,"changes":1},"upper_pane":"conversation","expanded_tools":false,"history_events":20,"body_bytes":65536,"clipboard":"unspecified"})
    );
    assert_eq!(
        owned["display"],
        json!({"thinking_visible":true,"secondary_fields_visible":false})
    );
    assert_eq!(owned["input"], json!({"submit_mode":"steer"}));
    assert_eq!(owned["composer"], json!({"send_key":"enter"}));
    assert_eq!(
        FrontendPreferences::decode(Some(
            &json!({"extension_data":{"future":true},"other":{"secret":"unowned"}})
        ))?,
        defaults
    );
    assert_eq!(
        FrontendPreferences::decode(Some(&json!({"composer":{}})))?,
        defaults
    );
    assert_eq!(owned.len(), 4);
    Ok(())
}

#[test]
fn complete_preferences_round_trip_without_transient_state() -> TestResult {
    let value = json!({"view":{"changes_open":true,"split":{"conversation":7,"changes":3},"upper_pane":"changes","expanded_tools":true,"history_events":51,"body_bytes":4096,"clipboard":"osc52"},"display":{"thinking_visible":false,"secondary_fields_visible":true},"input":{"submit_mode":"queue"},"composer":{"send_key":"alt-enter"},"extension_data":{"send":"future"}});
    let preferences = FrontendPreferences::decode(Some(&value))?;
    let owned = preferences.projection()?;
    assert_eq!(owned["view"], value["view"]);
    assert_eq!(owned["display"], value["display"]);
    assert_eq!(owned["input"], value["input"]);
    assert_eq!(owned["composer"], value["composer"]);
    assert!(!owned.contains_key("extension_data"));
    assert_eq!(
        FrontendPreferences::decode(Some(&Value::Object(owned)))?,
        preferences
    );
    Ok(())
}

#[test]
fn invalid_owned_fields_are_refused_by_exact_dotted_path() {
    for (value, path) in [
        (json!({"view":{"unknown":true}}), "tui.view.unknown"),
        (
            json!({"view":{"changes_open":"true"}}),
            "tui.view.changes_open",
        ),
        (
            json!({"view":{"history_events":0}}),
            "tui.view.history_events",
        ),
        (json!({"view":{"body_bytes":-1}}), "tui.view.body_bytes"),
        (json!({"view":{"body_bytes":1.5}}), "tui.view.body_bytes"),
        (
            json!({"view":{"split":{"conversation":65536}}}),
            "tui.view.split.conversation",
        ),
        (
            json!({"view":{"split":{"changes":0}}}),
            "tui.view.split.changes",
        ),
        (json!({"view":{"upper_pane":"work"}}), "tui.view.upper_pane"),
        (json!({"view":{"clipboard":"auto"}}), "tui.view.clipboard"),
        (
            json!({"display":{"thinking_visible":null}}),
            "tui.display.thinking_visible",
        ),
        (
            json!({"input":{"submit_mode":"send"}}),
            "tui.input.submit_mode",
        ),
        (json!({"input":{"send_key":"enter"}}), "tui.input.send_key"),
        (
            json!({"composer":{"send_key":"control-enter"}}),
            "tui.composer.send_key",
        ),
        (
            json!({"composer":{"send_key":false}}),
            "tui.composer.send_key",
        ),
        (
            json!({"composer":{"send_key":null}}),
            "tui.composer.send_key",
        ),
        (json!({"composer":{"future":true}}), "tui.composer.future"),
        (
            json!({"composer":{"submit_mode":"queue"}}),
            "tui.composer.submit_mode",
        ),
    ] {
        let error = FrontendPreferences::decode(Some(&value)).err();
        assert!(
            error.is_some_and(|error| error.to_string().contains(path)),
            "expected {path}"
        );
    }
}

#[test]
fn present_nonobjects_are_not_silently_defaults() {
    for value in [
        Value::Null,
        json!([]),
        json!({"view":null}),
        json!({"display":false}),
        json!({"input":[]}),
        json!({"composer":null}),
        json!({"composer":[]}),
    ] {
        assert!(FrontendPreferences::decode(Some(&value)).is_err());
    }
}

#[test]
fn send_key_labels_and_cycle_preserve_the_three_declared_policies() -> TestResult {
    for (policy, label, other) in [
        (ComposerSendKey::Enter, "enter", ComposerSendKey::ShiftEnter),
        (
            ComposerSendKey::ShiftEnter,
            "shift-enter",
            ComposerSendKey::AltEnter,
        ),
        (
            ComposerSendKey::AltEnter,
            "alt-enter",
            ComposerSendKey::Enter,
        ),
    ] {
        assert_eq!(policy.label(), label);
        assert_eq!(policy.next_policy(), other);
        assert_eq!(policy.next_policy().next_policy().next_policy(), policy);
        let decoded = FrontendPreferences::decode(Some(&json!({"composer":{"send_key":label}})))?;
        assert_eq!(decoded.composer_send_key, policy);
        assert_eq!(decoded.submit_mode, InFlightSubmitMode::Steer);
        assert_eq!(decoded.projection()?["composer"], json!({"send_key":label}));
    }
    Ok(())
}
