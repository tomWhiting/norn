//! Fresh executable PTYs prove frontend preference restart; helper-only tests are separate.

#[path = "support/mcp_launch_fixture.rs"]
pub mod mcp_fixture;
#[path = "../../norn-tui/tests/support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/frontend_preferences_restart.rs"]
pub mod support;

use serde_json::{Value, json};
use support::{Environment, TestResult, document, write_document};

type Scenario = (&'static str, fn() -> TestResult);

fn main() -> TestResult {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|arg| arg == mcp_fixture::FIXTURE_FLAG)
    {
        support::record_mcp_start(&arguments[1..])?;
        return mcp_fixture::run_mcp(&arguments[1..]);
    }
    let options = mcp_fixture::HarnessOptions::parse(&arguments)?;
    let scenarios: [Scenario; 5] = [
        (
            "view_shortcuts_restart_scopes_and_actual_custom_key",
            shortcut_restart,
        ),
        (
            "automatic_user_restart_and_temporary_run_preserve_original_settings",
            automatic_user_restart,
        ),
        (
            "local_restart_second_root_and_whole_object_shadowing",
            local_and_shadowing,
        ),
        (
            "malformed_owned_layers_refuse_before_raw_mode_or_mock_startup",
            malformed_layers,
        ),
        (
            "stale_owned_save_keeps_external_bytes_and_current_run_preferences",
            stale_save,
        ),
    ];
    let mut passed = 0;
    for (name, scenario) in scenarios {
        if options.selects(name) {
            if options.list {
                println!("{name}: test");
            } else {
                scenario().map_err(|error| format!("{name}: {error}"))?;
                passed += 1;
                println!("test {name} ... ok");
            }
        }
    }
    if !options.list {
        println!("Frontend preference restart result: {passed} passed");
    }
    Ok(())
}

fn original_document() -> Value {
    json!({
        "env":{"NFP_UNOWNED_SENTINEL":"preserve exact unrelated document value"},
        "tui":{
            "extension_data":{"future":{"pairing":"preserve"}},
            "extension":{"nested":["unchanged",7,null]}
        }
    })
}

fn assert_unowned(actual: &Value, original: &Value) {
    assert_eq!(actual["env"], original["env"]);
    assert_eq!(
        actual["tui"]["extension_data"],
        original["tui"]["extension_data"]
    );
    assert_eq!(actual["tui"]["extension"], original["tui"]["extension"]);
}

