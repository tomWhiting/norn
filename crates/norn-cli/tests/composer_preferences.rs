//! Fresh actual CLI processes prove physical send policy, persistence and startup refusal.

#[path = "support/mcp_launch_fixture.rs"]
pub mod mcp_fixture;
#[path = "support/frontend_preferences_restart.rs"]
pub mod restart_support;
#[path = "../../norn-tui/tests/support/retained_screen.rs"]
pub mod retained_screen;
#[path = "support/composer_preferences.rs"]
pub mod support;

use restart_support::{Environment, TestResult, document, write_document};
use serde_json::{Value, json};
use support::{SendKey, assert_requests, observe, submit_multiline};

type Scenario = (&'static str, fn() -> TestResult);

fn main() -> TestResult {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .first()
        .is_some_and(|arg| arg == mcp_fixture::FIXTURE_FLAG)
    {
        restart_support::record_mcp_start(&arguments[1..])?;
        return mcp_fixture::run_mcp(&arguments[1..]);
    }
    let options = mcp_fixture::HarnessOptions::parse(&arguments)?;
    let scenarios: [Scenario; 4] = [
        (
            "composer_user_send_key_persists_and_controls_fresh_processes",
            user_restart,
        ),
        (
            "composer_run_override_is_temporary_and_keeps_saved_bytes",
            temporary_run,
        ),
        (
            "composer_local_shadowing_is_root_specific_and_keeps_unowned_fields",
            local_shadowing,
        ),
        (
            "malformed_composer_layers_refuse_before_terminal_mcp_or_provider",
            malformed_layers,
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
        println!("Composer preference restart result: {passed} passed");
    }
    Ok(())
}

fn settings(send_key: &str) -> Value {
    json!({
        "env":{"NCP_UNOWNED_SENTINEL":"preserve unrelated settings"},
        "tui":{
            "composer":{"send_key":send_key},
            "extension_data":{"nested":["preserve",null,7]},
            "extension":{"owner":"outside composer"}
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

fn user_restart() -> TestResult {
    let mut environment = Environment::new()?;
    let original = settings("enter");
    write_document(&environment.user, &original)?;
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::Enter,
            "/view status",
            "Composer send key: enter",
        )?;
        submit_multiline(app, SendKey::Enter, "user Enter first α", "second 🙂")?;
        observe(
            app,
            SendKey::Enter,
            "/view composer send-key shift-enter",
            "Composer send key: shift-enter",
        )?;
        observe(
            app,
            SendKey::ShiftEnter,
            "/view preferences status",
            "Preference scope: User",
        )?;
        Ok(())
    })?;
    let shift_saved = document(&environment.user)?;
    assert_eq!(
        shift_saved["tui"]["composer"],
        json!({"send_key":"shift-enter"})
    );
    assert_unowned(&shift_saved, &original);
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::ShiftEnter,
            "/view status",
            "Composer send key: shift-enter",
        )?;
        submit_multiline(
            app,
            SendKey::ShiftEnter,
            "user ShiftEnter first γ",
            "second 界",
        )?;
        observe(
            app,
            SendKey::ShiftEnter,
            "/view composer send-key alt-enter",
            "Composer send key: alt-enter",
        )?;
        Ok(())
    })?;
    let alt_saved = document(&environment.user)?;
    assert_eq!(
        alt_saved["tui"]["composer"],
        json!({"send_key":"alt-enter"})
    );
    assert_unowned(&alt_saved, &original);
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::AltEnter,
            "/view status",
            "Composer send key: alt-enter",
        )?;
        submit_multiline(app, SendKey::AltEnter, "user AltEnter first β", "second 👩‍💻")?;
        observe(
            app,
            SendKey::AltEnter,
            "/view composer send-key enter",
            "Composer send key: enter",
        )?;
        Ok(())
    })?;
    let enter_saved = document(&environment.user)?;
    assert_eq!(enter_saved["tui"]["composer"], json!({"send_key":"enter"}));
    assert_unowned(&enter_saved, &original);
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::Enter,
            "/view status",
            "Composer send key: enter",
        )?;
        Ok(())
    })?;
    assert_requests(
        &environment.requests()?,
        &[
            "user Enter first α\nsecond 🙂",
            "user ShiftEnter first γ\nsecond 界",
            "user AltEnter first β\nsecond 👩‍💻",
        ],
    )?;
    environment.assert_mcp_launches(4)?;
    environment.finish()
}

