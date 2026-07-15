//! Cargo compilation-root discovery.

use std::collections::{BTreeMap, BTreeSet};

use toml::{Table, Value};

use super::{
    CargoDiagnostic, CargoDiagnosticCode, CargoTarget, CargoTargetKind, PackageRoot, TargetClass,
    diagnostic,
};
use crate::{EntryKind, OwnedSnapshot, RepositoryPath};

pub(super) struct TargetBuilder<'a> {
    snapshot: &'a OwnedSnapshot,
    root: PackageRoot,
    package: &'a str,
    manifest: RepositoryPath,
    diagnostics: &'a mut Vec<CargoDiagnostic>,
    targets: Vec<CargoTarget>,
    seen: BTreeSet<(CargoTargetKind, String)>,
}

impl<'a> TargetBuilder<'a> {
    pub(super) fn new(
        snapshot: &'a OwnedSnapshot,
        root: PackageRoot,
        package: &'a str,
        manifest: RepositoryPath,
        diagnostics: &'a mut Vec<CargoDiagnostic>,
    ) -> Self {
        Self {
            snapshot,
            root,
            package,
            manifest,
            diagnostics,
            targets: Vec::new(),
            seen: BTreeSet::new(),
        }
    }

    pub(super) fn library(&mut self, value: Option<&Value>, enabled: bool) {
        let default_name = self.package.replace('-', "_");
        let Some(value) = value else {
            self.auto_single(
                "src/lib.rs",
                CargoTargetKind::Library,
                TargetClass::Production,
                &default_name,
                enabled,
            );
            return;
        };
        let Some(table) = value.as_table() else {
            self.problem(CargoDiagnosticCode::TargetInvalid, None, None);
            return;
        };
        let Some(name) = optional_string(table, "name", &default_name) else {
            self.problem(CargoDiagnosticCode::TargetInvalid, None, None);
            return;
        };
        let Some(proc_macro) = proc_macro(table) else {
            self.problem(CargoDiagnosticCode::TargetInvalid, None, None);
            return;
        };
        let kind = if proc_macro {
            CargoTargetKind::ProcMacro
        } else {
            CargoTargetKind::Library
        };
        self.explicit_one(
            table,
            kind,
            TargetClass::Production,
            name,
            Some("src/lib.rs"),
            None,
        );
    }

    pub(super) fn explicit(
        &mut self,
        manifest: &Value,
        key: &str,
        kind: CargoTargetKind,
        class: TargetClass,
    ) {
        let Some(value) = manifest.get(key) else {
            return;
        };
        let Some(items) = value.as_array() else {
            self.problem(CargoDiagnosticCode::TargetInvalid, Some(kind), None);
            return;
        };
        for (ordinal, item) in items.iter().enumerate() {
            let Some(table) = item.as_table() else {
                self.problem(
                    CargoDiagnosticCode::TargetInvalid,
                    Some(kind),
                    Some(ordinal),
                );
                continue;
            };
            let Some(name) = table.get("name").and_then(Value::as_str) else {
                self.problem(
                    CargoDiagnosticCode::TargetInvalid,
                    Some(kind),
                    Some(ordinal),
                );
                continue;
            };
            self.explicit_one(table, kind, class, name, None, Some(ordinal));
        }
    }

    fn explicit_one(
        &mut self,
        table: &Table,
        kind: CargoTargetKind,
        class: TargetClass,
        name: &str,
        fallback: Option<&str>,
        ordinal: Option<usize>,
    ) {
        if !valid_name(name) {
            self.problem(CargoDiagnosticCode::TargetInvalid, Some(kind), ordinal);
            return;
        }
        if !self.claim(kind, name, ordinal, true) {
            return;
        }
        let root = match table.get("path") {
            Some(Value::String(raw)) => {
                resolve(&self.root, raw).ok_or(CargoDiagnosticCode::TargetPathInvalid)
            }
            Some(_) => Err(CargoDiagnosticCode::TargetPathInvalid),
            None => self.infer(kind, name, fallback),
        };
        match root {
            Ok(root) => self.add(kind, class, name, root, ordinal),
            Err(code) => self.problem(code, Some(kind), ordinal),
        }
    }

