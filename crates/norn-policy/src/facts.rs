//! Canonical repository fact graph shared by origin generation and evaluation.

mod integrity;

pub use integrity::RepositoryFactsError;

use serde::{Deserialize, Serialize};

use crate::config::RepositoryPolicy;
use crate::debt::{DebtOccurrence, DebtTargetContext, DebtTargetKind, scan_rust_debt};
use crate::digest::{Digest, digest_bytes};
use crate::path::RepositoryPath;
use crate::rust::cargo::CargoDiscovery;
use crate::rust::modules::{
    CompileTestFixtureFact, GeneratedIncludeRegistry, ModuleAnalysis, ModuleTargetIdentity,
    ModuleTargetKind, analyze_modules_with_cargo,
};
use crate::rust::{
    ModuleShapeViolation, ProductionMetrics, RustItemProjection, module_shape, production_metrics,
    rust_item_projections,
};
use crate::snapshot::{EntryKind, OwnedSnapshot};
use crate::writers::{WriterInventory, WriterSource, analyze_writers, builtin_sink_registry};

/// Stable fact-construction failure class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactFailureCode {
    /// A classified source is absent from the snapshot.
    SourceMissing,
    /// A classified source is not an ordinary file.
    SourceNotRegular,
    /// Production LOC or projection analysis failed.
    ProductionMetrics,
    /// Production `mod.rs` shape analysis failed.
    ModuleShape,
    /// Rust item identity/projection analysis failed.
    ItemProjection,
    /// Cargo target identity could not become a debt context.
    DebtTarget,
    /// Prohibited-debt analysis failed.
    DebtAnalysis,
    /// Writer inventory construction failed.
    WriterAnalysis,
}

/// One deterministic failure without source snippets or rendered prose.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FactFailure {
    /// Closed failure category.
    pub code: FactFailureCode,
    /// Relevant repository path, when the failure is source-specific.
    pub path: Option<RepositoryPath>,
    /// Target identity for a target-specific failure.
    pub target: Option<ModuleTargetIdentity>,
}

/// Exact classified-source input retained by the source-inventory digest.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInventoryEntry {
    /// Normalized source path.
    pub path: RepositoryPath,
    /// Digest of exact owned source bytes.
    pub content: Digest,
    /// Whether any production root reaches this source.
    pub production: bool,
    /// Whether a distinct test-only root or branch reaches this source.
    pub test_only: bool,
}

/// Production-only facts for one Rust source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProductionFileFact {
    /// Normalized source path.
    pub path: RepositoryPath,
    /// Sorted Cargo targets establishing production reachability.
    pub targets: Vec<ModuleTargetIdentity>,
    /// Cfg-aware Tokei LOC and path-bound token projection.
    pub metrics: ProductionMetrics,
    /// Declaration-only violations when this source is `mod.rs`.
    pub module_shape: Vec<ModuleShapeViolation>,
}

impl ProductionFileFact {
    /// Return whether the source uses the entrypoint-specific LOC ceiling.
    #[must_use]
    pub fn is_entrypoint(&self) -> bool {
        self.targets.iter().any(|target| {
            target.root == self.path
                && matches!(
                    target.kind,
                    ModuleTargetKind::Library
                        | ModuleTargetKind::ProcMacro
                        | ModuleTargetKind::Binary
                )
        })
    }

    /// Return the applicable validated policy ceiling.
    #[must_use]
    pub fn loc_limit(&self, policy: &RepositoryPolicy) -> u32 {
        if self.is_entrypoint() {
            policy.production_loc().entrypoint_max()
        } else {
            policy.production_loc().other_rust_max()
        }
    }
}

/// One item projection paired with its source path for origin comparison.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceItemFact {
    /// Normalized source path.
    pub path: RepositoryPath,
    /// Stable item identity, content, span, and current classification.
    pub item: RustItemProjection,
}

/// Complete deterministic facts derived from one immutable snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RepositoryFacts {
    /// Canonical identity of the complete owned snapshot analyzed.
    snapshot_identity: Digest,
    /// Pure Cargo workspace and target discovery.
    cargo: CargoDiscovery,
    /// Module/include reachability and structural diagnostics.
    modules: ModuleAnalysis,
    /// Exact classified-source inventory.
    source_inventory: Vec<SourceInventoryEntry>,
    /// Exact compile-test fixture provenance.
    compile_test_fixtures: Vec<CompileTestFixtureFact>,
    /// Digest of the complete source inventory.
    source_inventory_digest: Digest,
    /// Production file LOC/projection/shape facts.
    production_files: Vec<ProductionFileFact>,
    /// Production and test-only item projections for reclassification checks.
    items: Vec<SourceItemFact>,
    /// Prohibited-debt occurrences in every production target context.
    debt: Vec<DebtOccurrence>,
    /// Writer operations and unresolved/stale sink facts, when scanning ran.
    writers: Option<WriterInventory>,
    /// Closed construction failures in deterministic order.
    failures: Vec<FactFailure>,
}

