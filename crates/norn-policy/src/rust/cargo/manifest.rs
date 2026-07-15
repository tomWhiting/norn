//! Manifest parsing and workspace membership over snapshot data.

use std::collections::BTreeSet;

use globset::{GlobBuilder, GlobMatcher};
use toml::{Table, Value};

use super::{CargoDiagnostic, CargoDiagnosticCode, PackageRoot, diagnostic};
use crate::{EntryKind, OwnedSnapshot, RepositoryPath};

pub(super) fn parse_manifest(
    snapshot: &OwnedSnapshot,
    manifest: &RepositoryPath,
    is_root: bool,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> Option<Value> {
    let Some(entry) = snapshot.get(manifest) else {
        let code = if is_root {
            CargoDiagnosticCode::RootManifestMissing
        } else {
            CargoDiagnosticCode::PackageInvalid
        };
        diagnostics.push(diagnostic(code, manifest, None, None));
        return None;
    };
    if entry.kind() != EntryKind::Regular {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::EntryNotRegular,
            manifest,
            None,
            None,
        ));
        return None;
    }
    let Ok(text) = std::str::from_utf8(entry.bytes()) else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::ManifestNotUtf8,
            manifest,
            None,
            None,
        ));
        return None;
    };
    let Ok(value) = toml::from_str(text) else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::ManifestMalformed,
            manifest,
            None,
            None,
        ));
        return None;
    };
    Some(value)
}

pub(super) fn select_members(
    snapshot: &OwnedSnapshot,
    root: &Value,
    workspace: &Table,
    root_manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> BTreeSet<PackageRoot> {
    let mut roots = BTreeSet::new();
    if root.get("package").is_some() {
        roots.insert(PackageRoot::WorkspaceRoot);
    }
    let candidates = manifest_roots(snapshot);
    add_members(
        &mut roots,
        &candidates,
        read_patterns(workspace, "members", root_manifest, diagnostics),
        root_manifest,
        diagnostics,
    );
    apply_excludes(
        &mut roots,
        read_patterns(workspace, "exclude", root_manifest, diagnostics),
        root_manifest,
        diagnostics,
    );
    if roots.is_empty() {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::WorkspaceInvalid,
            root_manifest,
            None,
            None,
        ));
    }
    roots
}

fn add_members(
    roots: &mut BTreeSet<PackageRoot>,
    candidates: &[PackageRoot],
    patterns: Vec<(usize, String)>,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) {
    for (ordinal, raw) in patterns {
        let Some(pattern_matcher) = compile_pattern(&raw) else {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::WorkspacePatternInvalid,
                manifest,
                None,
                Some(ordinal),
            ));
            continue;
        };
        let mut found = false;
        for candidate in candidates
            .iter()
            .filter(|candidate| pattern_matcher.is_match(root_text(candidate)))
        {
            roots.insert(candidate.clone());
            found = true;
        }
        if !found {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::MemberPatternUnmatched,
                manifest,
                None,
                Some(ordinal),
            ));
        }
    }
}

fn apply_excludes(
    roots: &mut BTreeSet<PackageRoot>,
    patterns: Vec<(usize, String)>,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) {
    for (ordinal, raw) in patterns {
        let Some(matcher) = compile_pattern(&raw) else {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::WorkspacePatternInvalid,
                manifest,
                None,
                Some(ordinal),
            ));
            continue;
        };
        roots.retain(|candidate| {
            candidate == &PackageRoot::WorkspaceRoot || !matcher.is_match(root_text(candidate))
        });
    }
}

fn read_patterns(
    table: &Table,
    key: &str,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> Vec<(usize, String)> {
    let Some(value) = table.get(key) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::WorkspacePatternsInvalid,
            manifest,
            None,
            None,
        ));
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(ordinal, item)| {
            if let Some(raw) = item.as_str() {
                Some((ordinal, raw.to_owned()))
            } else {
                diagnostics.push(diagnostic(
                    CargoDiagnosticCode::WorkspacePatternsInvalid,
                    manifest,
                    None,
                    Some(ordinal),
                ));
                None
            }
        })
        .collect()
}

fn compile_pattern(raw: &str) -> Option<GlobMatcher> {
    let normalized = normalize_pattern(raw)?;
    let Ok(glob) = GlobBuilder::new(&normalized)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
    else {
        return None;
    };
    Some(glob.compile_matcher())
}

fn normalize_pattern(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "." => {}
            "" | ".." => return None,
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        Some(".".to_owned())
    } else {
        Some(parts.join("/"))
    }
}

fn manifest_roots(snapshot: &OwnedSnapshot) -> Vec<PackageRoot> {
    snapshot
        .iter()
        .filter_map(|(path, _)| {
            let root = path.as_str().strip_suffix("/Cargo.toml")?;
            let Ok(root) = RepositoryPath::parse(root) else {
                return None;
            };
            Some(PackageRoot::Member(root))
        })
        .collect()
}

pub(super) fn inherited_edition<'a>(
    workspace: &'a Table,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> Option<&'a str> {
    let value = workspace.get("package")?;
    let Some(package) = value.as_table() else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::PackageOptionInvalid,
            manifest,
            None,
            None,
        ));
        return None;
    };
    match package.get("edition") {
        Some(Value::String(edition)) => Some(edition),
        Some(_) => {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::PackageOptionInvalid,
                manifest,
                None,
                None,
            ));
            None
        }
        None => None,
    }
}

pub(super) fn automatic_by_default(
    package: &Table,
    inherited: Option<&str>,
    manual: bool,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> bool {
    let edition = match package.get("edition") {
        Some(Value::String(value)) => Some(value.as_str()),
        Some(Value::Table(table))
            if table.get("workspace").and_then(Value::as_bool) == Some(true) =>
        {
            if inherited.is_none() {
                diagnostics.push(diagnostic(
                    CargoDiagnosticCode::PackageOptionInvalid,
                    manifest,
                    None,
                    None,
                ));
            }
            inherited
        }
        Some(_) => {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::PackageOptionInvalid,
                manifest,
                None,
                None,
            ));
            None
        }
        None => Some("2015"),
    };
    match edition {
        Some("2015") => !manual,
        Some("2018" | "2021" | "2024") => true,
        Some(_) => {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::PackageOptionInvalid,
                manifest,
                None,
                None,
            ));
            false
        }
        None => false,
    }
}

pub(super) fn option_bool(
    table: &Table,
    key: &str,
    default: bool,
    manifest: &RepositoryPath,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> bool {
    match table.get(key) {
        Some(Value::Boolean(value)) => *value,
        Some(_) => {
            diagnostics.push(diagnostic(
                CargoDiagnosticCode::PackageOptionInvalid,
                manifest,
                None,
                None,
            ));
            false
        }
        None => default,
    }
}

pub(super) fn manifest_path(root: &PackageRoot) -> Option<RepositoryPath> {
    let raw = match root {
        PackageRoot::WorkspaceRoot => "Cargo.toml".to_owned(),
        PackageRoot::Member(root) => format!("{root}/Cargo.toml"),
    };
    let Ok(path) = RepositoryPath::parse(raw) else {
        return None;
    };
    Some(path)
}

fn root_text(root: &PackageRoot) -> &str {
    match root {
        PackageRoot::WorkspaceRoot => ".",
        PackageRoot::Member(path) => path.as_str(),
    }
}
