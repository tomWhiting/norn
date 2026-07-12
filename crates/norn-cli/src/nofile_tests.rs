use std::cell::{Cell, RefCell};

use norn::resource::DescriptorLimits;

use super::{NofileBackend, NofileCeiling, NofileOutcome, initialize_with};

struct FakeBackend {
    limits: RefCell<DescriptorLimits>,
    ceiling: Result<NofileCeiling, String>,
    set_error: Option<String>,
    set_calls: Cell<u64>,
}

impl NofileBackend for FakeBackend {
    fn limits(&self) -> DescriptorLimits {
        *self.limits.borrow()
    }

    fn ceiling(&self, _inherited: DescriptorLimits) -> Result<NofileCeiling, String> {
        self.ceiling.clone()
    }

    fn set_soft(&self, target: u64, _hard: Option<u64>) -> Result<(), String> {
        self.set_calls.set(self.set_calls.get() + 1);
        if let Some(reason) = &self.set_error {
            return Err(reason.clone());
        }
        self.limits.borrow_mut().soft = Some(target);
        Ok(())
    }
}

fn backend(soft: Option<u64>, hard: Option<u64>, ceiling: u64) -> FakeBackend {
    FakeBackend {
        limits: RefCell::new(DescriptorLimits { soft, hard }),
        ceiling: Ok(NofileCeiling {
            value: ceiling,
            source: "test ceiling",
        }),
        set_error: None,
        set_calls: Cell::new(0),
    }
}

#[test]
fn raises_once_to_finite_os_ceiling_clamped_by_hard_limit() {
    let backend = backend(Some(256), Some(4_096), 8_192);
    let report = initialize_with(&backend);

    assert_eq!(report.target, Some(4_096));
    assert_eq!(report.effective.soft, Some(4_096));
    assert_eq!(report.outcome, NofileOutcome::Raised);
    assert_eq!(backend.set_calls.get(), 1);
}

#[test]
fn never_lowers_an_inherited_soft_limit() {
    let backend = backend(Some(8_192), Some(8_192), 4_096);
    let report = initialize_with(&backend);

    assert_eq!(report.effective.soft, Some(8_192));
    assert_eq!(report.outcome, NofileOutcome::Unchanged);
    assert_eq!(backend.set_calls.get(), 0);
}

#[test]
fn unlimited_soft_limit_is_preserved_without_mutation() {
    let backend = backend(None, None, 4_096);
    let report = initialize_with(&backend);

    assert_eq!(report.effective.soft, None);
    assert_eq!(report.outcome, NofileOutcome::Unchanged);
    assert_eq!(backend.set_calls.get(), 0);
}

#[test]
fn ceiling_discovery_failure_is_explicit_and_does_not_mutate() {
    let backend = FakeBackend {
        limits: RefCell::new(DescriptorLimits {
            soft: Some(256),
            hard: None,
        }),
        ceiling: Err("no finite ceiling".to_owned()),
        set_error: None,
        set_calls: Cell::new(0),
    };
    let report = initialize_with(&backend);

    assert!(matches!(report.outcome, NofileOutcome::Failed { .. }));
    assert!(report.ceiling.is_none());
    assert_eq!(backend.set_calls.get(), 0);
}

#[test]
fn set_failure_is_reported_with_observed_effective_limits() {
    let mut backend = backend(Some(256), Some(4_096), 4_096);
    backend.set_error = Some("permission denied".to_owned());
    let report = initialize_with(&backend);

    assert!(matches!(report.outcome, NofileOutcome::Failed { .. }));
    assert_eq!(report.effective.soft, Some(256));
    assert_eq!(backend.set_calls.get(), 1);
}