fn temporary_run() -> TestResult {
    let mut environment = Environment::new()?;
    write_document(&environment.user, &settings("alt-enter"))?;
    let bytes = std::fs::read(&environment.user)?;
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::AltEnter,
            "/view preferences run",
            "Preference scope: Run",
        )?;
        observe(
            app,
            SendKey::AltEnter,
            "/view composer send-key shift-enter",
            "Composer send key: shift-enter",
        )?;
        observe(
            app,
            SendKey::ShiftEnter,
            "/view status",
            "Composer send key: shift-enter",
        )?;
        submit_multiline(
            app,
            SendKey::ShiftEnter,
            "temporary first",
            "temporary second",
        )?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, bytes);
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::AltEnter,
            "/view status",
            "Composer send key: alt-enter",
        )?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, bytes);
    assert!(!environment.local(0).exists());
    assert_requests(
        &environment.requests()?,
        &["temporary first\ntemporary second"],
    )?;
    environment.assert_mcp_launches(2)?;
    environment.finish()
}

fn local_shadowing() -> TestResult {
    let mut environment = Environment::new()?;
    write_document(&environment.user, &settings("alt-enter"))?;
    let user_bytes = std::fs::read(&environment.user)?;
    let local = environment.local(0);
    let original = json!({"tui":{"extension_data":{"owner":"local"},"extension":[4,5,6]}});
    write_document(&local, &original)?;
    environment.session(0, true, |app| {
        // The complete local tui object shadows lower composer fields, restoring Enter.
        observe(
            app,
            SendKey::Enter,
            "/view status",
            "Composer send key: enter",
        )?;
        let source = observe(
            app,
            SendKey::Enter,
            "/view preferences status",
            "Personal save shadowed by higher layer: true",
        )?;
        assert!(source.contains("WorkspaceLocal"));
        observe(
            app,
            SendKey::Enter,
            "/view preferences local",
            "Preference scope: Local",
        )?;
        observe(
            app,
            SendKey::Enter,
            "/view composer send-key alt-enter",
            "Composer send key: alt-enter",
        )?;
        observe(
            app,
            SendKey::AltEnter,
            "/view composer send-key shift-enter",
            "Composer send key: shift-enter",
        )?;
        Ok(())
    })?;
    let saved = document(&local)?;
    assert_eq!(saved["tui"]["composer"], json!({"send_key":"shift-enter"}));
    assert_eq!(
        saved["tui"]["extension_data"],
        original["tui"]["extension_data"]
    );
    assert_eq!(saved["tui"]["extension"], original["tui"]["extension"]);
    assert_eq!(std::fs::read(&environment.user)?, user_bytes);
    environment.session(0, true, |app| {
        observe(
            app,
            SendKey::ShiftEnter,
            "/view status",
            "Composer send key: shift-enter",
        )?;
        submit_multiline(
            app,
            SendKey::ShiftEnter,
            "local root first",
            "local root second",
        )?;
        Ok(())
    })?;
    environment.session(1, true, |app| {
        observe(
            app,
            SendKey::AltEnter,
            "/view status",
            "Composer send key: alt-enter",
        )?;
        submit_multiline(
            app,
            SendKey::AltEnter,
            "other root first",
            "other root second",
        )?;
        Ok(())
    })?;
    assert_eq!(std::fs::read(&environment.user)?, user_bytes);
    assert!(!environment.local(1).exists());
    assert_requests(
        &environment.requests()?,
        &[
            "local root first\nlocal root second",
            "other root first\nother root second",
        ],
    )?;
    environment.assert_mcp_launches(3)?;
    environment.finish()
}

fn malformed_layers() -> TestResult {
    for (layer, composer, field) in [
        (
            0,
            json!({"send_key":"unsupported-private-marker"}),
            "tui.composer.send_key",
        ),
        (1, json!({"send_key":false}), "tui.composer.send_key"),
        (
            2,
            json!({"send_key":"enter","unknown":true}),
            "tui.composer.unknown",
        ),
    ] {
        let mut environment = Environment::new()?;
        let path = match layer {
            0 => environment.user.clone(),
            1 => environment.shared(0),
            _ => environment.local(0),
        };
        write_document(&path, &json!({"tui":{"composer":composer}}))?;
        if layer < 2 {
            // Invalid lower owned objects must be diagnosed even when shadowed.
            write_document(&environment.local(0), &json!({"tui":{}}))?;
        }
        let before = std::fs::read(&path)?;
        environment.refuse(0, field, &path)?;
        assert_eq!(std::fs::read(&path)?, before);
        assert!(environment.requests()?.is_empty());
        environment.assert_mcp_launches(0)?;
        environment.finish()?;
    }
    Ok(())
}
