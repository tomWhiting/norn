use super::*;
use crate::session::persistence::types::{SESSION_FORMAT_VERSION, SessionStatus};

fn entry(id: &str, name: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        model: "gpt-test".to_owned(),
        working_dir: "/work".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
    }
}

#[cfg(unix)]
#[test]
fn locked_index_transaction_remains_on_root_after_root_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let container = tempfile::tempdir()?;
    let data_dir = container.path().join("sessions");
    let parked = container.path().join("parked");
    append_index_entry(&data_dir, &entry("original", "before"), None)?;

    let lock = lock_index(&data_dir, None)?;
    std::fs::rename(&data_dir, &parked)?;
    write_index_atomic(&data_dir, &[entry("replacement", "untouched")])?;

    let mut locked_entries = read_index_in(lock.root())?;
    locked_entries
        .first_mut()
        .ok_or("locked index unexpectedly empty")?
        .name = Some("after".to_owned());
    write_index_atomic_in(lock.root(), &locked_entries)?;
    drop(lock);

    let parked_root = PrivateRoot::open(&parked)?;
    let parked_entries = read_index_in(&parked_root)?;
    let replacement_entries = read_index(&data_dir)?;
    assert_eq!(parked_entries.len(), 1);
    let parked_entry = parked_entries
        .first()
        .ok_or("parked index unexpectedly empty")?;
    assert_eq!(parked_entry.id, "original");
    assert_eq!(parked_entry.name.as_deref(), Some("after"));
    assert_eq!(replacement_entries.len(), 1);
    let replacement_entry = replacement_entries
        .first()
        .ok_or("replacement index unexpectedly empty")?;
    assert_eq!(replacement_entry.id, "replacement");
    assert_eq!(replacement_entry.name.as_deref(), Some("untouched"));
    Ok(())
}