fn automatic_user_restart() -> TestResult {
    let mut environment = Environment::new()?;
    let original = original_document();
    write_document(&environment.user, &original)?;
    environment.session(0, true, |app| {
        let defaults = app.observe("/view status", "History demand: 20 events")?;
        assert!(defaults.contains("Body demand: 65536 original bytes"));
        assert!(defaults.contains("Thinking visible: true"));
        assert!(defaults.contains("Clipboard: Unspecified"));
        app.submit("restart fixture prompt")?;
        app.command("/view history 7")?;
        app.command("/view body 2048")?;
        app.command("/view clipboard disabled")?;
        app.command("/view detailed")?;
        app.command("/view split 3 2")?;
        app.press(b"\x05")?;
        app.press(b"\x14")?;
        let active = app.observe("/view status", "History demand: 7 events")?;
        assert!(active.contains("Body demand: 2048 original bytes"));
        assert!(active.contains("Thinking visible: false"));
        assert!(active.contains("Tool details by default: true"));
        assert!(active.contains("Clipboard: Disabled"));
        let preferences = app.observe("/view preferences status", "Preference scope: User")?;
        assert!(preferences.contains("Active preferences"));
        // The actual exit path, not a sleep or repeated status query, observes saves.
        Ok(())
    })?;
    let saved = document(&environment.user)?;
    assert_unowned(&saved, &original);
    assert_eq!(saved["tui"]["view"]["history_events"], 7);
    assert_eq!(saved["tui"]["view"]["body_bytes"], 2048);
    assert_eq!(
        saved["tui"]["view"]["split"],
        json!({"conversation":3,"changes":2})
    );
    assert_eq!(saved["tui"]["display"]["thinking_visible"], false);
    assert_eq!(saved["tui"]["input"]["submit_mode"], "queue");
    assert_eq!(
        saved["tui"]["input"]["bindings"]["pane_toggle"],
        json!(["alt+p", "f7"])
    );
    assert_eq!(environment.requests()?.len(), 1);
    let saved_bytes = std::fs::read(&environment.user)?;
    environment.session(0, true, |app| {
        let restored = app.observe("/view status", "History demand: 7 events")?;
        assert!(restored.contains("Body demand: 2048 original bytes"));
        assert!(restored.contains("Thinking visible: false"));
        assert!(restored.contains("Tool details by default: true"));
        assert!(restored.contains("Clipboard: Disabled"));
        assert!(restored.debug_text().contains("queue"));
        app.observe("/view preferences run", "Preference scope: Run")?;
        app.command("/view history 11")?;
        app.command("/view body 4096")?;
        app.observe("/view status", "History demand: 11 events")?;
        app.draft_resize("unsent original draft α🙂")?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, saved_bytes);
    environment.session(0, true, |app| {
        let restored = app.observe("/view status", "History demand: 7 events")?;
        assert!(restored.contains("Body demand: 2048 original bytes"));
        assert!(!restored.contains("unsent original draft"));
        assert!(restored.debug_text().contains("queue"));
        Ok(())
    })?;
    assert_eq!(
        environment.requests()?.len(),
        1,
        "local controls admitted provider work"
    );
    environment.assert_mcp_launches(3)?;
    environment.finish()
}

fn local_and_shadowing() -> TestResult {
    let mut environment = Environment::new()?;
    let mut original = original_document();
    original["tui"]["view"] = json!({"history_events":7,"body_bytes":2048});
    write_document(&environment.user, &original)?;
    let user_bytes = std::fs::read(&environment.user)?;
    let local = environment.local(0);
    let local_original = json!({"tui":{"extension_data":{"owner":"local"},"extension":[1,2,3]}});
    write_document(&local, &local_original)?;
    environment.session(0, true, |app| {
        // A higher tui object replaces the complete lower object, even if it
        // contains only unowned fields. Lower owned values must not leak through.
        let shadowed = app.observe("/view status", "History demand: 20 events")?;
        assert!(shadowed.contains("Body demand: 65536 original bytes"));
        let source = app.observe(
            "/view preferences status",
            "Personal save shadowed by higher layer: true",
        )?;
        assert!(source.contains("WorkspaceLocal"));
        app.observe("/view preferences run", "Preference scope: Run")?;
        app.command("/view history 9")?;
        app.command("/view body 8192")?;
        app.observe("/view preferences local", "Preference scope: Local")?;
        app.command("/view preferences save")?;
        Ok(())
    })?;
    let saved = document(&local)?;
    assert_eq!(saved["tui"]["view"]["history_events"], 9);
    assert_eq!(saved["tui"]["view"]["body_bytes"], 8192);
    assert_eq!(
        saved["tui"]["extension_data"],
        local_original["tui"]["extension_data"]
    );
    assert_eq!(
        saved["tui"]["extension"],
        local_original["tui"]["extension"]
    );
    assert_eq!(std::fs::read(&environment.user)?, user_bytes);
    environment.session(0, true, |app| {
        let active = app.observe("/view status", "History demand: 9 events")?;
        assert!(active.contains("Body demand: 8192 original bytes"));
        Ok(())
    })?;
    environment.session(1, true, |app| {
        let active = app.observe("/view status", "History demand: 7 events")?;
        assert!(active.contains("Body demand: 2048 original bytes"));
        Ok(())
    })?;
    assert!(!environment.local(1).exists());
    assert_eq!(environment.requests()?.len(), 0);
    environment.assert_mcp_launches(3)?;
    environment.finish()
}

fn malformed_layers() -> TestResult {
    for (layer, invalid, field) in [
        (
            0,
            json!({"input":{"bindings":{"pane_toggle":["alt+d"]}}}),
            "tui.input.bindings.pane_toggle",
        ),
        (
            1,
            json!({"input":{"bindings":{"pane_toggle":["ctrl+z"]}}}),
            "tui.input.bindings.pane_toggle",
        ),
        (
            2,
            json!({"input":{"bindings":{"future":[]}}}),
            "tui.input.bindings.future",
        ),
        (0, json!({"view":{"body_bytes":0}}), "tui.view.body_bytes"),
        (
            1,
            json!({"input":{"submit_mode":false}}),
            "tui.input.submit_mode",
        ),
        (
            2,
            json!({"display":{"unknown":true}}),
            "tui.display.unknown",
        ),
    ] {
        let mut environment = Environment::new()?;
        let path = match layer {
            0 => environment.user.clone(),
            1 => environment.shared(0),
            _ => environment.local(0),
        };
        write_document(&path, &json!({"tui":invalid}))?;
        if layer < 2 {
            // Malformed lower layers are diagnosed even when an empty local
            // object wins. This is the actual launch validator, not a decoder call.
            write_document(&environment.local(0), &json!({"tui":{}}))?;
        }
        let bytes = std::fs::read(&path)?;
        environment.refuse(0, field, &path)?;
        assert_eq!(std::fs::read(&path)?, bytes);
        assert!(environment.requests()?.is_empty());
        environment.assert_mcp_launches(0)?;
        environment.finish()?;
    }
    Ok(())
}

fn stale_save() -> TestResult {
    let mut environment = Environment::new()?;
    let original = original_document();
    write_document(&environment.user, &original)?;
    let mut external = original;
    external["tui"]["view"] = json!({"history_events":99});
    let user = environment.user.clone();
    environment.session(0, false, |app| {
        // Initial complete frame proves the CLI captured its launch snapshot.
        write_document(&user, &external)?;
        app.command("/view history 7")?;
        app.wait_contains("Preference save failed")?;
        let active = app.observe("/view status", "History demand: 7 events")?;
        assert!(active.contains("History demand: 7 events"));
        let failure = app.observe("/view preferences status", "Save state: failed")?;
        assert!(failure.contains("view"));
        assert!(failure.contains("Save failed before publication"));
        Ok(())
    })?;
    assert_eq!(document(&environment.user)?, external);
    assert!(environment.requests()?.is_empty());
    environment.assert_mcp_launches(1)?;
    environment.finish()
}

fn shortcut_restart() -> TestResult {
    let mut environment = Environment::new()?;
    let original = original_document();
    write_document(&environment.user, &original)?;
    environment.session(0, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+p, f7")?;
        app.observe(
            "/view keys set pane_toggle alt+q f7",
            "View shortcuts updated: pane_toggle",
        )?;
        app.observe("/view keys", "pane_toggle: alt+q, f7")?;
        let frame = app.frame(0, |_| true)?;
        app.press(b"\x1bq")?;
        let opened = app.frame(frame.end_offset, |screen| {
            screen.contains("Changes · select a tool call")
        })?;
        opened.assert_composer(1)?;
        Ok(())
    })?;
    let saved = document(&environment.user)?;
    assert_unowned(&saved, &original);
    assert_eq!(
        saved["tui"]["input"]["bindings"]["pane_toggle"],
        json!(["alt+q", "f7"])
    );
    let bytes = std::fs::read(&environment.user)?;
    environment.session(0, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+q, f7")?;
        app.observe("/view preferences run", "Preference scope: Run")?;
        app.observe(
            "/view keys set pane_toggle alt+r",
            "View shortcuts updated: pane_toggle",
        )?;
        app.observe("/view keys", "pane_toggle: alt+r")?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, bytes);
    environment.session(0, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+q, f7")?;
        Ok(())
    })?;
    let local = environment.local(0);
    write_document(
        &local,
        &json!({"tui":{"extension_data":{"preserve":"local"}}}),
    )?;
    environment.session(0, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+p, f7")?;
        app.observe("/view preferences local", "Preference scope: Local")?;
        app.observe(
            "/view keys set pane_toggle alt+b",
            "View shortcuts updated: pane_toggle",
        )?;
        Ok(())
    })?;
    let saved_local = document(&local)?;
    assert_eq!(
        saved_local["tui"]["input"]["bindings"]["pane_toggle"],
        json!(["alt+b"])
    );
    assert_eq!(saved_local["tui"]["extension_data"]["preserve"], "local");
    environment.session(0, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+b")?;
        Ok(())
    })?;
    environment.session(1, true, |app| {
        app.observe("/view keys", "pane_toggle: alt+q, f7")?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, bytes);
    assert!(
        environment.requests()?.is_empty(),
        "shortcut commands admitted provider work"
    );
    environment.assert_mcp_launches(6)?;
    environment.finish()
}
