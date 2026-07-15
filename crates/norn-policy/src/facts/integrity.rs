//! Cross-family integrity for the sealed canonical fact graph.

use std::collections::BTreeSet;

use thiserror::Error;

use super::{RepositoryFacts, source_inventory_identity};
use crate::writers::builtin_sink_registry;

pub(super) fn validate(facts: &RepositoryFacts) -> Result<(), RepositoryFactsError> {
    if !facts.cargo.is_valid() {
        return Err(RepositoryFactsError::Cargo);
    }
    if !facts.modules.is_valid() {
        return Err(RepositoryFactsError::Modules);
    }
    if !facts.failures.is_empty() {
        return Err(RepositoryFactsError::ConstructionFailures);
    }
    let writers = facts
        .writers
        .as_ref()
        .ok_or(RepositoryFactsError::WriterUnavailable)?;
    if !writers.has_valid_metadata() {
        return Err(RepositoryFactsError::WriterMetadata);
    }
    let Ok(registry) = builtin_sink_registry() else {
        return Err(RepositoryFactsError::WriterRegistry);
    };
    if writers.registry_digest() != registry.digest() {
        return Err(RepositoryFactsError::WriterRegistry);
    }
    if source_inventory_identity(&facts.source_inventory) != facts.source_inventory_digest {
        return Err(RepositoryFactsError::SourceDigest);
    }
    validate_source_inventory(facts)?;
    validate_compile_test_fixtures(facts)?;
    validate_production_inventory(facts)?;
    validate_writer_source_inventory(facts, writers)?;
    validate_family_paths(facts, writers.operations())?;
    Ok(())
}

fn validate_compile_test_fixtures(facts: &RepositoryFacts) -> Result<(), RepositoryFactsError> {
    if facts.modules.compile_test_fixtures != facts.compile_test_fixtures {
        return Err(RepositoryFactsError::CompileTestFixtureProjection);
    }
    for (index, pair) in facts.compile_test_fixtures.windows(2).enumerate() {
        if pair[0].path >= pair[1].path {
            return Err(RepositoryFactsError::CompileTestFixtureOrder { index: index + 1 });
        }
    }
    for (index, fixture) in facts.compile_test_fixtures.iter().enumerate() {
        let source = match facts
            .source_inventory
            .binary_search_by(|row| row.path.cmp(&fixture.path))
        {
            Ok(source_index) => facts.source_inventory.get(source_index),
            Err(_) => None,
        };
        let module = match facts
            .modules
            .files
            .binary_search_by(|row| row.path.cmp(&fixture.path))
        {
            Ok(module_index) => facts.modules.files.get(module_index),
            Err(_) => None,
        };
        let Some((source, module)) = source.zip(module) else {
            return Err(RepositoryFactsError::CompileTestFixtureSource { index });
        };
        if source.production
            || !source.test_only
            || module.production
            || !module.test_only
            || !module.test_targets.contains(&fixture.harness)
        {
            return Err(RepositoryFactsError::CompileTestFixtureClassification { index });
        }
    }
    Ok(())
}

fn validate_writer_source_inventory(
    facts: &RepositoryFacts,
    writers: &crate::writers::WriterInventory,
) -> Result<(), RepositoryFactsError> {
    let expected = facts
        .source_inventory
        .iter()
        .filter(|source| source.production)
        .collect::<Vec<_>>();
    if expected.len() != writers.sources().len() {
        return Err(RepositoryFactsError::WriterInventoryLength);
    }
    for (index, (source, writer)) in expected.iter().zip(writers.sources()).enumerate() {
        if &source.path != writer.path() || source.content != writer.content() {
            return Err(RepositoryFactsError::WriterInventoryRow { index });
        }
    }
    Ok(())
}

fn validate_source_inventory(facts: &RepositoryFacts) -> Result<(), RepositoryFactsError> {
    if facts.modules.files.len() != facts.source_inventory.len() {
        return Err(RepositoryFactsError::SourceInventoryLength);
    }
    for (index, (module, source)) in facts
        .modules
        .files
        .iter()
        .zip(&facts.source_inventory)
        .enumerate()
    {
        if module.path != source.path
            || module.production != source.production
            || module.test_only != source.test_only
        {
            return Err(RepositoryFactsError::SourceInventoryRow { index });
        }
    }
    Ok(())
}

fn validate_production_inventory(facts: &RepositoryFacts) -> Result<(), RepositoryFactsError> {
    let expected = facts
        .modules
        .files
        .iter()
        .filter(|file| file.production)
        .collect::<Vec<_>>();
    if expected.len() != facts.production_files.len() {
        return Err(RepositoryFactsError::ProductionInventoryLength);
    }
    for (index, (module, production)) in expected.iter().zip(&facts.production_files).enumerate() {
        if module.path != production.path || module.production_targets != production.targets {
            return Err(RepositoryFactsError::ProductionInventoryRow { index });
        }
    }
    Ok(())
}

