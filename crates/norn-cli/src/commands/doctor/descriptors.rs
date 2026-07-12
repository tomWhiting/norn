//! `norn doctor` file-descriptor capacity report.

use norn::resource::{DescriptorLimits, descriptor_snapshot};

use crate::nofile::{NofileInitReport, NofileOutcome, initialization_report};

pub(super) fn check_descriptors() -> bool {
    let snapshot = descriptor_snapshot();
    let (ok, line) = render_descriptor_report(initialization_report(), &snapshot);
    eprintln!("{line}");
    ok
}

fn render_descriptor_report(
    report: Option<&NofileInitReport>,
    snapshot: &norn::resource::DescriptorSnapshot,
) -> (bool, String) {
    let Some(report) = report else {
        return (
            false,
            "[FAIL] File-descriptor initialization report unavailable: run the official `norn doctor` binary entrypoint."
                .to_owned(),
        );
    };
    let open = snapshot.open.as_ref().map_or_else(
        || {
            format!(
                "unavailable ({})",
                snapshot
                    .open_error
                    .as_deref()
                    .unwrap_or("reason unavailable")
            )
        },
        |open| format!("{} via {} (includes observer)", open.count, open.source),
    );
    let ceiling = report.ceiling.as_ref().map_or_else(
        || "unavailable".to_owned(),
        |ceiling| format!("{} via {}", ceiling.value, ceiling.source),
    );
    let summary = format!(
        "inherited {}; ceiling {ceiling}; effective {}; open {open}",
        format_limits(report.inherited),
        format_limits(report.effective),
    );
    match &report.outcome {
        NofileOutcome::Raised | NofileOutcome::Unchanged if snapshot.open.is_some() => {
            (true, format!("[PASS] File-descriptor capacity: {summary}"))
        }
        NofileOutcome::Raised | NofileOutcome::Unchanged => (
            false,
            format!("[FAIL] File-descriptor count unavailable: {summary}"),
        ),
        NofileOutcome::Failed { reason } => (
            false,
            format!("[FAIL] File-descriptor capacity ({reason}): {summary}"),
        ),
    }
}

fn format_limits(limits: DescriptorLimits) -> String {
    format!(
        "soft {}, hard {}",
        format_limit(limits.soft),
        format_limit(limits.hard),
    )
}

fn format_limit(value: Option<u64>) -> String {
    value.map_or_else(|| "unlimited".to_owned(), |limit| limit.to_string())
}

#[cfg(test)]
#[path = "descriptors_tests.rs"]
mod tests;
