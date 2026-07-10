//! Debug dumper for raw API requests and responses.
//!
//! When a [`DebugDumper`] is active, every provider call appends
//! structured JSONL entries to a single session-scoped file. Each line
//! is a self-contained JSON object tagged by `type`:
//!
//! - `request` — the full serialized payload sent to the provider.
//! - `response_meta` — HTTP status code and response header names; all values
//!   are redacted because an authority can echo secrets under any name.
//! - `sse_event` — each parsed SSE event with type and data.
//!
//! The dumper is provider-agnostic: any concrete provider can use it to
//! record wire-level traffic for inspection.
//!
//! All file I/O is synchronous (`std::fs`). This is acceptable because
//! debug dumps are an infrequent diagnostic tool; callers should not
//! enable this in high-throughput production paths.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

/// Appends structured JSONL entries to a single debug dump file.
///
/// Multiple API calls within the same session append to the same file,
/// building a chronological log of all wire traffic. The parent
/// directory is created on first use.
pub struct DebugDumper {
    file_path: PathBuf,
}

/// A single JSONL entry written to the debug dump file.
#[derive(Serialize)]
struct DebugEntry<'a> {
    /// Entry type discriminator.
    r#type: &'a str,
    /// ISO-8601 timestamp.
    timestamp: String,
    /// Entry-specific payload.
    #[serde(flatten)]
    payload: DebugPayload<'a>,
}

/// Payload variants for different entry types.
#[derive(Serialize)]
#[serde(untagged)]
enum DebugPayload<'a> {
    /// The full request payload sent to the provider.
    Request {
        /// API endpoint URL.
        endpoint: &'a str,
        /// The serialized request body as a JSON value (not stringified).
        body: serde_json::Value,
    },
    /// HTTP response metadata.
    ResponseMeta {
        /// HTTP status code.
        status: u16,
        /// Response header names paired with redacted values.
        headers: &'a [(String, String)],
    },
    /// A single parsed SSE event.
    SseEvent {
        /// SSE event type (e.g. `response.output_text.delta`).
        event_type: &'a str,
        /// The parsed JSON data payload.
        data: &'a serde_json::Value,
    },
}

impl DebugDumper {
    /// Create a new dumper targeting the given file path.
    ///
    /// Creates the parent directory tree if it does not exist. Returns
    /// `None` if the directory cannot be created, logging a warning.
    #[must_use]
    pub fn new(file_path: &Path) -> Option<Self> {
        if let Some(parent) = file_path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!(
                path = %parent.display(),
                error = %e,
                "cannot create debug dump directory; API dumps disabled",
            );
            return None;
        }

        Some(Self {
            file_path: file_path.to_owned(),
        })
    }

    /// Log the full request payload.
    pub fn write_request(&self, endpoint: &str, body: &str) {
        let body_value = serde_json::from_str(body)
            .unwrap_or_else(|_| serde_json::Value::String(body.to_owned()));

        self.append_entry(
            "request",
            DebugPayload::Request {
                endpoint,
                body: body_value,
            },
        );
    }

    /// Log HTTP response metadata (status code and headers).
    pub fn write_response_meta(&self, status: u16, headers: &[(String, String)]) {
        let sanitized_headers = headers
            .iter()
            .map(|(name, _)| (name.clone(), "[REDACTED]".to_owned()))
            .collect::<Vec<_>>();
        self.append_entry(
            "response_meta",
            DebugPayload::ResponseMeta {
                status,
                headers: &sanitized_headers,
            },
        );
    }

    /// Log a single parsed SSE event.
    pub fn write_sse_event(&self, event_type: &str, data: &serde_json::Value) {
        self.append_entry("sse_event", DebugPayload::SseEvent { event_type, data });
    }

    fn append_entry(&self, entry_type: &str, payload: DebugPayload<'_>) {
        let entry = DebugEntry {
            r#type: entry_type,
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            payload,
        };

        let line = match serde_json::to_string(&entry) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to serialize debug dump entry",
                );
                return;
            }
        };

        match open_append(&self.file_path) {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}") {
                    tracing::warn!(
                        path = %self.file_path.display(),
                        error = %e,
                        "failed to write debug dump entry",
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %self.file_path.display(),
                    error = %e,
                    "failed to open debug dump file for append",
                );
            }
        }
    }
}

