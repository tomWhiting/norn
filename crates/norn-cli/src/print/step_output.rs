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
            model: Some(self.model),
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
    model: &str,
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
                model: Some(model),
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

/// Emit the typed error envelope for a failed plain-mode print run
/// (owner rulings 2026-07-06,
/// `docs/reviews/2026-07-05-context-window-incident.md` "Second bug").
///
/// Strictly ADDITIVE to the existing failure surface: the stderr line and
/// the exit code stay exactly as they were. Emits nothing for:
///
/// - `text` format — the human surface; stderr already carries the
///   failure,
/// - failure classes with no envelope
///   ([`PrintError::envelope_class`] → `None`): argument errors keep
///   clap parity (exit 2, stderr-only — R2) and a torn stream gets no
///   envelope at all (R4).
///
/// Driven mode must never route here — its accepted `run/execute` is
/// answered with the id-matched JSON-RPC error response instead
/// (`DRIVEN-PROTOCOL.md`), so an envelope on that stdout would corrupt
/// the frame stream.
///
/// A failure to WRITE the envelope is reported on stderr and deliberately
/// not propagated: the original error (already bound for the stderr line
/// and the exit code) keeps precedence over the secondary write failure.
pub(crate) fn emit_error_envelope(
    cli: &Cli,
    err: &PrintError,
    model: Option<&str>,
    session_id: Option<&str>,
) {
    let Some(class) = err.envelope_class() else {
        return;
    };
    let format = cli.output_format.unwrap_or(OutputFormat::Text);
    let stop = StopInfo::Error {
        message: err.to_string(),
        class: class.to_owned(),
    };
    if let Err(write_err) = write_error_envelope(cli, format, &stop, model, session_id) {
        eprintln!("norn: failed to write the error envelope: {write_err}");
    }
}

/// Write the minimal error envelope for `format` to stdout or `-o PATH`
/// (R5: a consumer watching the file instead of stdout deserves the same
/// typed stop — an empty or absent file on error would recreate the
/// unparseable-failure bug one layer up).
///
/// Mirrors the [`write_handled_locally`] minimal-envelope precedent (R3):
/// `output: null`, default usage, no events, no diagnostics — partial
/// state carriage is explicitly out of scope. `text` writes nothing.
fn write_error_envelope(
    cli: &Cli,
    format: OutputFormat,
    stop: &StopInfo,
    model: Option<&str>,
    session_id: Option<&str>,
) -> std::io::Result<()> {
    let usage = Usage::default();
    match format {
        OutputFormat::Text => Ok(()),
        OutputFormat::Json => {
            let envelope = JsonEnvelope {
                envelope_version: ENVELOPE_VERSION,
                stop,
                output: None,
                usage: UsageOut::from(&usage),
                model,
                session_id,
                events: &[],
                diagnostics: &[],
            };
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                render_json(&mut file, &envelope)
            } else {
                let mut stdout = std::io::stdout().lock();
                render_json(&mut stdout, &envelope)
            }
        }
        OutputFormat::StreamJson => {
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                emit_stream_completed(&mut file, None, &usage, stop, &[])
            } else {
                let mut stdout = std::io::stdout().lock();
                emit_stream_completed(&mut stdout, None, &usage, stop, &[])
            }
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
    use clap::Parser;
    use serde_json::json;

    /// Parse a real CLI invocation so the tests exercise the same `Cli`
    /// the binary hands the writers.
    fn cli_from(args: &[&str]) -> Cli {
        let mut full = vec!["norn"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).unwrap()
    }

    /// R5: `--output PATH` receives the full error envelope in `-f json`
    /// mode — versioned, error stop with message and class, null output,
    /// zeroed usage, model/session carried when known.
    #[test]
    fn error_envelope_json_written_to_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.json");
        let cli = cli_from(&["-p", "-f", "json", "-o", path.to_str().unwrap()]);
        emit_error_envelope(
            &cli,
            &PrintError::Session("index lock lost".to_owned()),
            Some("gpt-5"),
            Some("sess-1"),
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(raw.trim_end()).unwrap();
        assert_eq!(parsed["envelope_version"], json!(1));
        assert_eq!(parsed["stop"]["reason"], json!("error"));
        assert_eq!(parsed["stop"]["class"], json!("session"));
        assert_eq!(
            parsed["stop"]["message"],
            json!("session error: index lock lost")
        );
        assert!(parsed["output"].is_null());
        assert_eq!(parsed["usage"]["input_tokens"], json!(0));
        assert_eq!(parsed["model"], json!("gpt-5"));
        assert_eq!(parsed["session_id"], json!("sess-1"));
        assert_eq!(parsed["events"], json!([]));
        assert_eq!(parsed["diagnostics"], json!([]));
    }

    /// R5, stream-json surface: the file receives the terminal
    /// `completed` event carrying the error stop — the same contract the
    /// stdout NDJSON consumer gets.
    #[test]
    fn error_envelope_stream_json_written_to_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.ndjson");
        let cli = cli_from(&["-p", "-f", "stream-json", "-o", path.to_str().unwrap()]);
        emit_error_envelope(&cli, &PrintError::Io("disk full".to_owned()), None, None);
        let raw = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 1, "one terminal event, no diagnostics");
        let parsed: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["type"], json!("completed"));
        assert_eq!(parsed["envelope_version"], json!(1));
        assert_eq!(parsed["stop"]["reason"], json!("error"));
        assert_eq!(parsed["stop"]["class"], json!("io"));
        assert_eq!(parsed["stop"]["message"], json!("I/O error: disk full"));
        assert!(parsed["output"].is_null());
    }

    /// Text mode gets NO envelope: the stderr line is the whole failure
    /// surface for the human format — the output file is never created.
    #[test]
    fn error_envelope_text_mode_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let cli = cli_from(&["-p", "-o", path.to_str().unwrap()]);
        emit_error_envelope(
            &cli,
            &PrintError::Agent("boom".to_owned()),
            Some("gpt-5"),
            None,
        );
        assert!(!path.exists(), "text mode must not create the output file");
    }

    /// R2 boundary: an argument error (exit 2, clap parity) emits no
    /// envelope on any format; R4 boundary: a torn stream gets none
    /// either — a clean envelope on incomplete NDJSON would make the
    /// output look more trustworthy than it is.
    #[test]
    fn error_envelope_skipped_for_argument_and_torn_stream() {
        let dir = tempfile::tempdir().unwrap();
        for (name, err) in [
            ("arg.json", PrintError::Argument("bad flag".to_owned())),
            ("torn.json", PrintError::StreamTorn("tore".to_owned())),
        ] {
            let path = dir.path().join(name);
            let cli = cli_from(&["-p", "-f", "json", "-o", path.to_str().unwrap()]);
            emit_error_envelope(&cli, &err, None, None);
            assert!(
                !path.exists(),
                "{name}: envelope-less classes must not create the output file"
            );
        }
    }

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
