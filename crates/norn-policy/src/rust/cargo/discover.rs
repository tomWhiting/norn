//! Cargo discovery orchestration.

use std::collections::BTreeSet;

use toml::Value;

use super::manifest::{
    automatic_by_default, inherited_edition, manifest_path, option_bool, parse_manifest,
    select_members,
};
use super::target::TargetBuilder;
use super::{
    CargoDiagnostic, CargoDiagnosticCode, CargoDiscovery, CargoPackage, CargoTargetKind,
    PackageRoot, TargetClass, diagnostic,
};
use crate::{OwnedSnapshot, RepositoryPath};

/// Discover local workspace packages and Cargo targets without filesystem I/O.
#[must_use]
pub fn discover_cargo(snapshot: &OwnedSnapshot) -> CargoDiscovery {
    let Ok(root_manifest) = RepositoryPath::parse("Cargo.toml") else {
        return finish(Vec::new(), Vec::new());
    };
    let mut diagnostics = Vec::new();
    let Some(root_value) = parse_manifest(snapshot, &root_manifest, true, &mut diagnostics) else {
        return finish(Vec::new(), diagnostics);
    };
    let Some(workspace) = root_value.get("workspace").and_then(Value::as_table) else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::WorkspaceInvalid,
            &root_manifest,
            None,
            None,
        ));
        return finish(Vec::new(), diagnostics);
    };
    let roots = select_members(
        snapshot,
        &root_value,
        workspace,
        &root_manifest,
        &mut diagnostics,
    );
    let edition = inherited_edition(workspace, &root_manifest, &mut diagnostics);
    let mut packages = Vec::new();
    let mut names = BTreeSet::new();
    for root in roots {
        let Some(manifest) = manifest_path(&root) else {
            continue;
        };
        let value = if root == PackageRoot::WorkspaceRoot {
            root_value.clone()
        } else if let Some(value) = parse_manifest(snapshot, &manifest, false, &mut diagnostics) {
            value
        } else {
            continue;
        };
        if let Some(package) = discover_package(
            snapshot,
            root,
            manifest.clone(),
            &value,
            edition,
            &mut diagnostics,
        ) {
            if !names.insert(package.name.clone()) {
                diagnostics.push(diagnostic(
                    CargoDiagnosticCode::DuplicatePackageName,
                    &manifest,
                    None,
                    None,
                ));
            }
            packages.push(package);
        }
    }
    finish(packages, diagnostics)
}

fn discover_package(
    snapshot: &OwnedSnapshot,
    root: PackageRoot,
    manifest: RepositoryPath,
    value: &Value,
    inherited_edition: Option<&str>,
    diagnostics: &mut Vec<CargoDiagnostic>,
) -> Option<CargoPackage> {
    let Some(package) = value.get("package").and_then(Value::as_table) else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::PackageInvalid,
            &manifest,
            None,
            None,
        ));
        return None;
    };
    let Some(name) = package
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| valid_name(name))
    else {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::PackageInvalid,
            &manifest,
            None,
            None,
        ));
        return None;
    };
    let manual = ["lib", "bin", "example", "test", "bench"]
        .iter()
        .any(|key| value.get(*key).is_some());
    let default = automatic_by_default(package, inherited_edition, manual, &manifest, diagnostics);
    let autolib = option_bool(package, "autolib", default, &manifest, diagnostics);
    let autobins = option_bool(package, "autobins", default, &manifest, diagnostics);
    let autoexamples = option_bool(package, "autoexamples", default, &manifest, diagnostics);
    let autotests = option_bool(package, "autotests", default, &manifest, diagnostics);
    let autobenches = option_bool(package, "autobenches", default, &manifest, diagnostics);
    let mut builder =
        TargetBuilder::new(snapshot, root.clone(), name, manifest.clone(), diagnostics);
    builder.library(value.get("lib"), autolib);
    builder.explicit(
        value,
        "bin",
        CargoTargetKind::Binary,
        TargetClass::Production,
    );
    builder.explicit(
        value,
        "example",
        CargoTargetKind::Example,
        TargetClass::Production,
    );
    builder.explicit(
        value,
        "test",
        CargoTargetKind::IntegrationTest,
        TargetClass::TestOnly,
    );
    builder.explicit(
        value,
        "bench",
        CargoTargetKind::Benchmark,
        TargetClass::TestOnly,
    );
    builder.auto_single(
        "src/main.rs",
        CargoTargetKind::Binary,
        TargetClass::Production,
        name,
        autobins,
    );
    builder.auto_group(
        "src/bin",
        CargoTargetKind::Binary,
        TargetClass::Production,
        autobins,
    );
    builder.auto_group(
        "examples",
        CargoTargetKind::Example,
        TargetClass::Production,
        autoexamples,
    );
    builder.auto_group(
        "tests",
        CargoTargetKind::IntegrationTest,
        TargetClass::TestOnly,
        autotests,
    );
    builder.auto_group(
        "benches",
        CargoTargetKind::Benchmark,
        TargetClass::TestOnly,
        autobenches,
    );
    builder.build_script(package.get("build"));
    let targets = builder.finish();
    if targets.is_empty() {
        diagnostics.push(diagnostic(
            CargoDiagnosticCode::NoPackageTargets,
            &manifest,
            None,
            None,
        ));
    }
    Some(CargoPackage {
        name: name.to_owned(),
        root,
        manifest,
        targets,
    })
}

fn finish(
    mut packages: Vec<CargoPackage>,
    mut diagnostics: Vec<CargoDiagnostic>,
) -> CargoDiscovery {
    packages.sort_by(|left, right| {
        left.root
            .cmp(&right.root)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.manifest.cmp(&right.manifest))
            .then_with(|| left.targets.cmp(&right.targets))
    });
    diagnostics.sort();
    CargoDiscovery {
        packages,
        diagnostics,
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
}
