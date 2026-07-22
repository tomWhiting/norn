use std::path::Path;

use crate::rules::source::RuleOrigin;

use super::scanner::scan_rule_dirs_with_origins;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn write_rule(directory: &Path, id: &str) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(directory)?;
    std::fs::write(
        directory.join(format!("{id}.md")),
        format!("---\nglobs: \"**/*.{id}\"\norigin: operator\n---\n{id} body\n"),
    )
}

#[test]
fn discovery_preserves_each_actual_workspace_directory_index() -> TestResult {
    let workspace = tempfile::tempdir()?;
    let user = tempfile::tempdir()?;
    let workspace_root = workspace.path().canonicalize()?;
    let project_rules = workspace_root.join(".norn/rules");
    let claude_rules = workspace_root.join(".claude/rules");
    let meridian_rules = workspace_root.join(".meridian/rules");
    let user_rules = user.path().join("rules");

    write_rule(&project_rules, "project")?;
    write_rule(&user_rules, "operator")?;
    write_rule(&claude_rules, "claude")?;
    write_rule(&meridian_rules, "meridian")?;

    let directories = vec![project_rules, user_rules, claude_rules, meridian_rules];
    let scanned = scan_rule_dirs_with_origins(&directories, &workspace_root, &[0, 2, 3]);
    // Every fixture attempts to claim operator origin in frontmatter. Origin
    // is not part of Rule and must still come only from the directory index.
    for (id, index, origin) in [
        ("project", 0, RuleOrigin::Workspace),
        ("operator", 1, RuleOrigin::Operator),
        ("claude", 2, RuleOrigin::Workspace),
        ("meridian", 3, RuleOrigin::Workspace),
    ] {
        let entry = scanned
            .iter()
            .find(|entry| entry.rule.id.as_str() == id)
            .ok_or_else(|| std::io::Error::other(format!("missing {id} rule")))?;
        assert_eq!(entry.directory_index, index);
        assert_eq!(entry.origin, origin);
    }
    Ok(())
}