fn open_append(path: &Path) -> Result<File, std::io::Error> {
    reject_existing_non_regular_file(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other(
            "debug dump destination is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if file.metadata()?.mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(file)
}

fn reject_existing_non_regular_file(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            std::io::Error::other("debug dump destination is not a regular file"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn response_metadata_redacts_credential_and_redirect_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file)
            .ok_or_else(|| std::io::Error::other("failed to create debug dumper"))?;

        dumper.write_response_meta(
            307,
            &[
                ("Authorization".to_owned(), "bearer-secret".to_owned()),
                ("Set-Cookie".to_owned(), "cookie-secret".to_owned()),
                ("chatgpt-account-id".to_owned(), "account-secret".to_owned()),
                (
                    "x-codex-turn-state".to_owned(),
                    "turn-state-secret".to_owned(),
                ),
                (
                    "location".to_owned(),
                    "https://example.test/?token=location-secret".to_owned(),
                ),
                (
                    "x-request-id".to_owned(),
                    "echoed-api-key-secret".to_owned(),
                ),
            ],
        );

        let rendered = fs::read_to_string(file)?;
        for secret in [
            "bearer-secret",
            "cookie-secret",
            "account-secret",
            "turn-state-secret",
            "location-secret",
            "echoed-api-key-secret",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("[REDACTED]"));
        assert!(rendered.contains("x-request-id"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dump_files_are_private_and_symlinks_are_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let directory = tempfile::tempdir()?;
        let file = directory.path().join("debug.jsonl");
        let opened = open_append(&file)?;
        assert_eq!(opened.metadata()?.mode() & 0o777, 0o600);
        drop(opened);

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))?;
        let reopened = open_append(&file)?;
        assert_eq!(reopened.metadata()?.mode() & 0o777, 0o600);
        drop(reopened);

        let target = directory.path().join("target.jsonl");
        std::fs::write(&target, "unchanged")?;
        let link = directory.path().join("link.jsonl");
        symlink(&target, &link)?;
        assert!(open_append(&link).is_err());
        assert_eq!(std::fs::read_to_string(target)?, "unchanged");
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
        let content = fs::read_to_string(path).unwrap();
        content
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn dumper_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("debug.jsonl");

        let dumper = DebugDumper::new(&file).expect("should create dumper");
        dumper.write_request(
            "https://api.openai.com/v1/responses",
            r#"{"model":"gpt-5"}"#,
        );

        assert!(file.exists());
        let entries = read_jsonl(&file);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["type"], "request");
        assert_eq!(entries[0]["body"]["model"], "gpt-5");
        assert_eq!(
            entries[0]["endpoint"],
            "https://api.openai.com/v1/responses",
        );
    }

    #[test]
    fn request_body_embedded_as_json_not_string() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file).expect("should create dumper");

        dumper.write_request(
            "https://example.com",
            r#"{"model":"gpt-5","input":[{"role":"user","content":"hello"}]}"#,
        );

        let entries = read_jsonl(&file);
        assert!(entries[0]["body"].is_object(), "body must be a JSON object");
        assert_eq!(entries[0]["body"]["input"][0]["content"], "hello");
    }

    #[test]
    fn response_meta_captures_status_and_headers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file).expect("should create dumper");

        dumper.write_response_meta(
            200,
            &[
                ("content-type".to_owned(), "text/event-stream".to_owned()),
                ("x-request-id".to_owned(), "abc123".to_owned()),
            ],
        );

        let entries = read_jsonl(&file);
        assert_eq!(entries[0]["type"], "response_meta");
        assert_eq!(entries[0]["status"], 200);
        assert_eq!(entries[0]["headers"][0][0], "content-type");
        assert_eq!(entries[0]["headers"][0][1], "[REDACTED]");
    }

    #[test]
    fn sse_events_appended_with_type_and_data() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file).expect("should create dumper");

        dumper.write_sse_event(
            "response.output_text.delta",
            &serde_json::json!({"delta": "hello"}),
        );
        dumper.write_sse_event(
            "response.completed",
            &serde_json::json!({"response": {"status": "completed"}}),
        );

        let entries = read_jsonl(&file);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["type"], "sse_event");
        assert_eq!(entries[0]["event_type"], "response.output_text.delta");
        assert_eq!(entries[0]["data"]["delta"], "hello");
        assert_eq!(entries[1]["event_type"], "response.completed");
    }

    #[test]
    fn multiple_api_calls_append_to_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");

        let dumper1 = DebugDumper::new(&file).expect("should create dumper");
        dumper1.write_request("https://api.example.com", r#"{"model":"a"}"#);
        dumper1.write_response_meta(200, &[]);

        let dumper2 = DebugDumper::new(&file).expect("should create dumper");
        dumper2.write_request("https://api.example.com", r#"{"model":"b"}"#);
        dumper2.write_response_meta(200, &[]);

        let entries = read_jsonl(&file);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0]["body"]["model"], "a");
        assert_eq!(entries[2]["body"]["model"], "b");
    }

    #[test]
    fn entries_have_rfc3339_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file).expect("should create dumper");

        dumper.write_request("https://example.com", r#"{"model":"x"}"#);

        let entries = read_jsonl(&file);
        let ts = entries[0]["timestamp"].as_str().unwrap();
        assert!(ts.contains('T'), "timestamp must be ISO-8601: {ts}");
        assert!(ts.ends_with('Z'), "timestamp must be UTC: {ts}");
    }

    #[test]
    fn invalid_json_body_stored_as_string() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("debug.jsonl");
        let dumper = DebugDumper::new(&file).expect("should create dumper");

        dumper.write_request("https://example.com", "not valid json");

        let entries = read_jsonl(&file);
        assert_eq!(entries[0]["body"], "not valid json");
    }

    #[cfg(unix)]
    #[test]
    fn dumper_returns_none_for_unwritable_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let readonly = dir.path().join("readonly");
        fs::create_dir(&readonly).unwrap();
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o444)).unwrap();

        let file = readonly.join("subdir").join("debug.jsonl");
        let result = DebugDumper::new(&file);
        assert!(
            result.is_none(),
            "creating a file inside a read-only directory must fail",
        );

        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
