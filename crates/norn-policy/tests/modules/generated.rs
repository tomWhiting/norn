use norn_policy::rust::cargo::CargoTargetKind;
use norn_policy::rust::modules::{
    GeneratedIncludeRegistration, GeneratedIncludeRegistry, HashedSourceInput,
    ModuleDiagnosticCode, ModuleTargetIdentity, SourceSpan, analyze_modules,
    generated_invocation_digest,
};
use norn_policy::{Digest, RepositoryPath, digest_bytes};

use super::support::{TestResult, analyze, has_code};

const INVOCATION: &str = "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"))";
const LIB: &str = "mod generated { include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\")); }";
const MANIFEST: &str = "[workspace]\n[package]\nname = \"app\"\nedition = \"2024\"\n";

#[test]
fn exact_generated_include_registration_is_accepted() -> TestResult {
    let entries = fixture_entries("schema-v1");
    let (snapshot, cargo, _) = analyze(&entries, &GeneratedIncludeRegistry::empty())?;
    let registry = registry(&cargo, "schema-v1")?;
    let result = analyze_modules(&snapshot, &registry);

    assert!(result.is_valid(), "{:#?}", result.diagnostics);
    assert!(
        result
            .files
            .iter()
            .all(|file| file.path.as_str() != "generated.rs")
    );
    Ok(())
}

#[test]
fn generated_input_digest_drift_is_rejected() -> TestResult {
    let original = fixture_entries("schema-v1");
    let (_, cargo, _) = analyze(&original, &GeneratedIncludeRegistry::empty())?;
    let registry = registry(&cargo, "schema-v1")?;
    let drifted = fixture_entries("schema-v2");
    let (snapshot, _, _) = analyze(&drifted, &GeneratedIncludeRegistry::empty())?;
    let result = analyze_modules(&snapshot, &registry);

    assert!(has_code(
        &result,
        ModuleDiagnosticCode::GeneratedIncludeRegistryDrift
    ));
    Ok(())
}

#[test]
fn every_generated_authority_pin_is_enforced() -> TestResult {
    let entries = fixture_entries("schema-v1");
    let (snapshot, cargo, _) = analyze(&entries, &GeneratedIncludeRegistry::empty())?;
    let baseline = registry(&cargo, "schema-v1")?;

    let mut callsite = baseline.clone();
    callsite.entries[0].callsite.start += 1;
    let callsite_result = analyze_modules(&snapshot, &callsite);
    assert!(!callsite_result.is_valid());

    let mut invocation = baseline.clone();
    invocation.entries[0].invocation_digest = Digest::from_bytes([0_u8; 32]);
    let invocation_result = analyze_modules(&snapshot, &invocation);
    assert!(has_code(
        &invocation_result,
        ModuleDiagnosticCode::GeneratedIncludeRegistryDrift
    ));

    let mut target = baseline.clone();
    target.entries[0].target.name.push_str("-drifted");
    let target_result = analyze_modules(&snapshot, &target);
    assert!(has_code(
        &target_result,
        ModuleDiagnosticCode::GeneratedIncludeRegistryDrift
    ));

    let mut generator = baseline;
    generator.entries[0].generator.digest = Digest::from_bytes([0_u8; 32]);
    let generator_result = analyze_modules(&snapshot, &generator);
    assert!(has_code(
        &generator_result,
        ModuleDiagnosticCode::GeneratedIncludeRegistryDrift
    ));
    Ok(())
}

fn fixture_entries(schema: &str) -> Vec<(&'static str, &str)> {
    vec![
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", LIB),
        ("build.rs", "fn main() {}"),
        ("assets/schema.txt", schema),
    ]
}

fn registry(
    cargo: &norn_policy::rust::cargo::CargoDiscovery,
    schema: &str,
) -> Result<GeneratedIncludeRegistry, Box<dyn std::error::Error>> {
    let target = cargo
        .packages()
        .iter()
        .flat_map(|package| package.targets().iter())
        .find(|target| target.kind() == CargoTargetKind::Library)
        .map(ModuleTargetIdentity::from_target)
        .ok_or("library target missing")?;
    let callsite_start = LIB.find(INVOCATION).ok_or("invocation offset missing")?;
    let enclosing_start = LIB.find("mod generated").ok_or("module offset missing")?;
    let enclosing_end = LIB
        .rfind('}')
        .map(|offset| offset + 1)
        .ok_or("module end missing")?;
    let digest = generated_invocation_digest("generated.rs")
        .ok_or("generated invocation digest rejected")?;
    Ok(GeneratedIncludeRegistry {
        schema_version: 1,
        entries: vec![GeneratedIncludeRegistration {
            source: RepositoryPath::parse("src/lib.rs")?,
            callsite: SourceSpan {
                start: callsite_start,
                end: callsite_start + INVOCATION.len(),
            },
            enclosing_item: SourceSpan {
                start: enclosing_start,
                end: enclosing_end,
            },
            invocation_digest: digest,
            target,
            generator: HashedSourceInput {
                path: RepositoryPath::parse("build.rs")?,
                digest: digest_bytes(b"fn main() {}"),
            },
            inputs: vec![HashedSourceInput {
                path: RepositoryPath::parse("assets/schema.txt")?,
                digest: digest_bytes(schema.as_bytes()),
            }],
            output_basename: "generated.rs".to_owned(),
        }],
    })
}
