//! A blocked synchronous provider must not occupy the terminal's input/render owner.

use super::{
    OutputBuffer, PTY_APP_CHILD_ENV, PtyInteraction, PtySizeSpec, TerminalScreen, child_failure,
    clone_output, request_idle_exit, run_child_to_completion, wait_for_frame, wait_for_screen,
};
use futures_util::stream;
use norn::provider::{
    Provider, ProviderCapabilities, ProviderError, ProviderRequest, ProviderStream,
};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DRAFT: &str = "typing while provider preparation is busy";

pub(super) struct BusyProvider;

impl Provider for BusyProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn stream(&self, request: ProviderRequest) -> Result<ProviderStream, ProviderError> {
        drop(request);
        // Deliberate synchronous stand-in for request encoding/validation work.
        // The test budget is smaller; production receives no new delay or limit.
        std::thread::sleep(Duration::from_secs(5));
        // Then wait like an outstanding network request until explicit cancellation.
        Ok(Box::pin(stream::pending()))
    }
}

#[test]
fn run_app_accepts_input_during_synchronous_provider_work() -> Result<(), Box<dyn std::error::Error>>
{
    let run = run_child_to_completion(
        "run_app_child_entrypoint",
        PTY_APP_CHILD_ENV,
        Some("synchronous-provider-work"),
        PtyInteraction::TypeDuringSynchronousWork,
        PtySizeSpec::default(),
    )?;
    if !run.status.success() {
        return Err(child_failure("synchronous provider work", &run.status, &run.output).into());
    }
    assert_eq!(
        run.runtime.ok_or("missing runtime receipt")?["provider_calls"],
        1
    );
    Ok(())
}

pub(super) fn interact(
    writer: &mut impl Write,
    output: &Arc<OutputBuffer>,
    size: PtySizeSpec,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    writer.write_all(DRAFT.as_bytes())?;
    writer.flush()?;
    wait_for_frame(
        output,
        &[(size.rows, size.cols)],
        |screen| {
            screen
                .composer_rows()
                .iter()
                .any(|row| screen.lines()[*row].contains(DRAFT))
        },
        Duration::from_secs(2),
    )?;
    eprintln!("busy provider input-to-composer: {:?}", started.elapsed());
    // The provider deliberately occupies its worker for five seconds. Input
    // must be visible before its completion, not merely queued until afterwards.
    let snapshot = clone_output(output)?;
    let screen = TerminalScreen::from_output(&snapshot, size.rows, size.cols)?;
    assert!(!screen.contains("Turn completed"));
    let lines = screen.lines();
    let (row, line) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains("Latest"))
        .ok_or("pinned view has no Latest control")?;
    let offset = line.find("Latest").ok_or("Latest label disappeared")?;
    let column = line[..offset].chars().count() + 1;
    write!(
        writer,
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        column,
        row + 1,
        column,
        row + 1
    )?;
    writer.flush()?;
    writer.write_all(b"\x03")?;
    writer.flush()?;
    wait_for_screen(output, "Turn cancelled", size, Duration::from_secs(8))?;
    request_idle_exit(writer, output, &[(size.rows, size.cols)])?;
    Ok(())
}
