//! Read-only exact-call evidence from approved recorded bodies, never current filesystem diffs.

use norn::provider::request::ToolCallKind;
use norn::session_view::{DisplayText, ToolState, ToolView};
use serde_json::Value;

/// A recorded fact or an explicit reason that the fact cannot be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence<T> {
    /// Exact decoded value, including a legitimately empty string or array.
    Available(T),
    /// Missing evidence must never become an empty baseline or a zero count.
    Unavailable(Unavailable),
}

/// Why a requested fact is unavailable in this call's recorded evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The caller has not supplied a complete approved body.
    Body,
    /// The supplied object has no such field.
    MissingField(&'static str),
    /// A recorded field exists but has a different type.
    InvalidField {
        /// Exact field being interpreted.
        field: &'static str,
        /// Required evidence shape.
        expected: &'static str,
    },
    /// This tool does not capture the requested evidence.
    NotCaptured,
}

/// A result's application evidence, independent of diagnostics and lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppliedEvidence {
    /// The exact result explicitly says committed=true.
    Committed,
    /// The exact result explicitly says committed=false.
    NotCommitted,
    /// The retained lifecycle establishes a permission block before execution.
    Blocked,
    /// Current write output supplies this receipt only after the atomic write.
    /// It does not contain an explicit committed field or a before snapshot.
    WriteReceipt {
        /// Recorded result path, not resolved against today's working directory.
        path: String,
        /// Recorded bytes written, including an explicitly recorded zero.
        bytes_written: u64,
    },
    /// A call or successful process exit alone cannot prove a filesystem change.
    Unknown,
}

impl AppliedEvidence {
    /// Honest scope wording; callers display diagnostics separately.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::NotCommitted => "not committed",
            Self::Blocked => "blocked: not committed",
            Self::WriteReceipt { .. } => "applied: recorded write receipt",
            Self::Unknown => "application evidence unavailable",
        }
    }
}

/// Per-file facts emitted by `apply_patch`, with no inferred rename/baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchFile {
    /// Exact recorded path.
    pub path: Evidence<String>,
    /// Exact recorded status, including future values; never inferred from text.
    pub status: Evidence<String>,
}

/// A change proposal or result tied to one call; none is a whole-session diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Supplied edit fragment, not a whole-file before/after comparison.
    Edit {
        /// Exact argument path.
        path: Evidence<String>,
        /// Exact result path, retained independently of the argument path.
        result_path: Evidence<String>,
        /// Exact supplied fragment before replacement.
        old_string: Evidence<String>,
        /// Exact supplied replacement fragment.
        new_string: Evidence<String>,
        /// Supplied occurrence selector; absence is not an invented first match.
        occurrence: Evidence<u64>,
        /// Recorded post-commit hash; never calculated from today's disk.
        after_hash: Evidence<String>,
    },
    /// Submitted write content with no captured before-file baseline.
    Write {
        /// Exact argument path.
        path: Evidence<String>,
        /// Exact result path.
        result_path: Evidence<String>,
        /// Submitted full content, not a reread of the resulting file.
        content: Evidence<String>,
        /// Always unavailable for the current write receipt schema.
        before: Evidence<String>,
        /// Recorded write receipt count, distinct from submitted content length.
        bytes_written: Evidence<u64>,
    },
    /// Original supplied patch; applying/parsing it against files is forbidden here.
    Patch {
        /// Exact supplied patch text, including its format and whitespace.
        supplied_patch: Evidence<String>,
        /// Optional invocation directory, not today's process directory.
        working_dir: Evidence<String>,
        /// Actual per-file receipts, including deletions omitted by `files_modified`.
        per_file: Evidence<Vec<PatchFile>>,
        /// Result's modified-file list, not an exhaustive mutation claim.
        files_modified: Evidence<Vec<String>>,
        /// Result's attempted-file list, even when no change committed.
        files_attempted: Evidence<Vec<String>>,
    },
    /// Bash, MCP, unknown/custom calls and orphan invocation kinds have no
    /// structured filesystem receipt coverage here. Original bodies remain available.
    Unknown,
}