    fn infer(
        &self,
        kind: CargoTargetKind,
        name: &str,
        fallback: Option<&str>,
    ) -> Result<RepositoryPath, CargoDiagnosticCode> {
        let raw = fallback.map_or_else(
            || inferred(kind, name, self.package),
            |value| vec![value.to_owned()],
        );
        let mut candidates: Vec<_> = raw
            .into_iter()
            .filter_map(|item| resolve(&self.root, &item))
            .filter(|path| self.snapshot.contains_path(path))
            .collect();
        candidates.sort();
        candidates.dedup();
        match candidates.len() {
            0 => Err(CargoDiagnosticCode::TargetMissing),
            1 => candidates.pop().ok_or(CargoDiagnosticCode::TargetMissing),
            _ => Err(CargoDiagnosticCode::TargetAmbiguous),
        }
    }

    pub(super) fn auto_single(
        &mut self,
        raw: &str,
        kind: CargoTargetKind,
        class: TargetClass,
        name: &str,
        enabled: bool,
    ) {
        if !enabled || self.seen.contains(&(kind, name.to_owned())) {
            return;
        }
        let Some(root) = resolve(&self.root, raw) else {
            return;
        };
        if self.snapshot.contains_path(&root) && self.claim(kind, name, None, false) {
            self.add(kind, class, name, root, None);
        }
    }

    pub(super) fn auto_group(
        &mut self,
        directory: &str,
        kind: CargoTargetKind,
        class: TargetClass,
        enabled: bool,
    ) {
        if !enabled {
            return;
        }
        let prefix = format!("{}/", root_join(&self.root, directory));
        let mut groups: BTreeMap<String, Vec<RepositoryPath>> = BTreeMap::new();
        for (path, _) in self.snapshot.iter() {
            let Some(relative) = path.as_str().strip_prefix(&prefix) else {
                continue;
            };
            let candidate = relative
                .strip_suffix(".rs")
                .filter(|name| !name.contains('/'))
                .or_else(|| {
                    relative
                        .strip_suffix("/main.rs")
                        .filter(|name| !name.contains('/'))
                });
            let Some(name) = candidate else {
                continue;
            };
            if valid_name(name) {
                groups
                    .entry(name.to_owned())
                    .or_default()
                    .push(path.clone());
            } else {
                self.diagnostics.push(diagnostic(
                    CargoDiagnosticCode::TargetInvalid,
                    path,
                    Some(kind),
                    None,
                ));
            }
        }
        for (name, roots) in groups {
            if self.seen.contains(&(kind, name.clone())) {
                continue;
            }
            if roots.len() != 1 {
                self.problem(CargoDiagnosticCode::TargetAmbiguous, Some(kind), None);
            } else if self.claim(kind, &name, None, false) {
                self.add(kind, class, &name, roots[0].clone(), None);
            }
        }
    }

    pub(super) fn build_script(&mut self, value: Option<&Value>) {
        match value {
            Some(Value::Boolean(false)) => {}
            Some(Value::String(raw)) => self.required(raw),
            Some(_) => self.problem(
                CargoDiagnosticCode::PackageOptionInvalid,
                Some(CargoTargetKind::BuildScript),
                None,
            ),
            None => self.auto_single(
                "build.rs",
                CargoTargetKind::BuildScript,
                TargetClass::Production,
                "build-script-build",
                true,
            ),
        }
    }

    fn required(&mut self, raw: &str) {
        let kind = CargoTargetKind::BuildScript;
        if !self.claim(kind, "build-script-build", None, true) {
            return;
        }
        match resolve(&self.root, raw) {
            Some(root) => self.add(
                kind,
                TargetClass::Production,
                "build-script-build",
                root,
                None,
            ),
            None => self.problem(CargoDiagnosticCode::TargetPathInvalid, Some(kind), None),
        }
    }

    fn claim(
        &mut self,
        kind: CargoTargetKind,
        name: &str,
        ordinal: Option<usize>,
        explicit: bool,
    ) -> bool {
        if self.seen.insert((kind, name.to_owned())) {
            return true;
        }
        if explicit {
            self.problem(CargoDiagnosticCode::DuplicateTarget, Some(kind), ordinal);
        }
        false
    }