impl RepositoryFacts {
    /// Return the complete owned-snapshot identity that produced these facts.
    #[must_use]
    pub const fn snapshot_identity(&self) -> Digest {
        self.snapshot_identity
    }

    /// Validate the sealed graph and every cross-family completeness invariant.
    ///
    /// # Errors
    ///
    /// Returns the first closed structural or inventory mismatch.
    pub fn validate_integrity(&self) -> Result<(), RepositoryFactsError> {
        integrity::validate(self)
    }

    /// Return whether all fact families are complete and mutually coherent.
    #[must_use]
    pub fn is_structurally_valid(&self) -> bool {
        self.validate_integrity().is_ok()
    }

    /// Borrow pure Cargo discovery facts.
    #[must_use]
    pub const fn cargo(&self) -> &CargoDiscovery {
        &self.cargo
    }

    /// Borrow module/include reachability facts.
    #[must_use]
    pub const fn modules(&self) -> &ModuleAnalysis {
        &self.modules
    }

    /// Borrow the complete classified source inventory.
    #[must_use]
    pub fn source_inventory(&self) -> &[SourceInventoryEntry] {
        &self.source_inventory
    }

    /// Borrow every proven compile-test fixture root.
    #[must_use]
    pub fn compile_test_fixtures(&self) -> &[CompileTestFixtureFact] {
        &self.compile_test_fixtures
    }

    /// Return the digest of the complete classified source inventory.
    #[must_use]
    pub const fn source_inventory_digest(&self) -> Digest {
        self.source_inventory_digest
    }

    /// Borrow every production file fact.
    #[must_use]
    pub fn production_files(&self) -> &[ProductionFileFact] {
        &self.production_files
    }

    /// Borrow every production/test item aggregate.
    #[must_use]
    pub fn items(&self) -> &[SourceItemFact] {
        &self.items
    }

    /// Borrow every prohibited-debt occurrence.
    #[must_use]
    pub fn debt(&self) -> &[DebtOccurrence] {
        &self.debt
    }

    /// Borrow the complete writer inventory when analysis succeeded.
    #[must_use]
    pub const fn writers(&self) -> Option<&WriterInventory> {
        self.writers.as_ref()
    }

    /// Borrow every closed fact-construction failure.
    #[must_use]
    pub fn failures(&self) -> &[FactFailure] {
        &self.failures
    }
}