/// Owned compact evidence for a caller-owned exact item/body revision.
///
/// The caller retains source/agent/`ItemId` and approved body capabilities with
/// this value. Original argument/result bytes remain in that body's demand cache;
/// they are not copied into this parsed view. Unknown raw detail remains accessible
/// through those same capabilities, without invented structured mutation coverage.
#[derive(Debug)]
pub struct ChangeEvidence {
    /// Actual call identity, never a stream item ID or a fabricated value.
    pub call_id: Option<String>,
    /// Original terminal-safe tool name, explicitly absent when unknown.
    pub tool_name: Option<DisplayText>,
    /// Observed lifecycle at the exact parsed revision.
    state: ToolState,
    /// Observed result outcome despite incomplete invocation coverage.
    result_state: Option<ToolState>,
    /// Known structured proposal/receipt coverage, or explicitly unknown coverage.
    pub change: ChangeKind,
    /// Explicit recorded flag, preserved separately from inferred write-receipt evidence.
    pub committed: Evidence<bool>,
    /// What this result establishes about application, independent of failure.
    pub applied: AppliedEvidence,
    /// Non-null recorded error payload, including legacy strings and unknown shapes.
    pub error: Option<Value>,
    /// Original diagnostic entries, without dropping errors from committed changes.
    pub diagnostics: Evidence<Vec<Value>>,
}

impl ChangeEvidence {
    /// Preserve the lifecycle even when no complete body has been demanded.
    #[must_use]
    pub fn state(&self) -> ToolState {
        self.state
    }

    /// Result outcome can remain known when invocation coverage is incomplete.
    #[must_use]
    pub fn result_state(&self) -> Option<ToolState> {
        self.result_state
    }
}

/// Refusal to interpret malformed or contradictory recorded evidence.
#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    /// Error text names location without quoting potentially sensitive body bytes.
    #[error("tool call {call}: {body} JSON is malformed at line {line}, column {column}")]
    MalformedJson {
        /// Escaped actual call ID, or explicitly unavailable identity.
        call: String,
        /// Argument or result body.
        body: &'static str,
        /// Parser-reported line.
        line: usize,
        /// Parser-reported column.
        column: usize,
    },
    /// A known function's argument envelope must be a JSON object.
    #[error("tool call {call}: {body} evidence must be a JSON object")]
    NotObject {
        /// Escaped call identity.
        call: String,
        /// Body with the wrong top-level shape.
        body: &'static str,
    },
    /// Caller metadata and supplied body cannot describe different result revisions.
    #[error(
        "tool call {call}: recorded committed flag {recorded} conflicts with metadata {metadata}"
    )]
    ConflictingCommit {
        /// Escaped call identity.
        call: String,
        /// Flag from the exact supplied result.
        recorded: bool,
        /// Flag from retained tool metadata.
        metadata: bool,
    },
}

/// Interpret complete approved recorded bodies off the render/input path.
///
/// `None` means unavailable or not fully loaded, never an empty body. The caller
/// validates source/revision and completeness before supplying bytes. Spools hold
/// the same serialized output JSON as inline results; no wrapper is guessed or
/// recursively decoded here. Unknown/custom arguments remain raw strings.
///
/// # Errors
/// Returns a located error for malformed JSON, wrong known argument shape or a
/// result commitment inconsistent with retained metadata. Raw bytes stay caller-owned.
pub fn inspect_change(
    tool: &ToolView,
    arguments: Option<&str>,
    result: Option<&str>,
) -> Result<ChangeEvidence, ChangeError> {
    let name = tool.name.as_ref().map(DisplayText::as_str);
    let known = tool.kind == Some(ToolCallKind::Function)
        && matches!(name, Some("edit" | "write" | "apply_patch"));
    let args = if known {
        parse_body(tool, arguments, "arguments", true)?
    } else {
        None
    };
    let output = parse_body(tool, result, "result", known)?;
    let args = args.as_ref();
    let output = output.as_ref();
    let change = match (known, name) {
        (true, Some("edit")) => ChangeKind::Edit {
            path: string(args, "path"),
            result_path: string(output, "path"),
            old_string: string(args, "old_string"),
            new_string: string(args, "new_string"),
            occurrence: unsigned(args, "occurrence"),
            after_hash: string(output, "after_hash"),
        },
        (true, Some("write")) => ChangeKind::Write {
            path: string(args, "path"),
            result_path: string(output, "path"),
            content: string(args, "content"),
            before: Evidence::Unavailable(Unavailable::NotCaptured),
            bytes_written: unsigned(output, "bytes_written"),
        },
        (true, Some("apply_patch")) => ChangeKind::Patch {
            supplied_patch: string(args, "patch"),
            working_dir: string(args, "working_dir"),
            per_file: field(output, "per_file", "array of objects", patch_files),
            files_modified: field(output, "files_modified", "array of strings", strings),
            files_attempted: field(output, "files_attempted", "array of strings", strings),
        },
        _ => ChangeKind::Unknown,
    };
    let committed = field(output, "committed", "boolean", Value::as_bool);
    if let Evidence::Available(recorded) = &committed
        && let Some(metadata) = tool.committed
        && *recorded != metadata
    {
        return Err(ChangeError::ConflictingCommit {
            call: call_label(tool),
            recorded: *recorded,
            metadata,
        });
    }
    let applied = application(tool, &change, &committed);
    Ok(ChangeEvidence {
        call_id: tool.call_id.clone(),
        tool_name: tool.name.clone(),
        state: tool.state,
        result_state: tool.result_state,
        change,
        committed,
        applied,
        error: output
            .and_then(|value| value.get("error"))
            .filter(|value| !value.is_null())
            .cloned(),
        diagnostics: field(output, "diagnostics", "array", |value| {
            value.as_array().cloned()
        }),
    })
}