    fn add(
        &mut self,
        kind: CargoTargetKind,
        class: TargetClass,
        name: &str,
        root: RepositoryPath,
        ordinal: Option<usize>,
    ) {
        match self.snapshot.get(&root) {
            None => {
                self.diagnostics.push(diagnostic(
                    CargoDiagnosticCode::TargetMissing,
                    &root,
                    Some(kind),
                    ordinal,
                ));
                return;
            }
            Some(entry) if entry.kind() != EntryKind::Regular => {
                self.diagnostics.push(diagnostic(
                    CargoDiagnosticCode::EntryNotRegular,
                    &root,
                    Some(kind),
                    ordinal,
                ));
                return;
            }
            Some(_) => {}
        }
        self.targets.push(CargoTarget {
            package: self.package.to_owned(),
            package_root: self.root.clone(),
            kind,
            class,
            name: name.to_owned(),
            root,
        });
    }

    fn problem(
        &mut self,
        code: CargoDiagnosticCode,
        kind: Option<CargoTargetKind>,
        ordinal: Option<usize>,
    ) {
        self.diagnostics
            .push(diagnostic(code, &self.manifest, kind, ordinal));
    }

    pub(super) fn finish(mut self) -> Vec<CargoTarget> {
        self.targets.sort();
        self.targets
    }
}

fn proc_macro(table: &Table) -> Option<bool> {
    let direct = match table.get("proc-macro") {
        Some(Value::Boolean(value)) => *value,
        Some(_) => return None,
        None => false,
    };
    let Some(value) = table.get("crate-type") else {
        return Some(direct);
    };
    let crate_types = match value {
        Value::Array(items) if !items.is_empty() => items,
        _ => return None,
    };
    let mut proc_macro_type = false;
    for crate_type in crate_types {
        let crate_type = crate_type.as_str()?;
        if !matches!(
            crate_type,
            "bin" | "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
        ) {
            return None;
        }
        proc_macro_type |= crate_type == "proc-macro";
    }
    if (proc_macro_type && crate_types.len() != 1) || (direct && !proc_macro_type) {
        None
    } else {
        Some(direct || proc_macro_type)
    }
}

fn optional_string<'a>(table: &'a Table, key: &str, default: &'a str) -> Option<&'a str> {
    match table.get(key) {
        Some(Value::String(value)) => Some(value),
        Some(_) => None,
        None => Some(default),
    }
}

fn inferred(kind: CargoTargetKind, name: &str, package: &str) -> Vec<String> {
    match kind {
        CargoTargetKind::Binary => {
            let mut values = vec![
                format!("src/bin/{name}.rs"),
                format!("src/bin/{name}/main.rs"),
            ];
            if name == package {
                values.push("src/main.rs".to_owned());
            }
            values
        }
        CargoTargetKind::Example => vec![
            format!("examples/{name}.rs"),
            format!("examples/{name}/main.rs"),
        ],
        CargoTargetKind::IntegrationTest => {
            vec![format!("tests/{name}.rs"), format!("tests/{name}/main.rs")]
        }
        CargoTargetKind::Benchmark => vec![
            format!("benches/{name}.rs"),
            format!("benches/{name}/main.rs"),
        ],
        CargoTargetKind::Library | CargoTargetKind::ProcMacro | CargoTargetKind::BuildScript => {
            Vec::new()
        }
    }
}

fn resolve(root: &PackageRoot, raw: &str) -> Option<RepositoryPath> {
    if invalid_path(raw) {
        return None;
    }
    let mut parts: Vec<&str> = match root {
        PackageRoot::WorkspaceRoot => Vec::new(),
        PackageRoot::Member(path) => path.as_str().split('/').collect(),
    };
    let floor = parts.len();
    for part in raw.split('/') {
        match part {
            "." => {}
            ".." if parts.len() > floor => {
                parts.pop();
            }
            "" | ".." => return None,
            value => parts.push(value),
        }
    }
    let Ok(path) = RepositoryPath::parse(parts.join("/")) else {
        return None;
    };
    Some(path)
}

fn invalid_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    raw.is_empty()
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn root_join(root: &PackageRoot, raw: &str) -> String {
    match root {
        PackageRoot::WorkspaceRoot => raw.to_owned(),
        PackageRoot::Member(path) => format!("{path}/{raw}"),
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
}