/// Build the one canonical fact graph used by origin generation and checking.
///
/// Writer analysis always uses the compiled reviewed registry. Callers cannot
/// substitute an empty or locally extended registry at this authority boundary.
#[must_use]
pub fn analyze_facts(
    snapshot: &OwnedSnapshot,
    generated: &GeneratedIncludeRegistry,
) -> RepositoryFacts {
    let (cargo, modules) = analyze_modules_with_cargo(snapshot, generated);
    let mut source_inventory = Vec::new();
    let mut production_files = Vec::new();
    let mut items = Vec::new();
    let mut debt = Vec::new();
    let mut writer_sources = Vec::new();
    let mut failures = Vec::new();

    for file in &modules.files {
        let Some(entry) = snapshot.get(&file.path) else {
            failures.push(failure(FactFailureCode::SourceMissing, &file.path, None));
            continue;
        };
        if entry.kind() != EntryKind::Regular {
            failures.push(failure(FactFailureCode::SourceNotRegular, &file.path, None));
            continue;
        }
        source_inventory.push(SourceInventoryEntry {
            path: file.path.clone(),
            content: digest_bytes(entry.bytes()),
            production: file.production,
            test_only: file.test_only,
        });
        let projected = rust_item_projections(&file.path, entry.bytes());
        if let Ok(mut projected) = projected {
            let classification_failed = !file.production
                && projected
                    .iter_mut()
                    .try_for_each(RustItemProjection::force_test_only)
                    .is_err();
            if classification_failed {
                failures.push(failure(FactFailureCode::ItemProjection, &file.path, None));
            } else {
                items.extend(projected.into_iter().map(|item| SourceItemFact {
                    path: file.path.clone(),
                    item,
                }));
            }
        } else {
            failures.push(failure(FactFailureCode::ItemProjection, &file.path, None));
        }
        let mut debt_targets = file.production_targets.clone();
        debt_targets.extend(file.test_targets.iter().cloned());
        debt_targets.sort();
        debt_targets.dedup();
        scan_debt_targets(
            &file.path,
            entry.bytes(),
            &debt_targets,
            &mut debt,
            &mut failures,
        );
        if !file.production {
            continue;
        }
        let Ok(metrics) = production_metrics(&file.path, entry.bytes()) else {
            failures.push(failure(
                FactFailureCode::ProductionMetrics,
                &file.path,
                None,
            ));
            continue;
        };
        let shape = if file.path.file_name() == "mod.rs" {
            if let Ok(shape) = module_shape(entry.bytes()) {
                shape
            } else {
                failures.push(failure(FactFailureCode::ModuleShape, &file.path, None));
                Vec::new()
            }
        } else {
            Vec::new()
        };
        writer_sources.push(WriterSource::new(file.path.clone(), entry.bytes().to_vec()));
        production_files.push(ProductionFileFact {
            path: file.path.clone(),
            targets: file.production_targets.clone(),
            metrics,
            module_shape: shape,
        });
    }

    source_inventory.sort();
    production_files.sort_by(|left, right| left.path.cmp(&right.path));
    items.sort();
    debt.sort_by(|left, right| {
        (
            left.path(),
            left.target(),
            left.span(),
            left.construct(),
            left.fingerprint(),
        )
            .cmp(&(
                right.path(),
                right.target(),
                right.span(),
                right.construct(),
                right.fingerprint(),
            ))
    });
    let writers = (|| {
        let Ok(registry) = builtin_sink_registry() else {
            return None;
        };
        let Ok(inventory) = analyze_writers(&writer_sources, &registry) else {
            return None;
        };
        Some(inventory)
    })();
    if writers.is_none() {
        failures.push(FactFailure {
            code: FactFailureCode::WriterAnalysis,
            path: None,
            target: None,
        });
    }
    failures.sort();
    failures.dedup();
    let source_inventory_digest = source_inventory_identity(&source_inventory);
    let compile_test_fixtures = modules.compile_test_fixtures.clone();
    RepositoryFacts {
        snapshot_identity: snapshot.canonical_identity(),
        cargo,
        modules,
        source_inventory,
        compile_test_fixtures,
        source_inventory_digest,
        production_files,
        items,
        debt,
        writers,
        failures,
    }
}

fn scan_debt_targets(
    path: &RepositoryPath,
    bytes: &[u8],
    targets: &[ModuleTargetIdentity],
    debt: &mut Vec<DebtOccurrence>,
    failures: &mut Vec<FactFailure>,
) {
    for target in targets {
        let context = DebtTargetContext::new(debt_kind(target.kind), &target.package, &target.name);
        let Ok(context) = context else {
            failures.push(failure(FactFailureCode::DebtTarget, path, Some(target)));
            continue;
        };
        let occurrences = scan_rust_debt(path, &context, bytes);
        if let Ok(occurrences) = occurrences {
            debt.extend(occurrences);
        } else {
            failures.push(failure(FactFailureCode::DebtAnalysis, path, Some(target)));
        }
    }
}

fn failure(
    code: FactFailureCode,
    path: &RepositoryPath,
    target: Option<&ModuleTargetIdentity>,
) -> FactFailure {
    FactFailure {
        code,
        path: Some(path.clone()),
        target: target.cloned(),
    }
}

const fn debt_kind(kind: ModuleTargetKind) -> DebtTargetKind {
    match kind {
        ModuleTargetKind::Library => DebtTargetKind::Library,
        ModuleTargetKind::ProcMacro => DebtTargetKind::ProcMacro,
        ModuleTargetKind::Binary => DebtTargetKind::Binary,
        ModuleTargetKind::Example => DebtTargetKind::Example,
        ModuleTargetKind::BuildScript => DebtTargetKind::BuildScript,
        ModuleTargetKind::IntegrationTest => DebtTargetKind::IntegrationTest,
        ModuleTargetKind::Benchmark => DebtTargetKind::Benchmark,
    }
}

/// Hash a complete sorted source inventory under its fixed framing domain.
#[must_use]
pub fn source_inventory_identity(entries: &[SourceInventoryEntry]) -> Digest {
    let mut framed = Vec::new();
    append_inventory_field(&mut framed, b"norn-source-inventory-1");
    for entry in entries {
        append_inventory_field(&mut framed, entry.path.as_str().as_bytes());
        append_inventory_field(&mut framed, entry.content.as_bytes());
        framed.push(u8::from(entry.production));
        framed.push(u8::from(entry.test_only));
    }
    digest_bytes(&framed)
}

fn append_inventory_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
