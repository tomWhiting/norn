use std::error::Error;

use crate::tool::context::{SharedWorkingDir, ToolContext};

use super::{context_roots, qualified_tool_name};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn provider_names_are_safe_bounded_and_pair_qualified() {
    let first = qualified_tool_name("a", "b__c");
    let second = qualified_tool_name("a__b", "c");
    let punctuation = qualified_tool_name("docs", "read.file/with spaces");

    assert_ne!(first, second);
    assert!(punctuation.len() <= 64);
    assert!(
        punctuation
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    );
}

#[test]
fn context_roots_follow_each_agents_workspace_and_live_working_directory() -> TestResult {
    let temp = tempfile::tempdir()?;
    let child = temp.path().join("child");
    std::fs::create_dir(&child)?;
    let mut context = ToolContext::with_working_dir(SharedWorkingDir::new(child));
    context.confine_to_workspace(temp.path().to_path_buf());

    let roots = context_roots(&context)?;

    assert_eq!(roots.len(), 2);
    assert!(roots[0].uri().starts_with("file:"));
    assert!(roots[1].uri().starts_with("file:"));
    assert_eq!(roots[1].name(), Some("child"));
    Ok(())
}

#[test]
fn identical_workspace_and_working_directory_are_advertised_once() -> TestResult {
    let temp = tempfile::tempdir()?;
    let mut context =
        ToolContext::with_working_dir(SharedWorkingDir::new(temp.path().to_path_buf()));
    context.confine_to_workspace(temp.path().to_path_buf());

    let roots = context_roots(&context)?;

    assert_eq!(roots.len(), 1);
    Ok(())
}
