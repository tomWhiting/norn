use std::ffi::OsStr;
use std::path::PathBuf;

use uuid::Uuid;

pub(super) const JOURNAL_PREFIX: &str = ".norn-publication-";
pub(super) const JOURNAL_SUFFIX: &str = ".json";
pub(super) const JOURNAL_TEMP_PREFIX: &str = ".norn-publication-journal-";
pub(super) const JOURNAL_TEMP_SUFFIX: &str = ".tmp";
pub(super) const TIMELINE_STAGE_PREFIX: &str = ".norn-publication-timeline-";
pub(super) const TIMELINE_STAGE_SUFFIX: &str = ".stage";
pub(super) const AUDIO_STAGE_PREFIX: &str = ".norn-publication-audio-";
pub(super) const AUDIO_STAGE_SUFFIX: &str = ".stage";

pub(super) fn journal_path(transaction_id: &str) -> PathBuf {
    PathBuf::from(format!("{JOURNAL_PREFIX}{transaction_id}{JOURNAL_SUFFIX}"))
}

pub(super) fn journal_temp_path(transaction_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "{JOURNAL_TEMP_PREFIX}{transaction_id}{JOURNAL_TEMP_SUFFIX}"
    ))
}

pub(super) fn timeline_stage_path(transaction_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "{TIMELINE_STAGE_PREFIX}{transaction_id}{TIMELINE_STAGE_SUFFIX}"
    ))
}

pub(super) fn audio_stage_path(transaction_id: &str) -> PathBuf {
    PathBuf::from(format!(
        "{AUDIO_STAGE_PREFIX}{transaction_id}{AUDIO_STAGE_SUFFIX}"
    ))
}

pub(super) fn journal_id(name: &OsStr) -> Option<String> {
    owned_uuid(name, JOURNAL_PREFIX, JOURNAL_SUFFIX)
}

pub(super) fn journal_temp_id(name: &OsStr) -> Option<String> {
    owned_uuid(name, JOURNAL_TEMP_PREFIX, JOURNAL_TEMP_SUFFIX)
}

pub(super) fn timeline_stage_id(name: &OsStr) -> Option<String> {
    owned_uuid(name, TIMELINE_STAGE_PREFIX, TIMELINE_STAGE_SUFFIX)
}

pub(super) fn audio_stage_id(name: &OsStr) -> Option<String> {
    owned_uuid(name, AUDIO_STAGE_PREFIX, AUDIO_STAGE_SUFFIX)
}

fn owned_uuid(name: &OsStr, prefix: &str, suffix: &str) -> Option<String> {
    let value = name.to_str()?;
    let raw = value.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let parsed = Uuid::parse_str(raw).ok()?;
    let canonical = parsed.hyphenated().to_string();
    (canonical == raw).then_some(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_namespace_inventory_is_exact() {
        let id = "9aa3be78-9661-4fab-953f-58f88d5a8f25";
        assert_eq!(
            journal_path(id).to_string_lossy(),
            format!("{JOURNAL_PREFIX}{id}{JOURNAL_SUFFIX}")
        );
        assert_eq!(
            journal_temp_path(id).to_string_lossy(),
            format!("{JOURNAL_TEMP_PREFIX}{id}{JOURNAL_TEMP_SUFFIX}")
        );
        assert_eq!(
            timeline_stage_path(id).to_string_lossy(),
            format!("{TIMELINE_STAGE_PREFIX}{id}{TIMELINE_STAGE_SUFFIX}")
        );
        assert_eq!(
            audio_stage_path(id).to_string_lossy(),
            format!("{AUDIO_STAGE_PREFIX}{id}{AUDIO_STAGE_SUFFIX}")
        );
        assert!(journal_id(OsStr::new(".norn-publication-not-a-uuid.json")).is_none());
    }
}
