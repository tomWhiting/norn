//! Exact-call change fixtures using the real edit/write/patch output envelopes.

use norn::provider::request::ToolCallKind;
use norn::session_view::{DisplayText, ToolState, ToolView};
use norn::tool::failure::{ToolErrorKind, ToolErrorPayload};
use norn::tool::traits::ToolOutput;
use serde_json::{Value, json};

use super::{AppliedEvidence, ChangeError, ChangeKind, Evidence, Unavailable, inspect_change};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn view(name: &str) -> ToolView {
    ToolView {
        call_id: Some("original-call".to_owned()),
        stream_item_id: None,
        name: Some(DisplayText::new(name)),
        description: Some(DisplayText::new("Original operator-facing description")),
        description_error: None,
        kind: Some(ToolCallKind::Function),
        arguments: None,
        result: None,
        invocation_event: None,
        invocation_attempt: None,
        result_event: None,
        result_parent: None,
        state: ToolState::Completed,
        result_state: Some(ToolState::Completed),
        duration_ms: Some(13),
        committed: None,
    }
}

fn diagnostic_failure(content: Value) -> Result<String, serde_json::Error> {
    let output = ToolOutput::failure_with_content(
        content,
        ToolErrorPayload::new(
            ToolErrorKind::ValidationFailed,
            "recorded syntax diagnostic",
        ),
    );
    serde_json::to_string(&output.content)
}

#[test]
fn edit_preserves_committed_fragment_hash_and_failure_together() -> TestResult {
    let mut tool = view("edit");
    tool.committed = Some(true);
    tool.state = ToolState::Failed;
    tool.result_state = Some(ToolState::Failed);
    let arguments =
        r#"{"path":"src/lib.rs","old_string":"fn old() {}","new_string":"fn (","occurrence":2}"#;
    let result = diagnostic_failure(json!({
        "path":"src/lib.rs", "kind":"edit_committed", "committed":true,
        "after_hash":"recorded-after-hash", "match_count":2,
        "diagnostics":[{"severity":"error","message":"expected identifier"}],
        "blast_radius":{}, "check_overrides":[{"check_name":"ast_validation"}]
    }))?;
    let evidence = inspect_change(&tool, Some(arguments), Some(&result))?;
    assert_eq!(evidence.applied, AppliedEvidence::Committed);
    assert_eq!(evidence.committed, Evidence::Available(true));
    assert_eq!(evidence.state(), ToolState::Failed);
    assert_eq!(evidence.result_state(), Some(ToolState::Failed));
    assert_eq!(
        evidence.error.as_ref().and_then(|value| value.get("kind")),
        Some(&json!("validation_failed"))
    );
    let ChangeKind::Edit {
        old_string,
        new_string,
        occurrence,
        after_hash,
        ..
    } = evidence.change
    else {
        return Err("expected an edit fragment".into());
    };
    assert_eq!(old_string, Evidence::Available("fn old() {}".to_owned()));
    assert_eq!(new_string, Evidence::Available("fn (".to_owned()));
    assert_eq!(occurrence, Evidence::Available(2));
    assert_eq!(
        after_hash,
        Evidence::Available("recorded-after-hash".to_owned())
    );
    assert_eq!(
        evidence.diagnostics,
        Evidence::Available(vec![
            json!({"severity":"error","message":"expected identifier"})
        ])
    );
    Ok(())
}

