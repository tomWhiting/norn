use chrono::Utc;

use super::super::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionRecordOrigin, SessionStatus,
};
use super::validate_index_entries;

fn entry(id: String, parent_id: Option<String>, rel_path: Option<String>) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id,
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path,
        parent_id,
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
        provider_state_identity: None,
    }
}

#[test]
fn deep_parent_graph_validates_without_a_depth_limit() -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = vec![entry("root".to_owned(), None, None)];
    let mut parent = "root".to_owned();
    for index in 0..8_192 {
        let id = format!("child-{index}");
        entries.push(entry(
            id.clone(),
            Some(parent),
            Some(format!("root/children/{id}.jsonl")),
        ));
        parent = id;
    }

    validate_index_entries(&entries)?;
    Ok(())
}

#[test]
fn parent_cycle_fails_closed() {
    let entries = vec![
        entry(
            "cycle-a".to_owned(),
            Some("cycle-b".to_owned()),
            Some("cycle-a/children/cycle-a.jsonl".to_owned()),
        ),
        entry(
            "cycle-b".to_owned(),
            Some("cycle-a".to_owned()),
            Some("cycle-a/children/cycle-b.jsonl".to_owned()),
        ),
    ];
    assert!(validate_index_entries(&entries).is_err());
}
