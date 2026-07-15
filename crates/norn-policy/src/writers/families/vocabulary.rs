//! Closed writer-family and review vocabularies.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::writers::{WriterClassification, WriterClassificationKind, WriterToken};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WriterFamilyVocabulary {
    families: Vec<WriterToken>,
    shared_primitives: Vec<WriterToken>,
    cleanup_reviews: Vec<WriterToken>,
    false_positive_reviews: Vec<WriterToken>,
}

impl WriterFamilyVocabulary {
    pub(super) fn new(
        families: Vec<WriterToken>,
        shared_primitives: Vec<WriterToken>,
        cleanup_reviews: Vec<WriterToken>,
        false_positive_reviews: Vec<WriterToken>,
    ) -> Self {
        Self {
            families,
            shared_primitives,
            cleanup_reviews,
            false_positive_reviews,
        }
    }

    pub(super) fn families(&self) -> &[WriterToken] {
        &self.families
    }

    pub(super) fn shared_primitives(&self) -> &[WriterToken] {
        &self.shared_primitives
    }

    pub(super) fn cleanup_reviews(&self) -> &[WriterToken] {
        &self.cleanup_reviews
    }

    pub(super) fn false_positive_reviews(&self) -> &[WriterToken] {
        &self.false_positive_reviews
    }

    pub(super) fn validate(
        &self,
        classifications: &[WriterClassification],
    ) -> Result<(), VocabularyIssue> {
        validate_order(&self.families, VocabularyTable::Families)?;
        validate_order(&self.shared_primitives, VocabularyTable::SharedPrimitives)?;
        validate_order(&self.cleanup_reviews, VocabularyTable::CleanupReviews)?;
        validate_order(
            &self.false_positive_reviews,
            VocabularyTable::FalsePositiveReviews,
        )?;
        validate_disjoint(self)?;
        validate_references(self, classifications)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VocabularyTable {
    Families,
    SharedPrimitives,
    CleanupReviews,
    FalsePositiveReviews,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VocabularyIssue {
    Order {
        table: VocabularyTable,
        index: usize,
    },
    Overlap,
    UndeclaredReference,
    UnusedDeclaration,
    SharedPrimitiveEdges,
}

fn validate_order(values: &[WriterToken], table: VocabularyTable) -> Result<(), VocabularyIssue> {
    for (index, pair) in values.windows(2).enumerate() {
        if pair[0] >= pair[1] {
            return Err(VocabularyIssue::Order {
                table,
                index: index + 1,
            });
        }
    }
    Ok(())
}

fn validate_disjoint(vocabulary: &WriterFamilyVocabulary) -> Result<(), VocabularyIssue> {
    let mut declared = BTreeSet::new();
    for token in vocabulary
        .families
        .iter()
        .chain(&vocabulary.shared_primitives)
        .chain(&vocabulary.cleanup_reviews)
        .chain(&vocabulary.false_positive_reviews)
    {
        if !declared.insert(token) {
            return Err(VocabularyIssue::Overlap);
        }
    }
    Ok(())
}

fn validate_references(
    vocabulary: &WriterFamilyVocabulary,
    classifications: &[WriterClassification],
) -> Result<(), VocabularyIssue> {
    let mut families = BTreeSet::new();
    let mut shared_primitives = BTreeSet::new();
    let mut cleanup_reviews = BTreeSet::new();
    let mut false_positive_reviews = BTreeSet::new();
    let mut shared_edges = BTreeMap::new();

    for row in classifications {
        match &row.classification {
            WriterClassificationKind::Family { family } => {
                families.insert(family.clone());
            }
            WriterClassificationKind::ReviewedCleanup { review } => {
                cleanup_reviews.insert(review.clone());
            }
            WriterClassificationKind::ReviewedFalsePositive { review } => {
                false_positive_reviews.insert(review.clone());
            }
            WriterClassificationKind::SharedPrimitive {
                primitive,
                inbound_families,
            } => {
                shared_primitives.insert(primitive.clone());
                families.extend(inbound_families.iter().cloned());
                match shared_edges.entry(primitive.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(inbound_families.clone());
                    }
                    Entry::Occupied(entry) if entry.get() != inbound_families => {
                        return Err(VocabularyIssue::SharedPrimitiveEdges);
                    }
                    Entry::Occupied(_) => {}
                }
            }
        }
    }

    require_exact(&vocabulary.families, &families)?;
    require_exact(&vocabulary.shared_primitives, &shared_primitives)?;
    require_exact(&vocabulary.cleanup_reviews, &cleanup_reviews)?;
    require_exact(&vocabulary.false_positive_reviews, &false_positive_reviews)
}

fn require_exact(
    declared: &[WriterToken],
    referenced: &BTreeSet<WriterToken>,
) -> Result<(), VocabularyIssue> {
    let declared = declared.iter().cloned().collect::<BTreeSet<_>>();
    if !referenced.is_subset(&declared) {
        return Err(VocabularyIssue::UndeclaredReference);
    }
    if declared != *referenced {
        return Err(VocabularyIssue::UnusedDeclaration);
    }
    Ok(())
}
