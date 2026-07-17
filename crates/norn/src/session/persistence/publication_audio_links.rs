use std::collections::BTreeMap;

use crate::session::events::SessionEvent;
use crate::session::response_audio::{
    referenced_response_audio_artifacts, response_audio_artifact_links,
};
use crate::session::{ResponseAudioArtifactRef, SessionPersistError};

pub(super) struct ReferenceRequirement {
    pub(super) reference: ResponseAudioArtifactRef,
    binding: ReferenceBinding,
}

enum ReferenceBinding {
    PartialOnly,
    Linked(Option<String>),
}

pub(super) fn collect_reference_requirements(
    events: &[SessionEvent],
) -> Result<Vec<ReferenceRequirement>, SessionPersistError> {
    let references = referenced_response_audio_artifacts(events)?;
    let links = response_audio_artifact_links(events)?;
    let mut linked = BTreeMap::<String, Option<String>>::new();
    for link in links {
        let name = link.reference().file_name();
        let response_id = link.response_id().map(str::to_owned);
        if linked
            .get(&name)
            .is_some_and(|existing| existing != &response_id)
        {
            return Err(invalid_artifact(
                link.reference(),
                "multiple links disagree on the artifact response ID",
            ));
        }
        linked.insert(name, response_id);
    }
    Ok(references
        .into_iter()
        .map(|reference| ReferenceRequirement {
            binding: match linked.remove(&reference.file_name()) {
                Some(response_id) => ReferenceBinding::Linked(response_id),
                None => ReferenceBinding::PartialOnly,
            },
            reference,
        })
        .collect())
}

pub(super) fn validate_link_binding(
    requirement: &ReferenceRequirement,
    artifact_sealed: bool,
    terminal_response_id: Option<&str>,
) -> Result<(), SessionPersistError> {
    let expected_response_id = match &requirement.binding {
        ReferenceBinding::PartialOnly => return Ok(()),
        ReferenceBinding::Linked(response_id) => response_id,
    };
    if !artifact_sealed {
        return Err(invalid_artifact(
            requirement.reference,
            "a linked response-audio artifact was not sealed",
        ));
    }
    if terminal_response_id != expected_response_id.as_deref() {
        return Err(invalid_artifact(
            requirement.reference,
            "linked and terminal response IDs disagreed",
        ));
    }
    Ok(())
}

fn invalid_artifact(
    reference: ResponseAudioArtifactRef,
    reason: &'static str,
) -> SessionPersistError {
    SessionPersistError::InvalidResponseAudioArtifact {
        artifact_id: reference.to_string(),
        reason,
    }
}
