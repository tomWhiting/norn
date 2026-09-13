//! Actual terminal cancellation with a held provider and no explicit provider release.

use super::*;

/// Exercise both separate presses and a queued pair through the actual App.
pub fn verify_interrupt_exit(queued_pair: bool) -> TestResult {
    let mut app = Workspace::start(Some("enter"), false, false, None)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exercise(&mut app, queued_pair)
    }))
    .map_err(|payload| panic_error(payload.as_ref(), "interrupt assertions"))
    .and_then(|result| result);
    let cleanup = app.finish(result.is_err());
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (result, cleanup) => Err(io::Error::other(format!(
            "interrupt exercise: {result:?}; cleanup: {cleanup:?}; terminal:\n{}",
            String::from_utf8_lossy(&app.output.bytes()?)
        ))
        .into()),
    }
}

fn exercise(app: &mut Workspace, queued_pair: bool) -> io::Result<()> {
    app.input(b"interrupt fixture\r", |screen| screen.contains(INITIAL))?;
    app.input(b"next draft", |screen| {
        screen
            .composer_rows()
            .iter()
            .any(|row| screen.lines()[*row].contains("next draft"))
    })?;
    let before = app.snapshot()?;
    if before["provider_calls"] != 1 || before["user_events"] != json!(["interrupt fixture"]) {
        return Err(io::Error::other(format!(
            "unexpected initial admission: {before}"
        )));
    }
    if queued_pair {
        app.control("close")?;
        app.send(b"\x03\x03")?;
    } else {
        let screen = app.input(b"\x03", |screen| {
            screen.contains("Press Ctrl+C again within 3s to exit")
        })?;
        if !screen
            .composer_rows()
            .iter()
            .any(|row| screen.lines()[*row].contains("next draft"))
        {
            return Err(io::Error::other("cancellation discarded the next draft"));
        }
        app.control("close")?;
        app.send(b"\x03")?;
    }
    app.finish(false)?;
    let report: Value = serde_json::from_slice(&std::fs::read(&app.final_report)?)?;
    if report["provider_calls"] != 1 || report["user_events"] != json!(["interrupt fixture"]) {
        return Err(io::Error::other(format!(
            "cancelled input was re-admitted: {report}"
        )));
    }
    Lifecycle::from_output(&app.output.bytes()?, 24, 100).assert_restored()
}
