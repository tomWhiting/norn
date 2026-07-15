//! Snapshot proof that the harness name resolves to locked crates.io trybuild.

use toml::Value;

use super::super::super::cargo::{CargoPackage, CargoTarget};
use crate::{EntryKind, OwnedSnapshot, RepositoryPath};

const TRYBUILD_VERSION: &str = "1.0.117";
const TRYBUILD_REQUIREMENT: &str = "1";
const TRYBUILD_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const TRYBUILD_CHECKSUM: &str = "0710d4dfbeae4f9c390baa784c49858a7468fa433f3fe5d0ec5ebef651cf59f9";

pub(super) fn is_verified(
    snapshot: &OwnedSnapshot,
    package: &CargoPackage,
    target: &CargoTarget,
) -> bool {
    let Some(manifest) = read_toml(snapshot, package.manifest()) else {
        return false;
    };
    if !has_external_version_dependency(&manifest)
        || manifest.get("test").is_some()
        || !is_canonical_auto_test(package, target)
        || repository_config_overrides(snapshot)
    {
        return false;
    }
    let Some(root_manifest_path) = repository_path("Cargo.toml") else {
        return false;
    };
    let Some(root_manifest) = read_toml(snapshot, &root_manifest_path) else {
        return false;
    };
    if root_has_dependency_overrides(&root_manifest) {
        return false;
    }
    let Some(lock_path) = repository_path("Cargo.lock") else {
        return false;
    };
    let Some(lock) = read_toml(snapshot, &lock_path) else {
        return false;
    };
    locked_dependency_is_crates_io(&lock, package.name())
}

fn has_external_version_dependency(manifest: &Value) -> bool {
    manifest
        .get("dev-dependencies")
        .and_then(Value::as_table)
        .and_then(|dependencies| dependencies.get("trybuild"))
        .and_then(Value::as_str)
        == Some(TRYBUILD_REQUIREMENT)
}

fn root_has_dependency_overrides(manifest: &Value) -> bool {
    manifest.get("patch").is_some() || manifest.get("replace").is_some()
}

fn repository_config_overrides(snapshot: &OwnedSnapshot) -> bool {
    [".cargo/config", ".cargo/config.toml"].iter().any(|raw| {
        let Some(path) = repository_path(raw) else {
            return true;
        };
        if !snapshot.contains_path(&path) {
            return false;
        }
        let Some(config) = read_toml(snapshot, &path) else {
            return true;
        };
        ["paths", "patch", "source", "registries"]
            .iter()
            .any(|key| config.get(*key).is_some())
    })
}

fn is_canonical_auto_test(package: &CargoPackage, target: &CargoTarget) -> bool {
    let Some(relative) = package_relative(package, target.root()) else {
        return false;
    };
    relative == format!("tests/{}.rs", target.name())
        || relative == format!("tests/{}/main.rs", target.name())
}

fn package_relative<'a>(package: &CargoPackage, path: &'a RepositoryPath) -> Option<&'a str> {
    match package.root() {
        super::super::super::cargo::PackageRoot::WorkspaceRoot => Some(path.as_str()),
        super::super::super::cargo::PackageRoot::Member(root) => {
            path.as_str().strip_prefix(&format!("{root}/"))
        }
    }
}

fn locked_dependency_is_crates_io(lock: &Value, package_name: &str) -> bool {
    let Some(packages) = lock.get("package").and_then(Value::as_array) else {
        return false;
    };
    let local = packages
        .iter()
        .filter(|package| {
            lock_name(package) == Some(package_name) && package.get("source").is_none()
        })
        .collect::<Vec<_>>();
    let trybuild = packages
        .iter()
        .filter(|package| lock_name(package) == Some("trybuild"))
        .collect::<Vec<_>>();
    local.len() == 1
        && trybuild.len() == 1
        && local[0]
            .get("dependencies")
            .and_then(Value::as_array)
            .is_some_and(|dependencies| {
                dependencies
                    .iter()
                    .any(|dependency| dependency.as_str() == Some("trybuild"))
            })
        && crates_io_package(trybuild[0])
}

fn crates_io_package(package: &Value) -> bool {
    package.get("version").and_then(Value::as_str) == Some(TRYBUILD_VERSION)
        && package.get("source").and_then(Value::as_str) == Some(TRYBUILD_SOURCE)
        && package.get("checksum").and_then(Value::as_str) == Some(TRYBUILD_CHECKSUM)
}

fn lock_name(package: &Value) -> Option<&str> {
    package.get("name").and_then(Value::as_str)
}

fn read_toml(snapshot: &OwnedSnapshot, path: &RepositoryPath) -> Option<Value> {
    let entry = snapshot.get(path)?;
    if entry.kind() != EntryKind::Regular {
        return None;
    }
    let Ok(text) = std::str::from_utf8(entry.bytes()) else {
        return None;
    };
    let Ok(value) = toml::from_str(text) else {
        return None;
    };
    Some(value)
}

fn repository_path(raw: &str) -> Option<RepositoryPath> {
    let Ok(path) = RepositoryPath::parse(raw) else {
        return None;
    };
    Some(path)
}
