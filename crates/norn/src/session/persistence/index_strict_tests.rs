use std::fs;
use std::io;
use std::sync::{Arc, Barrier};

use chrono::Utc;

use super::*;
use crate::provider::usage::Usage;
use crate::session::persistence::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
};

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
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
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
    }
}

#[test]
fn active_index_always_starts_with_exact_format_two_header()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    append_index_entry(temp.path(), &entry("session-a"), None)?;

    let body = fs::read_to_string(index_file_path(temp.path()))?;
    let first = body
        .lines()
        .next()
        .ok_or_else(|| io::Error::other("index was unexpectedly empty"))?;
    assert_eq!(first, r#"{"norn_session_format":2}"#);
    let rows = read_index(temp.path())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.first().map(|row| row.id.as_str()), Some("session-a"));
    Ok(())
}

#[test]
fn existing_corrupt_or_headerless_index_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        String::new(),
        format!("{}\n", serde_json::to_string(&entry("headerless"))?),
        "{\"norn_session_format\":2}\n\n".to_owned(),
        "{\"norn_session_format\":2,\"extra\":true}\n".to_owned(),
        "{\"norn_session_format\":2}\n{not-json}\n".to_owned(),
    ];
    for body in cases {
        let temp = tempfile::tempdir()?;
        fs::write(index_file_path(temp.path()), body)?;
        let error = read_index(temp.path())
            .err()
            .ok_or_else(|| io::Error::other("corrupt index unexpectedly loaded"))?;
        assert!(matches!(error, SessionPersistError::InvalidIndex(_)));
    }
    Ok(())
}

#[test]
fn atomic_replacement_does_not_silently_repair_a_corrupt_index()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let corrupt = b"{\"norn_session_format\":2}\n{not-json}\n";
    fs::write(index_file_path(temp.path()), corrupt)?;

    let error = write_index_atomic(temp.path(), &[entry("replacement")])
        .err()
        .ok_or_else(|| io::Error::other("corrupt index was silently replaced"))?;
    assert!(matches!(error, SessionPersistError::InvalidIndex(_)));
    assert_eq!(fs::read(index_file_path(temp.path()))?, corrupt);
    Ok(())
}

#[test]
fn duplicate_keys_are_rejected_at_root_and_nested_levels() -> Result<(), Box<dyn std::error::Error>>
{
    let valid = serde_json::to_string(&entry("session-a"))?;
    let root_duplicate = valid.replacen(
        r#""id":"session-a""#,
        r#""id":"session-a","id":"session-b""#,
        1,
    );
    let nested_duplicate = valid.replacen(
        r#""origin":{"kind":"native"}"#,
        r#""origin":{"kind":"native","kind":"native"}"#,
        1,
    );
    for row in [root_duplicate, nested_duplicate] {
        let temp = tempfile::tempdir()?;
        fs::write(
            index_file_path(temp.path()),
            format!("{{\"norn_session_format\":2}}\n{row}\n"),
        )?;
        let error = read_index(temp.path())
            .err()
            .ok_or_else(|| io::Error::other("duplicate-key index unexpectedly loaded"))?;
        assert!(matches!(error, SessionPersistError::InvalidIndex(_)));
    }
    Ok(())
}

#[test]
fn missing_index_is_empty_only_for_absent_or_fresh_store() -> Result<(), Box<dyn std::error::Error>>
{
    let container = tempfile::tempdir()?;
    let absent = container.path().join("absent");
    assert!(read_index(&absent)?.is_empty());

    let fresh = container.path().join("fresh");
    fs::create_dir(&fresh)?;
    assert!(read_index(&fresh)?.is_empty());

    let populated = container.path().join("populated");
    fs::create_dir(&populated)?;
    fs::write(populated.join("orphan.jsonl"), b"artifact\n")?;
    let error = read_index(&populated)
        .err()
        .ok_or_else(|| io::Error::other("store with missing index unexpectedly loaded"))?;
    assert!(matches!(error, SessionPersistError::MissingIndex { .. }));
    Ok(())
}