fn validate_family_paths(
    facts: &RepositoryFacts,
    writers: &[crate::writers::WriterOperation],
) -> Result<(), RepositoryFactsError> {
    let source_paths: BTreeSet<&str> = facts
        .source_inventory
        .iter()
        .map(|source| source.path.as_str())
        .collect();
    let production_paths: BTreeSet<&str> = facts
        .production_files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    for (index, item) in facts.items.iter().enumerate() {
        if !source_paths.contains(item.path.as_str()) {
            return Err(RepositoryFactsError::ItemSource { index });
        }
    }
    for (index, debt) in facts.debt.iter().enumerate() {
        if !source_paths.contains(debt.path().as_str()) {
            return Err(RepositoryFactsError::DebtSource { index });
        }
    }
    for (index, writer) in writers.iter().enumerate() {
        if !production_paths.contains(writer.path().as_str()) {
            return Err(RepositoryFactsError::WriterSource { index });
        }
    }
    Ok(())
}

/// Sealed canonical fact-graph structural or completeness failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RepositoryFactsError {
    /// Cargo discovery reported a structural failure.
    #[error("canonical repository facts contain invalid Cargo discovery")]
    Cargo,
    /// Module reachability reported a structural failure.
    #[error("canonical repository facts contain invalid module reachability")]
    Modules,
    /// One or more analyzer families reported construction failures.
    #[error("canonical repository facts contain analyzer failures")]
    ConstructionFailures,
    /// Writer analysis did not return an inventory.
    #[error("canonical repository facts have no writer inventory")]
    WriterUnavailable,
    /// Writer inventory metadata or its source digest is internally stale.
    #[error("canonical writer inventory metadata is invalid")]
    WriterMetadata,
    /// Writer analysis did not use the compiled reviewed registry.
    #[error("canonical writer inventory used an unreviewed registry")]
    WriterRegistry,
    /// Writer analysis and production reachability contain different row counts.
    #[error("canonical writer inventory does not cover every production source")]
    WriterInventoryLength,
    /// One writer source identity differs in exact path or content.
    #[error("canonical writer source differs from production source at row {index}")]
    WriterInventoryRow {
        /// First mismatched source identity row.
        index: usize,
    },
    /// The classified source inventory digest is stale or forged.
    #[error("canonical source inventory digest does not match its rows")]
    SourceDigest,
    /// Reachability and source inventory contain different row counts.
    #[error("canonical source inventory does not cover every reachable source")]
    SourceInventoryLength,
    /// One reachability/source row differs in path or classification.
    #[error("canonical source inventory differs from reachability at row {index}")]
    SourceInventoryRow {
        /// First mismatched row.
        index: usize,
    },
    /// Module analysis and the retained compile-test projection differ.
    #[error("canonical compile-test fixture projection differs from module analysis")]
    CompileTestFixtureProjection,
    /// Compile-test fixture rows were not strictly path-sorted and unique.
    #[error("canonical compile-test fixtures are not strictly sorted at row {index}")]
    CompileTestFixtureOrder {
        /// First invalid row.
        index: usize,
    },
    /// A compile-test fixture has no corresponding analyzed source.
    #[error("canonical compile-test fixture at row {index} has no source row")]
    CompileTestFixtureSource {
        /// Invalid fixture row.
        index: usize,
    },
    /// A compile-test fixture is not exclusively test-reachable through its harness.
    #[error("canonical compile-test fixture classification is invalid at row {index}")]
    CompileTestFixtureClassification {
        /// Invalid fixture row.
        index: usize,
    },
    /// Production reachability and production facts have different row counts.
    #[error("canonical production inventory does not cover every production source")]
    ProductionInventoryLength,
    /// One production row differs in path or complete target set.
    #[error("canonical production inventory differs from reachability at row {index}")]
    ProductionInventoryRow {
        /// First mismatched row.
        index: usize,
    },
    /// An item group names a source outside the complete inventory.
    #[error("canonical item group at row {index} has no source inventory row")]
    ItemSource {
        /// Invalid item row.
        index: usize,
    },
    /// A debt occurrence names a source outside the complete inventory.
    #[error("canonical debt occurrence at row {index} has no source inventory row")]
    DebtSource {
        /// Invalid debt row.
        index: usize,
    },
    /// A writer operation names a non-production source.
    #[error("canonical writer operation at row {index} has no production row")]
    WriterSource {
        /// Invalid writer row.
        index: usize,
    },
}