#[test]
fn blocked_edit_keeps_its_proposal_without_fabricated_after_hash() -> TestResult {
    let mut tool = view("edit");
    tool.state = ToolState::Failed;
    tool.committed = Some(false);
    let result = diagnostic_failure(json!({
        "path":"src/lib.rs", "kind":"edit_blocked_by_ast", "committed":false,
        "diagnostics":[{"severity":"error"}], "blast_radius":{}, "check_overrides":[]
    }))?;
    let evidence = inspect_change(
        &tool,
        Some(r#"{"path":"src/lib.rs","old_string":"old","new_string":"proposed"}"#),
        Some(&result),
    )?;
    assert_eq!(evidence.applied, AppliedEvidence::NotCommitted);
    let ChangeKind::Edit {
        new_string,
        after_hash,
        ..
    } = evidence.change
    else {
        return Err("expected the blocked edit's proposed fragment".into());
    };
    assert_eq!(new_string, Evidence::Available("proposed".to_owned()));
    assert_eq!(
        after_hash,
        Evidence::Unavailable(Unavailable::MissingField("after_hash"))
    );
    Ok(())
}

#[test]
fn permission_block_without_a_mutation_receipt_keeps_proposed_edit_uncommitted() -> TestResult {
    let mut tool = view("edit");
    tool.state = ToolState::Blocked;
    tool.result_state = Some(ToolState::Blocked);
    let output = ToolOutput::failure(ToolErrorPayload::new(
        ToolErrorKind::PermissionDenied,
        "recorded permission refusal",
    ));
    let result = serde_json::to_string(&output.content)?;
    let evidence = inspect_change(
        &tool,
        Some(r#"{"path":"x","old_string":"old","new_string":"proposed"}"#),
        Some(&result),
    )?;
    assert_eq!(evidence.applied, AppliedEvidence::Blocked);
    assert_eq!(evidence.applied.label(), "blocked: not committed");
    assert_eq!(
        evidence.committed,
        Evidence::Unavailable(Unavailable::MissingField("committed"))
    );
    assert!(evidence.error.is_some());
    Ok(())
}

#[test]
fn missing_or_ill_typed_fragments_are_never_empty_strings() -> TestResult {
    let tool = view("edit");
    for arguments in [
        r#"{"path":"x"}"#,
        r#"{"path":"x","old_string":false,"new_string":null}"#,
    ] {
        let evidence = inspect_change(&tool, Some(arguments), None)?;
        let ChangeKind::Edit {
            old_string,
            new_string,
            ..
        } = evidence.change
        else {
            return Err("expected edit evidence".into());
        };
        assert!(matches!(old_string, Evidence::Unavailable(_)));
        assert!(matches!(new_string, Evidence::Unavailable(_)));
    }
    let evidence = inspect_change(&tool, Some(r#"{"old_string":"","new_string":""}"#), None)?;
    let ChangeKind::Edit {
        old_string,
        new_string,
        ..
    } = evidence.change
    else {
        return Err("expected the explicitly empty fragments".into());
    };
    assert_eq!(old_string, Evidence::Available(String::new()));
    assert_eq!(new_string, Evidence::Available(String::new()));
    Ok(())
}

#[test]
fn real_write_receipt_proves_applied_with_diagnostic_error_without_before_content() -> TestResult {
    let mut tool = view("write");
    tool.state = ToolState::Failed;
    tool.result_state = Some(ToolState::Failed);
    let result = diagnostic_failure(json!({
        "path":"src/new.rs", "bytes_written":4, "line_count":1, "length_limit":null,
        "diagnostics":[{"severity":"error","message":"incomplete function"}],
        "check_overrides":[]
    }))?;
    let evidence = inspect_change(
        &tool,
        Some(r#"{"path":"src/new.rs","content":"fn ("}"#),
        Some(&result),
    )?;
    assert_eq!(
        evidence.committed,
        Evidence::Unavailable(Unavailable::MissingField("committed"))
    );
    assert_eq!(
        evidence.applied,
        AppliedEvidence::WriteReceipt {
            path: "src/new.rs".to_owned(),
            bytes_written: 4
        }
    );
    assert!(evidence.error.is_some());
    assert_eq!(evidence.state(), ToolState::Failed);
    let ChangeKind::Write {
        before,
        content,
        bytes_written,
        ..
    } = evidence.change
    else {
        return Err("expected write receipt evidence".into());
    };
    assert_eq!(before, Evidence::Unavailable(Unavailable::NotCaptured));
    assert_eq!(content, Evidence::Available("fn (".to_owned()));
    assert_eq!(bytes_written, Evidence::Available(4));
    Ok(())
}

#[test]
fn write_argument_content_and_success_alone_do_not_establish_application() -> TestResult {
    let tool = view("write");
    for output in [
        None,
        Some("{}"),
        Some(r#"{"bytes_written":"4","path":"x"}"#),
        Some(r#"{"bytes_written":4}"#),
    ] {
        let evidence = inspect_change(&tool, Some(r#"{"path":"x","content":"data"}"#), output)?;
        assert_eq!(evidence.applied, AppliedEvidence::Unknown);
        let ChangeKind::Write { before, .. } = evidence.change else {
            return Err("expected supplied write content".into());
        };
        assert_eq!(before, Evidence::Unavailable(Unavailable::NotCaptured));
    }
    Ok(())
}

#[test]
fn explicit_zero_write_is_evidence_but_does_not_invent_an_empty_before_file() -> TestResult {
    let tool = view("write");
    let result = serde_json::to_string(
        &ToolOutput::success(json!({
            "path":"empty", "bytes_written":0, "line_count":0, "length_limit":null,
            "diagnostics":[], "check_overrides":[]
        }))
        .content,
    )?;
    let evidence = inspect_change(
        &tool,
        Some(r#"{"path":"empty","content":""}"#),
        Some(&result),
    )?;
    assert_eq!(
        evidence.applied,
        AppliedEvidence::WriteReceipt {
            path: "empty".to_owned(),
            bytes_written: 0
        }
    );
    let ChangeKind::Write {
        before, content, ..
    } = evidence.change
    else {
        return Err("expected recorded zero-byte write".into());
    };
    assert_eq!(before, Evidence::Unavailable(Unavailable::NotCaptured));
    assert_eq!(content, Evidence::Available(String::new()));
    Ok(())
}

#[test]
fn patch_staged_hunk_counts_do_not_overrule_not_committed() -> TestResult {
    let mut tool = view("apply_patch");
    tool.committed = Some(false);
    tool.state = ToolState::Failed;
    let patch = "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
    let arguments = serde_json::to_string(&json!({"patch":patch,"working_dir":"historical/root"}))?;
    let result = diagnostic_failure(json!({
        "kind":"patch_blocked_by_ast", "files_modified":[], "files_attempted":["historical/root/file"],
        "hunks_applied":1, "lines_added":1, "lines_removed":1, "committed":false,
        "diagnostics":[{"severity":"error","file":"historical/root/file"}],
        "resolution_details":[], "check_overrides":[], "mode":"auto", "follow_up_id":null
    }))?;
    let evidence = inspect_change(&tool, Some(&arguments), Some(&result))?;
    assert_eq!(evidence.applied, AppliedEvidence::NotCommitted);
    let ChangeKind::Patch {
        supplied_patch,
        files_modified,
        files_attempted,
        ..
    } = evidence.change
    else {
        return Err("expected supplied patch".into());
    };
    assert_eq!(supplied_patch, Evidence::Available(patch.to_owned()));
    assert_eq!(files_modified, Evidence::Available(Vec::new()));
    assert_eq!(
        files_attempted,
        Evidence::Available(vec!["historical/root/file".to_owned()])
    );
    Ok(())
}

#[test]
fn patch_commit_with_errors_preserves_deleted_file_receipt() -> TestResult {
    let mut tool = view("apply_patch");
    tool.state = ToolState::Failed;
    tool.committed = Some(true);
    let result = diagnostic_failure(json!({
        "kind":"patch_committed", "files_modified":["created"], "committed":true,
        "hunks_applied":2, "lines_added":1, "lines_removed":2,
        "per_file":[{"path":"removed","status":"deleted","hunks":1,"lines_added":0,"lines_removed":2},
                    {"path":"created","status":"created","hunks":1,"lines_added":1,"lines_removed":0}],
        "diagnostics":[{"severity":"error","file":"created"}],
        "resolution_details":[], "check_overrides":[], "mode":"auto", "follow_up_id":null
    }))?;
    let evidence = inspect_change(
        &tool,
        Some(
            r#"{"patch":"*** Begin Patch\n*** Delete File: removed\n*** Add File: created\n+bad\n*** End Patch"}"#,
        ),
        Some(&result),
    )?;
    assert_eq!(evidence.applied, AppliedEvidence::Committed);
    assert!(evidence.error.is_some());
    let ChangeKind::Patch {
        per_file: Evidence::Available(files),
        ..
    } = evidence.change
    else {
        return Err("expected exact per-file receipts".into());
    };
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, Evidence::Available("removed".to_owned()));
    assert_eq!(files[0].status, Evidence::Available("deleted".to_owned()));
    Ok(())
}

#[test]
fn submitted_patch_alone_is_intent_and_keeps_both_formats_exact() -> TestResult {
    let tool = view("apply_patch");
    for patch in [
        "--- a\r\n+++ b\r\n",
        "*** Begin Patch\n*** Update File: x\n@@\n-a\n+b\n*** End Patch\n",
    ] {
        let arguments = serde_json::to_string(&json!({"patch":patch}))?;
        let evidence = inspect_change(&tool, Some(&arguments), None)?;
        assert_eq!(evidence.applied, AppliedEvidence::Unknown);
        let ChangeKind::Patch {
            supplied_patch,
            per_file,
            ..
        } = evidence.change
        else {
            return Err("expected a submitted patch".into());
        };
        assert_eq!(supplied_patch, Evidence::Available(patch.to_owned()));
        assert_eq!(per_file, Evidence::Unavailable(Unavailable::Body));
    }
    Ok(())
}

#[test]
fn arbitrary_writer_claims_never_become_structured_filesystem_coverage() -> TestResult {
    for name in ["bash", "mcp__server__edit", "future_tool"] {
        let tool = view(name);
        let arguments = "original argument bytes, not assumed JSON";
        let result = r#"{"exit_code":0,"committed":true,"bytes_written":4,"path":"x"}"#;
        let evidence = inspect_change(&tool, Some(arguments), Some(result))?;
        assert_eq!(evidence.change, ChangeKind::Unknown);
        assert_eq!(evidence.applied, AppliedEvidence::Unknown);
        assert_eq!(evidence.committed, Evidence::Available(true));
        assert_eq!(evidence.call_id.as_deref(), Some("original-call"));
        assert_eq!(evidence.tool_name, tool.name);
    }
    Ok(())
}

#[test]
fn custom_and_orphan_calls_do_not_borrow_a_known_function_schema() -> TestResult {
    for kind in [Some(ToolCallKind::Custom), None] {
        let mut tool = view("edit");
        tool.kind = kind;
        let evidence = inspect_change(&tool, Some("freeform text"), Some(r#"{"committed":true}"#))?;
        assert_eq!(evidence.change, ChangeKind::Unknown);
        assert_eq!(evidence.applied, AppliedEvidence::Unknown);
    }
    Ok(())
}

#[test]
fn absent_bodies_and_conflicting_revisions_remain_explicit() -> TestResult {
    let mut tool = view("edit");
    let evidence = inspect_change(&tool, None, None)?;
    let ChangeKind::Edit {
        old_string,
        new_string,
        ..
    } = evidence.change
    else {
        return Err("expected missing edit evidence".into());
    };
    assert_eq!(old_string, Evidence::Unavailable(Unavailable::Body));
    assert_eq!(new_string, Evidence::Unavailable(Unavailable::Body));
    tool.committed = Some(false);
    assert!(matches!(
        inspect_change(&tool, None, Some(r#"{"committed":true}"#)),
        Err(ChangeError::ConflictingCommit {
            recorded: true,
            metadata: false,
            ..
        })
    ));
    Ok(())
}

#[test]
fn malformed_known_body_is_located_without_quoting_original_bytes() -> TestResult {
    let mut tool = view("edit");
    tool.call_id = Some("call\u{1b}[2J".to_owned());
    let error = inspect_change(&tool, Some("{\"private-secret\": broken"), None)
        .err()
        .ok_or("malformed JSON accepted")?;
    let text = error.to_string();
    assert!(text.contains("arguments JSON is malformed at line 1"));
    assert!(!text.contains("private-secret"));
    assert!(!text.contains('\u{1b}'));
    assert!(matches!(
        inspect_change(&tool, Some("[]"), None),
        Err(ChangeError::NotObject {
            body: "arguments",
            ..
        })
    ));
    assert!(matches!(
        inspect_change(&tool, None, Some("[")),
        Err(ChangeError::MalformedJson { body: "result", .. })
    ));
    Ok(())
}

#[test]
fn exact_supplied_call_evidence_does_not_depend_on_path_existence_or_today_content() -> TestResult {
    let mut first = view("edit");
    first.call_id = Some("earlier-call".to_owned());
    let mut later = view("edit");
    later.call_id = Some("later-call".to_owned());
    let old_args = r#"{"path":"/unavailable/historical/file","old_string":"original","new_string":"intermediate"}"#;
    let later_args = r#"{"path":"/unavailable/historical/file","old_string":"intermediate","new_string":"today"}"#;
    let result = r#"{"committed":true,"after_hash":"recorded-only"}"#;
    let evidence = inspect_change(&first, Some(old_args), Some(result))?;
    let later_evidence = inspect_change(&later, Some(later_args), Some(result))?;
    first.call_id = Some("subsequent-metadata".to_owned());
    assert_eq!(evidence.call_id.as_deref(), Some("earlier-call"));
    assert_eq!(later_evidence.call_id.as_deref(), Some("later-call"));
    assert_ne!(evidence.change, later_evidence.change);
    Ok(())
}
