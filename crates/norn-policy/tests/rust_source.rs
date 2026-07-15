//! Production Rust source projection and module-shape tests.

use norn_policy::RepositoryPath;
use norn_policy::rust::{ModuleShapeKind, RustSource, module_shape, production_metrics};

#[test]
fn test_only_items_do_not_contribute_loc_or_projection() -> Result<(), Box<dyn std::error::Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    let first = b"pub fn live() {}\n#[cfg(test)]\nmod tests {\n fn one() {}\n}\n";
    let second = b"pub fn live() {}\n#[cfg(test)]\nmod tests {\n fn one() {}\n fn two() {}\n}\n";

    let first_metrics = production_metrics(&path, first)?;
    let second_metrics = production_metrics(&path, second)?;
    assert_eq!(first_metrics.loc, second_metrics.loc);
    assert_eq!(first_metrics.projection, second_metrics.projection);
    assert_eq!(first_metrics.excluded.len(), 1);
    Ok(())
}

#[test]
fn possible_cfg_remains_in_production() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        RustSource::parse(b"#[cfg(target_os = \"macos\")]\npub fn platform() {}\n".to_vec())?;
    assert!(source.test_only_ranges()?.is_empty());
    Ok(())
}

#[test]
fn cfg_attr_uses_both_possible_branches() -> Result<(), Box<dyn std::error::Error>> {
    let possible = RustSource::parse(
        b"#[cfg_attr(feature = \"x\", cfg(test))]\npub fn maybe() {}\n".to_vec(),
    )?;
    let definite =
        RustSource::parse(b"#[cfg_attr(not(test), cfg(test))]\npub fn hidden() {}\n".to_vec())?;

    assert!(possible.test_only_ranges()?.is_empty());
    assert_eq!(definite.test_only_ranges()?.len(), 1);
    Ok(())
}

#[test]
fn projection_is_path_bound_and_crlf_normalized() -> Result<(), Box<dyn std::error::Error>> {
    let left: RepositoryPath = "src/left.rs".parse()?;
    let right: RepositoryPath = "src/right.rs".parse()?;
    let lf = production_metrics(&left, b"pub fn value() {\n  1;\n}\n")?;
    let crlf = production_metrics(&left, b"pub fn value() {\r\n  1;\r\n}\r\n")?;
    let moved = production_metrics(&right, b"pub fn value() {\n  1;\n}\n")?;

    assert_eq!(lf.projection, crlf.projection);
    assert_ne!(lf.projection, moved.projection);
    Ok(())
}

#[test]
fn module_shape_allows_only_external_modules_and_visible_uses()
-> Result<(), Box<dyn std::error::Error>> {
    let valid = b"//! docs\npub mod child;\npub(crate) use child::Thing;\npub(super) use child::Other;\npub(in crate) use child::Third;\npub(in crate::parent) use child::Fourth;\n";
    assert!(module_shape(valid)?.is_empty());

    let private_use = module_shape(b"use child::Thing;\n")?;
    assert_eq!(private_use[0].kind, ModuleShapeKind::PrivateUse);

    for private_scope in [
        b"pub(self) use child::Thing;\n".as_slice(),
        b"pub(in self) use child::Thing;\n".as_slice(),
    ] {
        let violations = module_shape(private_scope)?;
        assert_eq!(violations[0].kind, ModuleShapeKind::PrivateUse);
    }

    let inline = module_shape(b"pub mod child {}\n")?;
    assert_eq!(inline[0].kind, ModuleShapeKind::InlineModule);

    let function = module_shape(b"pub fn work() {}\n")?;
    assert_eq!(function[0].kind, ModuleShapeKind::OtherItem);
    Ok(())
}

#[test]
fn test_only_logic_is_ignored_by_module_shape() -> Result<(), Box<dyn std::error::Error>> {
    let source = b"pub mod child;\n#[cfg(test)]\nfn helper() {}\n";
    assert!(module_shape(source)?.is_empty());
    Ok(())
}

#[test]
fn inner_test_cfg_excludes_the_complete_inline_module() -> Result<(), Box<dyn std::error::Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    let hidden = production_metrics(
        &path,
        b"pub mod hidden {\n#![cfg(test)]\nfn helper() {}\n}\n",
    )?;
    let empty = production_metrics(&path, b"")?;

    assert_eq!(hidden.loc, 0);
    assert_eq!(hidden.excluded[0].start(), 0);
    assert_eq!(hidden.projection, empty.projection);
    Ok(())
}

#[test]
fn raw_cfg_names_cover_outer_inner_and_nested_attributes() -> Result<(), Box<dyn std::error::Error>>
{
    for source in [
        "#[r#cfg(r#test)]\nfn hidden() {}\n",
        "mod hidden {\n#![r#cfg(test)]\nfn helper() {}\n}\n",
        "#[r#cfg_attr(all(), r#cfg(test))]\nfn hidden() {}\n",
    ] {
        let parsed = RustSource::parse(source.as_bytes().to_vec())?;
        assert_eq!(parsed.test_only_ranges()?.len(), 1, "{source}");
    }
    Ok(())
}

#[test]
fn production_range_walk_uses_heap_at_twenty_thousand_modules()
-> Result<(), Box<dyn std::error::Error>> {
    const DEPTH: usize = 20_000;

    let mut source = String::with_capacity(DEPTH * 14 + 64);
    for _ in 0..DEPTH {
        source.push_str("mod layer {");
    }
    source.push_str("#[cfg(test)] fn hidden() {}");
    source.extend(std::iter::repeat_n('}', DEPTH));
    let parsed = RustSource::parse(source.into_bytes())?;
    assert_eq!(parsed.test_only_ranges()?.len(), 1);
    Ok(())
}