fn application(
    tool: &ToolView,
    change: &ChangeKind,
    committed: &Evidence<bool>,
) -> AppliedEvidence {
    if matches!(change, ChangeKind::Unknown) {
        return AppliedEvidence::Unknown;
    }
    match committed {
        Evidence::Available(true) => AppliedEvidence::Committed,
        Evidence::Available(false) => AppliedEvidence::NotCommitted,
        Evidence::Unavailable(_)
            if tool.state == ToolState::Blocked
                || tool.result_state == Some(ToolState::Blocked) =>
        {
            AppliedEvidence::Blocked
        }
        Evidence::Unavailable(Unavailable::MissingField("committed")) => match change {
            ChangeKind::Write {
                result_path: Evidence::Available(path),
                bytes_written: Evidence::Available(bytes_written),
                ..
            } => AppliedEvidence::WriteReceipt {
                path: path.clone(),
                bytes_written: *bytes_written,
            },
            _ => AppliedEvidence::Unknown,
        },
        Evidence::Unavailable(_) => AppliedEvidence::Unknown,
    }
}

fn parse_body(
    tool: &ToolView,
    body: Option<&str>,
    label: &'static str,
    object: bool,
) -> Result<Option<Value>, ChangeError> {
    body.map(|text| {
        let value: Value =
            serde_json::from_str(text).map_err(|error| ChangeError::MalformedJson {
                call: call_label(tool),
                body: label,
                line: error.line(),
                column: error.column(),
            })?;
        if object && !value.is_object() {
            return Err(ChangeError::NotObject {
                call: call_label(tool),
                body: label,
            });
        }
        Ok(value)
    })
    .transpose()
}

fn call_label(tool: &ToolView) -> String {
    tool.call_id.as_deref().map_or_else(
        || "unavailable".to_owned(),
        |call| {
            DisplayText::new(call)
                .as_str()
                .replace('\n', "\\n")
                .replace('\t', "\\t")
        },
    )
}

fn field<T>(
    object: Option<&Value>,
    name: &'static str,
    expected: &'static str,
    decode: impl FnOnce(&Value) -> Option<T>,
) -> Evidence<T> {
    let Some(object) = object else {
        return Evidence::Unavailable(Unavailable::Body);
    };
    let Some(value) = object.get(name) else {
        return Evidence::Unavailable(Unavailable::MissingField(name));
    };
    decode(value).map_or(
        Evidence::Unavailable(Unavailable::InvalidField {
            field: name,
            expected,
        }),
        Evidence::Available,
    )
}

fn string(object: Option<&Value>, name: &'static str) -> Evidence<String> {
    field(object, name, "string", |value| {
        value.as_str().map(str::to_owned)
    })
}

fn unsigned(object: Option<&Value>, name: &'static str) -> Evidence<u64> {
    field(object, name, "unsigned integer", Value::as_u64)
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn patch_files(value: &Value) -> Option<Vec<PatchFile>> {
    value
        .as_array()?
        .iter()
        .map(|value| {
            value.as_object()?;
            Some(PatchFile {
                path: string(Some(value), "path"),
                status: string(Some(value), "status"),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "changes_tests.rs"]
mod tests;
