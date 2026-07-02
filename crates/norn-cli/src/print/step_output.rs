//! Post-step output dispatch for print-mode execution.
//!
//! Bundles the completed step's data ([`StepOutput`]) and routes it to the
//! selected output surface: text / json / stream-json to stdout or `-o
//! PATH`, or the driven-mode `run/execute` result value. Extracted from
//! `orchestrator.rs` so that module stays within the 500-line budget.

use serde_json::Value;

use norn::provider::usage::Usage;
use norn::session::events::SessionEvent;

use super::orchestrator::PrintError;
use super::output::{
    ENVELOPE_VERSION, JsonEnvelope, StopInfo, UsageOut, emit_stream_completed, render_json,
    render_text,
};
use crate::cli::{Cli, OutputFormat};
use crate::runtime::RuntimeBundle;

/// Post-step output data bundled for the output writers. Eliminates
/// the `too_many_arguments` lint without sacrificing the named-field
/// clarity that the orchestrator needs.
pub(crate) struct StepOutput<'a> {
    /// Final output for a completed stop; partial output otherwise.
    pub(crate) output: Option<&'a Value>,
    /// Accumulated token usage across the step.
    pub(crate) usage: &'a Usage,
    /// Model identifier used for the call.
    pub(crate) model: &'a str,
    /// Session ID when persistence is enabled.
    pub(crate) session_id: Option<&'a str>,
    /// Session events emitted during this step.
    pub(crate) events: &'a [SessionEvent],
    /// Typed stop information projected from the step result.
    pub(crate) stop: &'a StopInfo,
    /// Diagnostics collected during the step.
    pub(crate) diagnostics: &'a [norn::integration::NornDiagnostic],
}

impl StepOutput<'_> {
    /// The [`JsonEnvelope`] projection of this step — the single
    /// structured output shape shared by `-f json` and the driven
    /// `run/execute` result.
    fn envelope(&self) -> JsonEnvelope<'_> {
        JsonEnvelope {
            envelope_version: ENVELOPE_VERSION,
            stop: self.stop,
            output: self.output,
            usage: UsageOut::from(self.usage),
            model: self.model,
            session_id: self.session_id,
            events: self.events,
            diagnostics: self.diagnostics,
        }
    }
}

/// Write the completed step through the format-selected renderer.
pub(crate) fn write_output(
    cli: &Cli,
    format: OutputFormat,
    step: &StepOutput<'_>,
) -> Result<(), PrintError> {
    match format {
        OutputFormat::Text => write_text(cli, step.output, step.diagnostics),
        OutputFormat::Json => write_json(cli, step),
        OutputFormat::StreamJson => {
            write_stream_completed(cli, step.output, step.usage, step.stop, step.diagnostics)
        }
    }
}

fn write_text(
    cli: &Cli,
    output: Option<&Value>,
    diagnostics: &[norn::integration::NornDiagnostic],
) -> Result<(), PrintError> {
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        let mut stderr = std::io::stderr().lock();
        render_text(&mut file, &mut stderr, output, diagnostics, cli.quiet)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    render_text(&mut stdout, &mut stderr, output, diagnostics, cli.quiet)?;
    Ok(())
}

fn write_json(cli: &Cli, step: &StepOutput<'_>) -> Result<(), PrintError> {
    let envelope = step.envelope();
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        render_json(&mut file, &envelope)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    render_json(&mut stdout, &envelope)?;
    Ok(())
}

fn write_stream_completed(
    cli: &Cli,
    output: Option<&Value>,
    usage: &Usage,
    stop: &StopInfo,
    diagnostics: &[norn::integration::NornDiagnostic],
) -> Result<(), PrintError> {
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        emit_stream_completed(&mut file, output, usage, stop, diagnostics)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    emit_stream_completed(&mut stdout, output, usage, stop, diagnostics)?;
    Ok(())
}

/// Render the "no agent call" output for a dispatch that was handled
/// locally. For `text` mode this is a no-op (the closure already wrote
/// to stderr); for `json` it produces a minimal envelope with no model
/// output; for `stream-json` it emits a single `completed` event.
pub(crate) fn write_handled_locally(
    cli: &Cli,
    format: OutputFormat,
    bundle: &RuntimeBundle,
    session_id: Option<&str>,
) -> Result<(), PrintError> {
    let usage = Usage::default();
    let diagnostics: Vec<norn::integration::NornDiagnostic> = Vec::new();
    match format {
        OutputFormat::Text => Ok(()),
        OutputFormat::Json => {
            let envelope = JsonEnvelope {
                envelope_version: ENVELOPE_VERSION,
                stop: &StopInfo::Completed,
                output: None,
                usage: UsageOut::from(&usage),
                model: &bundle.model,
                session_id,
                events: &[],
                diagnostics: &diagnostics,
            };
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                render_json(&mut file, &envelope)?;
            } else {
                let mut stdout = std::io::stdout().lock();
                render_json(&mut stdout, &envelope)?;
            }
            Ok(())
        }
        OutputFormat::StreamJson => {
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                emit_stream_completed(&mut file, None, &usage, &StopInfo::Completed, &diagnostics)?;
            } else {
                let mut stdout = std::io::stdout().lock();
                emit_stream_completed(
                    &mut stdout,
                    None,
                    &usage,
                    &StopInfo::Completed,
                    &diagnostics,
                )?;
            }
            Ok(())
        }
    }
}

/// Build the `run/execute` response result value from the completed step.
///
/// The shape is the SAME structured envelope `-f json` produces
/// ([`JsonEnvelope`]), serialised to a [`Value`] so it rides the JSON-RPC
/// response `result` field. This keeps the driven result byte-compatible
/// with the one-shot capture path (`DRIVEN-PROTOCOL.md` "Stop envelope").
pub(crate) fn driven_result_value(step: &StepOutput<'_>) -> Result<Value, PrintError> {
    serde_json::to_value(step.envelope()).map_err(|err| PrintError::Agent(err.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The driven result value is the SAME envelope shape as `-f json`:
    /// versioned, typed stop, partial output, usage.
    #[test]
    fn driven_result_value_matches_json_envelope_shape() {
        let usage = Usage {
            input_tokens: 11,
            output_tokens: 4,
            ..Usage::default()
        };
        let output = json!("partial answer");
        let stop_info = StopInfo::TimedOut {
            elapsed_ms: 1200,
            iterations: 2,
        };
        let step = StepOutput {
            output: Some(&output),
            usage: &usage,
            model: "gpt-5",
            session_id: Some("s-1"),
            events: &[],
            stop: &stop_info,
            diagnostics: &[],
        };
        let value = driven_result_value(&step).unwrap();
        assert_eq!(value["envelope_version"], json!(1));
        assert_eq!(value["stop"]["reason"], json!("timed_out"));
        assert_eq!(value["stop"]["elapsed_ms"], json!(1200));
        assert_eq!(value["output"], json!("partial answer"));
        assert_eq!(value["usage"]["input_tokens"], json!(11));
        assert_eq!(value["model"], json!("gpt-5"));
        assert_eq!(value["session_id"], json!("s-1"));
        assert!(
            value["stop"].get("retryable").is_none(),
            "retryability is the caller's judgment — never encoded"
        );
    }
}