#[test]
fn exact_uuid_index_temporary_is_reclaimed_before_first_read()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let temporary = format!("index.jsonl.tmp.{}", uuid::Uuid::new_v4().hyphenated());
    fs::write(temp.path().join(&temporary), b"interrupted rewrite")?;

    assert!(read_index(temp.path())?.is_empty());
    assert!(!temp.path().join(temporary).exists());
    Ok(())
}

#[test]
fn exact_uuid_index_temporary_is_reclaimed_before_canonical_read()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    append_index_entry(temp.path(), &entry("session-a"), None)?;
    let temporary = format!("index.jsonl.tmp.{}", uuid::Uuid::new_v4().hyphenated());
    fs::write(temp.path().join(&temporary), b"interrupted rewrite")?;

    let rows = read_index(temp.path())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "session-a");
    assert!(!temp.path().join(temporary).exists());
    Ok(())
}

#[test]
fn owned_index_temporary_name_with_non_file_entry_fails_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    append_index_entry(temp.path(), &entry("session-a"), None)?;
    let temporary = format!("index.jsonl.tmp.{}", uuid::Uuid::new_v4().hyphenated());
    fs::create_dir(temp.path().join(&temporary))?;

    let error = read_index(temp.path())
        .err()
        .ok_or_else(|| io::Error::other("non-file index temporary unexpectedly loaded"))?;
    assert!(matches!(
        error,
        SessionPersistError::IndexArtifactConflict { .. }
    ));
    assert!(temp.path().join(temporary).is_dir());
    Ok(())
}

#[test]
fn concurrent_full_rewrites_do_not_lose_unique_insertions() -> Result<(), Box<dyn std::error::Error>>
{
    const WORKERS: usize = 16;

    let temp = tempfile::tempdir()?;
    let data_dir = Arc::new(temp.path().to_path_buf());
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for worker in 0..WORKERS {
        let data_dir = Arc::clone(&data_dir);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            append_index_entry(&data_dir, &entry(&format!("session-{worker}")), None)
        }));
    }
    for worker in workers {
        worker
            .join()
            .map_err(|_panic| io::Error::other("index insertion worker panicked"))??;
    }

    let entries = read_index(&data_dir)?;
    assert_eq!(entries.len(), WORKERS);
    let body = fs::read_to_string(index_file_path(&data_dir))?;
    assert_eq!(body.matches("norn_session_format").count(), 1);
    assert!(
        !fs::read_dir(&*data_dir)?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("index.jsonl.tmp.")
            })
    );
    Ok(())
}

#[test]
fn concurrent_same_id_insert_converges_on_one_row() -> Result<(), Box<dyn std::error::Error>> {
    const WORKERS: usize = 12;

    let temp = tempfile::tempdir()?;
    let data_dir = Arc::new(temp.path().to_path_buf());
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let data_dir = Arc::clone(&data_dir);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            insert_index_entry_if_absent(&data_dir, &entry("same-session"), None)
        }));
    }
    let mut inserted = 0;
    for worker in workers {
        let previous = worker
            .join()
            .map_err(|_panic| io::Error::other("same-id insertion worker panicked"))??;
        if previous.is_none() {
            inserted += 1;
        }
    }
    assert_eq!(inserted, 1);
    assert_eq!(read_index(&data_dir)?.len(), 1);
    Ok(())
}

#[test]
fn counter_overflow_fails_without_changing_the_index() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut maxed = entry("maxed-session");
    maxed.event_count = u64::MAX;
    append_index_entry(temp.path(), &maxed, None)?;
    let before = fs::read(index_file_path(temp.path()))?;

    let error = update_session_index(temp.path(), &maxed.id, 1, &Usage::default(), None)
        .err()
        .ok_or_else(|| io::Error::other("overflowing index update unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        SessionPersistError::IndexCounterOverflow {
            field: "event_count",
            ..
        }
    ));
    assert_eq!(fs::read(index_file_path(temp.path()))?, before);
    Ok(())
}
