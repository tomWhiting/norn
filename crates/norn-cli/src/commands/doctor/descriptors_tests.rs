use norn::resource::{DescriptorLimits, DescriptorOpenCount, DescriptorSnapshot};

use crate::nofile::{NofileCeiling, NofileInitReport, NofileOutcome};

use super::render_descriptor_report;

fn report(outcome: NofileOutcome) -> NofileInitReport {
    NofileInitReport {
        inherited: DescriptorLimits {
            soft: Some(256),
            hard: None,
        },
        ceiling: Some(NofileCeiling {
            value: 12_288,
            source: "kern.maxfilesperproc",
        }),
        target: Some(12_288),
        effective: DescriptorLimits {
            soft: Some(12_288),
            hard: None,
        },
        outcome,
    }
}

fn snapshot() -> DescriptorSnapshot {
    DescriptorSnapshot {
        limits: Some(DescriptorLimits {
            soft: Some(12_288),
            hard: None,
        }),
        limits_error: None,
        open: Some(DescriptorOpenCount {
            count: 42,
            source: "/dev/fd",
            includes_observer: true,
        }),
        open_error: None,
    }
}

#[test]
fn successful_report_names_before_after_ceiling_and_open_count() {
    let (ok, line) = render_descriptor_report(Some(&report(NofileOutcome::Raised)), &snapshot());

    assert!(ok);
    assert!(line.contains("inherited soft 256, hard unlimited"));
    assert!(line.contains("12288 via kern.maxfilesperproc"));
    assert!(line.contains("effective soft 12288, hard unlimited"));
    assert!(line.contains("open 42 via /dev/fd (includes observer)"));
    assert!(!line.contains("ulimit"));
}

#[test]
fn mutation_failure_and_missing_count_fail_truthfully() {
    let mut snapshot = snapshot();
    snapshot.open = None;
    snapshot.open_error = Some("descriptor directory unavailable".to_owned());
    let (ok, line) = render_descriptor_report(
        Some(&report(NofileOutcome::Failed {
            reason: "permission denied".to_owned(),
        })),
        &snapshot,
    );

    assert!(!ok);
    assert!(line.contains("permission denied"));
    assert!(line.contains("unavailable (descriptor directory unavailable)"));
}
