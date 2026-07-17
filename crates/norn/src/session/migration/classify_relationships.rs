use std::collections::BTreeMap;
use std::path::{Component, Path};

use crate::session::persistence::strict::ResumeFidelity;

use super::classify::{ClassifiedSession, encode_relative_path};
use super::error::SessionMigrationError;
use super::types::{LegacyClassificationReason, LegacySessionMigrationRecord};

pub(super) fn demote_invalid_relationships(
    sessions: &mut Vec<ClassifiedSession>,
    records: &mut [LegacySessionMigrationRecord],
) -> Result<(), SessionMigrationError> {
    let by_id = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.entry.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut states = sessions
        .iter()
        .map(|session| {
            if session.entry.rel_path.is_some() == session.entry.parent_id.is_some() {
                RelationshipState::Unvisited
            } else {
                RelationshipState::Complete(RelationshipOutcome::Invalid(
                    "rel_path and parent_id must both be present for a child or absent for a root"
                        .to_owned(),
                ))
            }
        })
        .collect::<Vec<_>>();

    for start in 0..sessions.len() {
        resolve_relationship(start, sessions, &by_id, &mut states);
    }
    let invalid = sessions
        .iter()
        .enumerate()
        .filter_map(|(index, session)| match &states[index] {
            RelationshipState::Complete(RelationshipOutcome::Invalid(diagnostic)) => {
                Some((session.entry.id.clone(), diagnostic.clone()))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    demote(sessions, records, &invalid)
}

fn resolve_relationship(
    start: usize,
    sessions: &[ClassifiedSession],
    by_id: &BTreeMap<&str, usize>,
    states: &mut [RelationshipState],
) {
    if matches!(states[start], RelationshipState::Complete(_)) {
        return;
    }
    let mut path = Vec::new();
    let mut current = start;
    loop {
        match &states[current] {
            RelationshipState::Complete(_) => break,
            RelationshipState::Visiting => {
                if let Some(cycle_start) = path.iter().position(|candidate| *candidate == current) {
                    for index in &path[cycle_start..] {
                        states[*index] = RelationshipState::Complete(RelationshipOutcome::Invalid(
                            "parent_id chain contains a cycle".to_owned(),
                        ));
                    }
                } else {
                    states[current] = RelationshipState::Complete(RelationshipOutcome::Invalid(
                        "parent relationship visitation state is inconsistent".to_owned(),
                    ));
                }
                break;
            }
            RelationshipState::Unvisited => {
                states[current] = RelationshipState::Visiting;
                path.push(current);
            }
        }
        let Some(parent_id) = sessions[current].entry.parent_id.as_deref() else {
            states[current] =
                RelationshipState::Complete(RelationshipOutcome::Valid { root: current });
            break;
        };
        let Some(parent) = by_id.get(parent_id).copied() else {
            states[current] = RelationshipState::Complete(RelationshipOutcome::Invalid(format!(
                "parent session '{parent_id}' is absent or not resumable"
            )));
            break;
        };
        current = parent;
    }
    for index in path.into_iter().rev() {
        if matches!(states[index], RelationshipState::Complete(_)) {
            continue;
        }
        states[index] = outcome_from_parent(index, sessions, by_id, states);
    }
}

fn outcome_from_parent(
    index: usize,
    sessions: &[ClassifiedSession],
    by_id: &BTreeMap<&str, usize>,
    states: &[RelationshipState],
) -> RelationshipState {
    let Some(parent_id) = sessions[index].entry.parent_id.as_deref() else {
        return RelationshipState::Complete(RelationshipOutcome::Valid { root: index });
    };
    let Some(parent) = by_id.get(parent_id).copied() else {
        return RelationshipState::Complete(RelationshipOutcome::Invalid(format!(
            "parent session '{parent_id}' is absent or not resumable"
        )));
    };
    match &states[parent] {
        RelationshipState::Complete(RelationshipOutcome::Valid { root }) => {
            let root_id = sessions[*root].entry.id.as_str();
            if first_component(sessions[index].entry.rel_path.as_deref()) == Some(root_id) {
                RelationshipState::Complete(RelationshipOutcome::Valid { root: *root })
            } else {
                RelationshipState::Complete(RelationshipOutcome::Invalid(format!(
                    "child timeline is not rooted beneath ultimate parent '{root_id}'"
                )))
            }
        }
        RelationshipState::Complete(RelationshipOutcome::Invalid(_)) => {
            RelationshipState::Complete(RelationshipOutcome::Invalid(format!(
                "parent session '{parent_id}' is not resumable"
            )))
        }
        RelationshipState::Unvisited | RelationshipState::Visiting => RelationshipState::Complete(
            RelationshipOutcome::Invalid("parent relationship did not converge".to_owned()),
        ),
    }
}

fn first_component(relative: Option<&str>) -> Option<&str> {
    Path::new(relative?)
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
}

fn demote(
    sessions: &mut Vec<ClassifiedSession>,
    records: &mut [LegacySessionMigrationRecord],
    invalid: &BTreeMap<String, String>,
) -> Result<(), SessionMigrationError> {
    if invalid.is_empty() {
        return Ok(());
    }
    let record_positions = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            Some((
                (record.session_id.clone()?, record.source_path.clone()?),
                index,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let mut removed = Vec::new();
    sessions.retain(|session| {
        if let Some(diagnostic) = invalid.get(&session.entry.id) {
            removed.push((session.clone(), diagnostic.clone()));
            false
        } else {
            true
        }
    });
    for (session, diagnostic) in removed {
        let source_path = encode_relative_path(&session.source_path)?;
        let key = (session.entry.id, source_path);
        if let Some(position) = record_positions.get(&key).copied() {
            let record = &mut records[position];
            record.fidelity = ResumeFidelity::InspectOnly;
            record.destination_path = None;
            record
                .reasons
                .push(LegacyClassificationReason::InvalidIndexRow {
                    line: record.source_index_line,
                    diagnostic,
                });
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum RelationshipState {
    Unvisited,
    Visiting,
    Complete(RelationshipOutcome),
}

#[derive(Clone, Debug)]
enum RelationshipOutcome {
    Valid { root: usize },
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::session::migration::classify::TimelineTotals;
    use crate::session::migration::legacy_index::LegacySessionIndexEntry;
    use crate::session::persistence::SessionStatus;

    fn session(id: &str, parent_id: Option<String>, root: &str) -> ClassifiedSession {
        let now = Utc::now();
        let rel_path = parent_id
            .as_ref()
            .map(|_| format!("{root}/children/{id}.jsonl"));
        ClassifiedSession {
            entry: LegacySessionIndexEntry {
                id: id.to_owned(),
                name: None,
                model: "test-model".to_owned(),
                working_dir: "/work".to_owned(),
                created_at: now,
                updated_at: now,
                event_count: 0,
                status: SessionStatus::Active,
                format_version: 1,
                total_input_tokens: 0,
                total_output_tokens: 0,
                total_cache_read_tokens: 0,
                rel_path,
                parent_id,
            },
            source_path: format!("{id}.jsonl").into(),
            source_sha256: "a".repeat(64),
            source_format: 1,
            fidelity: ResumeFidelity::Canonical,
            totals: TimelineTotals::default(),
        }
    }

    #[test]
    fn deep_legacy_parent_graph_has_no_depth_cap() -> Result<(), SessionMigrationError> {
        let mut sessions = vec![session("root", None, "root")];
        let mut parent = "root".to_owned();
        for index in 0..8_192 {
            let id = format!("child-{index}");
            sessions.push(session(&id, Some(parent), "root"));
            parent = id;
        }
        let expected = sessions.len();

        demote_invalid_relationships(&mut sessions, &mut [])?;
        assert_eq!(sessions.len(), expected);
        Ok(())
    }

    #[test]
    fn cycle_and_its_descendant_are_demoted_in_one_graph_pass() -> Result<(), SessionMigrationError>
    {
        let mut sessions = vec![
            session("cycle-a", Some("cycle-b".to_owned()), "cycle-a"),
            session("cycle-b", Some("cycle-a".to_owned()), "cycle-a"),
            session("descendant", Some("cycle-a".to_owned()), "cycle-a"),
        ];

        demote_invalid_relationships(&mut sessions, &mut [])?;
        assert!(sessions.is_empty());
        Ok(())
    }
}
