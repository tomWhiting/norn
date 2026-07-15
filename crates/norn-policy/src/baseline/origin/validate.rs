//! Cross-family ordering and identity checks for immutable origin facts.

use std::collections::BTreeSet;

use super::errors::OriginError;
use crate::baseline::{DebtOriginFact, ItemGroupFact, ProductionFileFact, WriterOperationFact};
use crate::facts::{SourceInventoryEntry, source_inventory_identity};
use crate::rust::modules::{CompileTestFixtureFact, ModuleTargetKind};

pub(super) fn validate_sequences(
    source_inventory: &[SourceInventoryEntry],
    fixtures: &[CompileTestFixtureFact],
    expected_source_digest: crate::Digest,
    production: &[ProductionFileFact],
    item_groups: &[ItemGroupFact],
    debt: &[DebtOriginFact],
    writers: &[WriterOperationFact],
) -> Result<(), OriginError> {
    validate_sources(source_inventory, expected_source_digest)?;
    validate_fixtures(source_inventory, fixtures)?;
    for (index, pair) in production.windows(2).enumerate() {
        if pair[0].path() >= pair[1].path() {
            return Err(OriginError::ProductionOrder { index: index + 1 });
        }
    }
    for (index, pair) in item_groups.windows(2).enumerate() {
        if item_group_order(&pair[0], &pair[1]).is_ge() {
            return Err(OriginError::ItemGroupOrder { index: index + 1 });
        }
    }
    for (index, pair) in debt.windows(2).enumerate() {
        if debt_order(&pair[0], &pair[1]).is_ge() {
            return Err(OriginError::DebtOrder { index: index + 1 });
        }
    }
    for (index, pair) in writers.windows(2).enumerate() {
        if writer_order(&pair[0], &pair[1]).is_ge() {
            return Err(OriginError::WriterOrder { index: index + 1 });
        }
    }

    let mut origin_ids = BTreeSet::new();
    for id in production
        .iter()
        .map(ProductionFileFact::origin_id)
        .chain(item_groups.iter().map(ItemGroupFact::origin_id))
        .chain(debt.iter().map(DebtOriginFact::origin_id))
        .chain(writers.iter().map(WriterOperationFact::origin_id))
    {
        if !origin_ids.insert(id) {
            return Err(OriginError::DuplicateOriginId);
        }
    }
    Ok(())
}

fn validate_sources(
    sources: &[SourceInventoryEntry],
    expected_digest: crate::Digest,
) -> Result<(), OriginError> {
    for (index, source) in sources.iter().enumerate() {
        if !source.production && !source.test_only {
            return Err(OriginError::SourceInventoryClassification { index });
        }
    }
    for (index, pair) in sources.windows(2).enumerate() {
        if pair[0].path >= pair[1].path {
            return Err(OriginError::SourceInventoryOrder { index: index + 1 });
        }
    }
    if source_inventory_identity(sources) != expected_digest {
        return Err(OriginError::SourceInventoryDigest);
    }
    Ok(())
}

fn validate_fixtures(
    sources: &[SourceInventoryEntry],
    fixtures: &[CompileTestFixtureFact],
) -> Result<(), OriginError> {
    for (index, pair) in fixtures.windows(2).enumerate() {
        if pair[0].path >= pair[1].path {
            return Err(OriginError::CompileTestFixtureOrder { index: index + 1 });
        }
    }
    for (index, fixture) in fixtures.iter().enumerate() {
        let source = match sources.binary_search_by(|source| source.path.cmp(&fixture.path)) {
            Ok(source_index) => sources.get(source_index),
            Err(_) => None,
        };
        if source.is_none_or(|source| source.production || !source.test_only)
            || fixture.harness.kind != ModuleTargetKind::IntegrationTest
        {
            return Err(OriginError::CompileTestFixtureSource { index });
        }
    }
    Ok(())
}

pub(super) fn item_group_order(left: &ItemGroupFact, right: &ItemGroupFact) -> std::cmp::Ordering {
    (left.path(), left.base_identity(), left.content()).cmp(&(
        right.path(),
        right.base_identity(),
        right.content(),
    ))
}

pub(super) fn debt_order(left: &DebtOriginFact, right: &DebtOriginFact) -> std::cmp::Ordering {
    (left.fingerprint(), left.ordinal(), left.path()).cmp(&(
        right.fingerprint(),
        right.ordinal(),
        right.path(),
    ))
}

pub(super) fn writer_order(
    left: &WriterOperationFact,
    right: &WriterOperationFact,
) -> std::cmp::Ordering {
    (left.operation_id(), left.path(), left.span()).cmp(&(
        right.operation_id(),
        right.path(),
        right.span(),
    ))
}
