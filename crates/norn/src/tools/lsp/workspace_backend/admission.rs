//! Norn descriptor admission for Chiron language-server processes.

use std::sync::{Arc, Mutex};

use lsp::server::admission::{
    ProcessAdmissionError, STDIO_PARENT_DESCRIPTOR_WEIGHT, STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT,
    ServerProcessAdmission, ServerProcessLease, ServerSpawnRequest,
};

use crate::resource::{
    DescriptorGovernor, DescriptorPermit, THREE_PIPE_RETAINED, THREE_PIPE_SPAWN_PEAK,
};

#[derive(Debug)]
pub(super) struct NornServerProcessAdmission {
    governor: Arc<DescriptorGovernor>,
}

impl NornServerProcessAdmission {
    pub(super) fn new() -> Result<Arc<Self>, crate::resource::DescriptorAdmissionError> {
        Ok(Arc::new(Self {
            governor: DescriptorGovernor::global()?,
        }))
    }
}

#[derive(Debug)]
enum LeaseState {
    SpawnPeak(DescriptorPermit),
    Retained { _permit: DescriptorPermit },
}

#[derive(Debug)]
struct NornServerProcessLease {
    state: Mutex<Option<LeaseState>>,
}

impl ServerProcessLease for NornServerProcessLease {
    fn settle_after_spawn(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::error!(
                    "language-server descriptor lease mutex was poisoned; recovering safely"
                );
                poisoned.into_inner()
            }
        };
        let Some(current) = state.take() else {
            tracing::error!("language-server descriptor lease lost its owned permit");
            return;
        };
        let LeaseState::SpawnPeak(mut peak) = current else {
            *state = Some(current);
            return;
        };
        let Some(retained) = peak.split(THREE_PIPE_RETAINED) else {
            tracing::error!(
                spawn_peak = STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT,
                retained = THREE_PIPE_RETAINED,
                "language-server descriptor lease could not settle; retaining the full peak"
            );
            *state = Some(LeaseState::SpawnPeak(peak));
            return;
        };
        *state = Some(LeaseState::Retained { _permit: retained });
    }
}

impl ServerProcessAdmission for NornServerProcessAdmission {
    fn try_acquire(
        &self,
        request: &ServerSpawnRequest,
    ) -> Result<Arc<dyn ServerProcessLease>, ProcessAdmissionError> {
        if request.spawn_peak_descriptor_weight != THREE_PIPE_SPAWN_PEAK
            || request.spawn_peak_descriptor_weight != STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT
            || request.retained_descriptor_weight != THREE_PIPE_RETAINED
            || request.retained_descriptor_weight != STDIO_PARENT_DESCRIPTOR_WEIGHT
        {
            return Err(ProcessAdmissionError::new(AdmissionContractError {
                requested_peak: request.spawn_peak_descriptor_weight,
                requested_retained: request.retained_descriptor_weight,
            }));
        }
        let permit = self
            .governor
            .try_acquire(request.spawn_peak_descriptor_weight)
            .map_err(ProcessAdmissionError::new)?;
        Ok(Arc::new(NornServerProcessLease {
            state: Mutex::new(Some(LeaseState::SpawnPeak(permit))),
        }))
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "Chiron requested language-server descriptor weights peak={requested_peak}, retained={requested_retained}; expected peak={THREE_PIPE_SPAWN_PEAK}, retained={THREE_PIPE_RETAINED}"
)]
struct AdmissionContractError {
    requested_peak: u32,
    requested_retained: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ServerSpawnRequest {
        ServerSpawnRequest {
            server_name: "test-server".to_owned(),
            root: std::path::PathBuf::from("/tmp/test-workspace"),
            spawn_peak_descriptor_weight: STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT,
            retained_descriptor_weight: STDIO_PARENT_DESCRIPTOR_WEIGHT,
        }
    }

    #[test]
    fn lease_settles_from_exact_spawn_peak_and_releases_on_drop()
    -> Result<(), Box<dyn std::error::Error>> {
        let governor = Arc::new(DescriptorGovernor::with_capacity(
            STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT,
        ));
        let admission = NornServerProcessAdmission {
            governor: Arc::clone(&governor),
        };
        let lease = admission.try_acquire(&request())?;
        assert_eq!(governor.available(), 0);
        lease.settle_after_spawn();
        let released_surplus =
            usize::try_from(STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT - STDIO_PARENT_DESCRIPTOR_WEIGHT)?;
        assert_eq!(governor.available(), released_surplus);
        lease.settle_after_spawn();
        assert_eq!(governor.available(), released_surplus);
        drop(lease);
        assert_eq!(
            governor.available(),
            usize::try_from(STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT)?,
        );
        Ok(())
    }

    #[test]
    fn mismatched_contract_is_refused_without_consuming_capacity()
    -> Result<(), Box<dyn std::error::Error>> {
        let governor = Arc::new(DescriptorGovernor::with_capacity(
            STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT,
        ));
        let admission = NornServerProcessAdmission {
            governor: Arc::clone(&governor),
        };
        let mut invalid = request();
        invalid.spawn_peak_descriptor_weight =
            invalid.spawn_peak_descriptor_weight.saturating_sub(1);
        assert!(admission.try_acquire(&invalid).is_err());
        assert_eq!(
            governor.available(),
            usize::try_from(STDIO_SPAWN_PEAK_DESCRIPTOR_WEIGHT)?,
        );
        Ok(())
    }
}
