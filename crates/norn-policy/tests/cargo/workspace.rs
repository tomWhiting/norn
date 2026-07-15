use norn_policy::rust::cargo::{CargoDiagnostic, CargoDiagnosticCode, PackageRoot, discover_cargo};
use norn_policy::{EntryKind, OwnedSnapshot};

use super::support::{TestResult, snapshot, snapshot_bytes, snapshot_with_kind};

#[test]
fn workspace_globs_excludes_and_root_package_are_deterministic() -> TestResult {
    let root = r#"
        [workspace]
        members = ["crates/*", "tools/keep"]
        exclude = ["crates/skip"]

        [workspace.package]
        edition = "2024"

        [package]
        name = "root"
        edition.workspace = true
    "#;
    let member = |name: &str| {
        format!(
            r#"[package]
            name = "{name}"
            edition.workspace = true
        "#
        )
    };
    let crate_a = member("crate-a");
    let skipped = member("skipped");
    let tool = member("tool");
    let snapshot = snapshot(&[
        ("tools/keep/src/lib.rs", ""),
        ("crates/skip/Cargo.toml", &skipped),
        ("Cargo.toml", root),
        ("crates/a/src/lib.rs", ""),
        ("tools/keep/Cargo.toml", &tool),
        ("src/lib.rs", ""),
        ("crates/a/Cargo.toml", &crate_a),
        ("crates/skip/src/lib.rs", ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    let packages: Vec<_> = result
        .packages()
        .iter()
        .map(|package| {
            let root = match package.root() {
                PackageRoot::WorkspaceRoot => ".",
                PackageRoot::Member(path) => path.as_str(),
            };
            (package.name(), root)
        })
        .collect();
    assert_eq!(
        packages,
        [
            ("root", "."),
            ("crate-a", "crates/a"),
            ("tool", "tools/keep"),
        ]
    );
    assert!(
        result
            .packages()
            .iter()
            .all(|package| package.targets().len() == 1)
    );
    Ok(())
}

#[test]
fn malformed_and_unmatched_workspace_patterns_are_closed_diagnostics() -> TestResult {
    let snapshot = snapshot(&[(
        "Cargo.toml",
        r#"[workspace]
            members = ["../outside", "missing/*", 7]
            exclude = [false, "/absolute"]
        "#,
    )])?;

    let result = discover_cargo(&snapshot);
    let codes: Vec<_> = result
        .diagnostics()
        .iter()
        .map(CargoDiagnostic::code)
        .collect();
    assert!(codes.contains(&CargoDiagnosticCode::WorkspacePatternsInvalid));
    assert!(codes.contains(&CargoDiagnosticCode::WorkspacePatternInvalid));
    assert!(codes.contains(&CargoDiagnosticCode::MemberPatternUnmatched));
    assert!(!result.is_valid());
    Ok(())
}

#[test]
fn wildcard_members_never_select_a_virtual_workspace_root() -> TestResult {
    let snapshot = snapshot(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"*\"]"),
        (
            "member/Cargo.toml",
            "[package]\nname = \"member\"\nedition = \"2024\"",
        ),
        ("member/src/lib.rs", ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    assert_eq!(result.packages().len(), 1);
    assert_eq!(result.packages()[0].name(), "member");
    Ok(())
}

#[test]
fn empty_virtual_workspaces_fail_closed() -> TestResult {
    for manifest in [
        "[workspace]",
        "[workspace]\nmembers = []",
        "[workspace]\nmembers = [\"member\"]\nexclude = [\"member\"]",
    ] {
        let snapshot = snapshot(&[
            ("Cargo.toml", manifest),
            (
                "member/Cargo.toml",
                "[package]\nname = \"member\"\nedition = \"2024\"",
            ),
        ])?;
        let result = discover_cargo(&snapshot);
        assert!(
            result
                .diagnostics()
                .iter()
                .any(|item| item.code() == CargoDiagnosticCode::WorkspaceInvalid)
        );
    }
    Ok(())
}

#[test]
fn dot_components_are_normalized_for_members_and_excludes() -> TestResult {
    let snapshot = snapshot(&[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"./crates/*\"]\nexclude = [\"./crates/skip\"]",
        ),
        (
            "crates/keep/Cargo.toml",
            "[package]\nname = \"keep\"\nedition = \"2024\"",
        ),
        (
            "crates/skip/Cargo.toml",
            "[package]\nname = \"skip\"\nedition = \"2024\"",
        ),
        ("crates/keep/src/lib.rs", ""),
    ])?;

    let result = discover_cargo(&snapshot);
    assert!(result.is_valid());
    assert_eq!(result.packages().len(), 1);
    assert_eq!(result.packages()[0].name(), "keep");
    Ok(())
}

#[test]
fn root_manifest_failures_are_distinct() -> TestResult {
    let missing = discover_cargo(&OwnedSnapshot::empty());
    assert_eq!(
        missing.diagnostics()[0].code(),
        CargoDiagnosticCode::RootManifestMissing
    );

    let malformed = discover_cargo(&snapshot(&[("Cargo.toml", "[workspace")])?);
    assert_eq!(
        malformed.diagnostics()[0].code(),
        CargoDiagnosticCode::ManifestMalformed
    );

    let non_utf8 = discover_cargo(&snapshot_bytes(&[("Cargo.toml", &[0xff, 0xfe])])?);
    assert_eq!(
        non_utf8.diagnostics()[0].code(),
        CargoDiagnosticCode::ManifestNotUtf8
    );

    let nonregular = discover_cargo(&snapshot_with_kind(&[(
        "Cargo.toml",
        EntryKind::Symlink,
        "../outside",
    )])?);
    assert_eq!(
        nonregular.diagnostics()[0].code(),
        CargoDiagnosticCode::EntryNotRegular
    );

    let no_workspace = discover_cargo(&snapshot(&[("Cargo.toml", "[package]\nname = \"solo\"")])?);
    assert_eq!(
        no_workspace.diagnostics()[0].code(),
        CargoDiagnosticCode::WorkspaceInvalid
    );
    Ok(())
}

#[test]
fn duplicate_package_names_and_nonregular_member_manifests_fail() -> TestResult {
    let duplicate = snapshot(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]"),
        ("a/Cargo.toml", "[package]\nname = \"same\""),
        ("b/Cargo.toml", "[package]\nname = \"same\""),
    ])?;
    let result = discover_cargo(&duplicate);
    assert!(
        result
            .diagnostics()
            .iter()
            .any(|item| item.code() == CargoDiagnosticCode::DuplicatePackageName)
    );

    let nonregular = snapshot_with_kind(&[
        (
            "Cargo.toml",
            EntryKind::Regular,
            "[workspace]\nmembers = [\"member\"]",
        ),
        (
            "member/Cargo.toml",
            EntryKind::Symlink,
            "../../outside/Cargo.toml",
        ),
    ])?;
    let result = discover_cargo(&nonregular);
    assert!(
        result
            .diagnostics()
            .iter()
            .any(|item| item.code() == CargoDiagnosticCode::EntryNotRegular)
    );
    Ok(())
}
