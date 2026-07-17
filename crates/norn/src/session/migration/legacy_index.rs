use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::session::persistence::SessionStatus;
use crate::session::persistence::io::ensure_session_id_path_safe;
use crate::util::validate_private_component;

/// Exact index-row shape written by legacy format-0/1 runtimes.
///
/// This type is intentionally migration-local. Normal runtime code accepts only
/// the canonical format-2 [`crate::session::SessionIndexEntry`].
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacySessionIndexEntry {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) model: String,
    pub(super) working_dir: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) event_count: u64,
    pub(super) status: SessionStatus,
    #[serde(default)]
    pub(super) format_version: u32,
    #[serde(default)]
    pub(super) total_input_tokens: u64,
    #[serde(default)]
    pub(super) total_output_tokens: u64,
    #[serde(default)]
    pub(super) total_cache_read_tokens: u64,
    #[serde(default)]
    pub(super) rel_path: Option<String>,
    #[serde(default)]
    pub(super) parent_id: Option<String>,
}

pub(super) fn timeline_path(entry: &LegacySessionIndexEntry) -> Result<PathBuf, String> {
    ensure_session_id_path_safe(&entry.id).map_err(|error| error.to_string())?;
    let Some(relative) = entry.rel_path.as_deref() else {
        return Ok(PathBuf::from(format!("{}.jsonl", entry.id)));
    };
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let valid = matches!(components.as_slice(), [
        Component::Normal(root),
        Component::Normal(children),
        Component::Normal(file),
    ] if children == &std::ffi::OsStr::new("children")
        && component_is_safe(root)
        && component_is_safe(file)
        && Path::new(file).extension().is_some_and(|extension| extension == "jsonl"));
    if !valid {
        return Err(
            "indexed rel_path must have the safe '<root>/children/<file>.jsonl' shape".to_owned(),
        );
    }
    Ok(PathBuf::from(relative))
}

fn component_is_safe(component: &std::ffi::OsStr) -> bool {
    component
        .to_str()
        .is_some_and(|value| validate_private_component(value, "session path component").is_ok())
}
