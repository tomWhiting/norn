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
    assert_eq!(
        FrontendPreferences::decode(Some(
            &json!({"composer":{"future":true},"other":{"secret":"unowned"}})
        ))?,
        defaults
    );
    assert_eq!(owned.len(), 3);
    Ok(())
}

#[test]
fn complete_preferences_round_trip_without_transient_state() -> TestResult {
    let value = json!({"view":{"changes_open":true,"split":{"conversation":7,"changes":3},"upper_pane":"changes","expanded_tools":true,"history_events":51,"body_bytes":4096,"clipboard":"osc52"},"display":{"thinking_visible":false,"secondary_fields_visible":true},"input":{"submit_mode":"queue"},"composer":{"send":"future"}});
    let preferences = FrontendPreferences::decode(Some(&value))?;
    let owned = preferences.projection()?;
    assert_eq!(owned["view"], value["view"]);
    assert_eq!(owned["display"], value["display"]);
    assert_eq!(owned["input"], value["input"]);
    assert!(!owned.contains_key("composer"));
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
    ] {
        assert!(FrontendPreferences::decode(Some(&value)).is_err());
    }
}
