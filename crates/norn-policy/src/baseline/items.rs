//! Stable item-group facts without source-order identity.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;
use thiserror::Error;

use super::model::{OriginId, identity_digest};
use crate::digest::{Digest, digest_bytes};
use crate::facts;
use crate::path::RepositoryPath;
use crate::rust::RustItemProjection;

/// One stable path/base/content group with classification multiplicities.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ItemGroupFact {
    origin_id: OriginId,
    path: RepositoryPath,
    base_identity: Digest,
    content: Digest,
    production_count: u32,
    test_only_count: u32,
}

/// One stable group whose production occurrences became test-only.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ItemReclassification {
    origin_id: OriginId,
    path: RepositoryPath,
    hidden_count: u32,
}

impl ItemReclassification {
    /// Return the immutable group identity.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the source containing the stable group.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the number of production occurrences reclassified as test-only.
    #[must_use]
    pub const fn hidden_count(&self) -> u32 {
        self.hidden_count
    }
}

impl ItemGroupFact {
    /// Construct one stable aggregate without a source-order ordinal.
    ///
    /// # Errors
    ///
    /// Rejects a group with no production or test occurrence.
    pub fn new(
        path: RepositoryPath,
        base_identity: Digest,
        content: Digest,
        production_count: u32,
        test_only_count: u32,
    ) -> Result<Self, ItemGroupError> {
        if production_count == 0 && test_only_count == 0 {
            return Err(ItemGroupError::Empty);
        }
        let origin_id = item_origin_id(
            &path,
            base_identity,
            content,
            production_count,
            test_only_count,
        );
        Ok(Self {
            origin_id,
            path,
            base_identity,
            content,
            production_count,
            test_only_count,
        })
    }

    /// Convert one canonical Rust item projection for its source path.
    ///
    /// # Errors
    ///
    /// Rejects an impossible empty projection aggregate.
    pub fn from_projection(
        path: &RepositoryPath,
        projection: &RustItemProjection,
    ) -> Result<Self, ItemGroupError> {
        Self::new(
            path.clone(),
            projection.base_identity(),
            projection.content(),
            projection.production_count(),
            projection.test_only_count(),
        )
    }

    /// Return the immutable aggregate origin identity, including both counts.
    #[must_use]
    pub const fn origin_id(&self) -> OriginId {
        self.origin_id
    }

    /// Return the repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the normalized structural item identity.
    #[must_use]
    pub const fn base_identity(&self) -> Digest {
        self.base_identity
    }

    /// Return the normalized item content digest.
    #[must_use]
    pub const fn content(&self) -> Digest {
        self.content
    }

    /// Return the production occurrence multiplicity.
    #[must_use]
    pub const fn production_count(&self) -> u32 {
        self.production_count
    }

    /// Return the test-only occurrence multiplicity.
    #[must_use]
    pub const fn test_only_count(&self) -> u32 {
        self.test_only_count
    }
}

impl TryFrom<&facts::SourceItemFact> for ItemGroupFact {
    type Error = ItemGroupError;

    fn try_from(value: &facts::SourceItemFact) -> Result<Self, Self::Error> {
        Self::from_projection(&value.path, &value.item)
    }
}

/// Compare immutable and current aggregate multiplicities without ordinals.
///
/// A finding requires both a production-count reduction and a test-only-count
/// increase for the same canonical item content. Exact path-bound groups are
/// retained as origin evidence, while the secondary semantic identity detects
/// transfers across files and enclosing modules. Removing production without
/// adding the same item to tests, or adding a test duplicate without reducing
/// production, is not treated as reclassification.
///
/// # Errors
///
/// Rejects either input when its stable keys are unsorted or duplicated.
pub fn compare_item_groups(
    origin: &[ItemGroupFact],
    current: &[ItemGroupFact],
) -> Result<Vec<ItemReclassification>, ItemComparisonError> {
    validate_group_order(origin, ItemComparisonSide::Origin)?;
    validate_group_order(current, ItemComparisonSide::Current)?;

    let mut transfer_counts = semantic_transfer_counts(origin, current);
    let mut findings = Vec::new();
    for baseline in origin {
        let exact_reduction = baseline.production_count().saturating_sub(
            exact_group(current, baseline).map_or(0, ItemGroupFact::production_count),
        );
        let hidden_count = transfer_counts
            .get_mut(&semantic_item_identity(baseline.content()))
            .map_or(0, |counts| counts.hidden_from(exact_reduction));
        if hidden_count > 0 {
            findings.push(ItemReclassification {
                origin_id: baseline.origin_id(),
                path: baseline.path().clone(),
                hidden_count,
            });
        }
    }
    findings.sort();
    Ok(findings)
}

fn exact_group<'a>(
    groups: &'a [ItemGroupFact],
    target: &ItemGroupFact,
) -> Option<&'a ItemGroupFact> {
    match groups.binary_search_by(|candidate| group_order(candidate, target)) {
        Ok(index) => groups.get(index),
        Err(_) => None,
    }
}

#[derive(Default)]
struct SemanticTransferCounts {
    production_gains: VecDeque<u32>,
    origin: VecDeque<u32>,
    current: VecDeque<u32>,
}

impl SemanticTransferCounts {
    fn cancel_origin(&mut self) {
        let mut retained = VecDeque::new();
        while let Some(mut current) = self.current.pop_front() {
            while current > 0 {
                let Some(origin) = self.origin.front_mut() else {
                    break;
                };
                let cancelled = current.min(*origin);
                current -= cancelled;
                *origin -= cancelled;
                if *origin == 0 {
                    self.origin.pop_front();
                }
            }
            if current > 0 {
                retained.push_back(current);
            }
        }
        self.current = retained;
    }

    fn hidden_from(&mut self, mut exact_reduction: u32) -> u32 {
        consume(&mut self.production_gains, &mut exact_reduction);
        let mut requested = exact_reduction;
        let original = requested;
        consume(&mut self.current, &mut requested);
        original - requested
    }
}

fn consume(available: &mut VecDeque<u32>, requested: &mut u32) {
    while *requested > 0 {
        let Some(count) = available.front_mut() else {
            break;
        };
        let taken = (*requested).min(*count);
        *requested -= taken;
        *count -= taken;
        if *count == 0 {
            available.pop_front();
        }
    }
}

fn semantic_transfer_counts(
    origin: &[ItemGroupFact],
    current: &[ItemGroupFact],
) -> BTreeMap<Digest, SemanticTransferCounts> {
    let mut counts = BTreeMap::<Digest, SemanticTransferCounts>::new();
    for group in origin {
        if group.test_only_count() > 0 {
            counts
                .entry(semantic_item_identity(group.content()))
                .or_default()
                .origin
                .push_back(group.test_only_count());
        }
    }
    for group in current {
        let production_gain = group
            .production_count()
            .saturating_sub(exact_group(origin, group).map_or(0, ItemGroupFact::production_count));
        if production_gain > 0 {
            counts
                .entry(semantic_item_identity(group.content()))
                .or_default()
                .production_gains
                .push_back(production_gain);
        }
        if group.test_only_count() > 0 {
            counts
                .entry(semantic_item_identity(group.content()))
                .or_default()
                .current
                .push_back(group.test_only_count());
        }
    }
    for value in counts.values_mut() {
        value.cancel_origin();
    }
    counts
}

fn semantic_item_identity(content: Digest) -> Digest {
    const DOMAIN: &[u8] = b"production-item-semantic";
    let mut encoded = Vec::with_capacity(DOMAIN.len() + 1 + content.as_bytes().len());
    encoded.extend_from_slice(DOMAIN);
    encoded.push(0);
    encoded.extend_from_slice(content.as_bytes());
    digest_bytes(&encoded)
}

fn validate_group_order(
    groups: &[ItemGroupFact],
    side: ItemComparisonSide,
) -> Result<(), ItemComparisonError> {
    for (index, pair) in groups.windows(2).enumerate() {
        if group_order(&pair[0], &pair[1]).is_ge() {
            return Err(ItemComparisonError {
                side,
                index: index + 1,
            });
        }
    }
    Ok(())
}

fn group_order(left: &ItemGroupFact, right: &ItemGroupFact) -> std::cmp::Ordering {
    (left.path(), left.base_identity(), left.content()).cmp(&(
        right.path(),
        right.base_identity(),
        right.content(),
    ))
}

fn item_origin_id(
    path: &RepositoryPath,
    base_identity: Digest,
    content: Digest,
    production_count: u32,
    test_only_count: u32,
) -> OriginId {
    identity_digest(
        b"production-item-group",
        &[
            path.as_str().as_bytes(),
            base_identity.as_bytes(),
            content.as_bytes(),
            &production_count.to_be_bytes(),
            &test_only_count.to_be_bytes(),
        ],
    )
}

/// Invalid stable item aggregate.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ItemGroupError {
    /// The aggregate had no occurrences in either classification.
    #[error("item group has no production or test occurrence")]
    Empty,
}

/// Side of an invalid item comparison input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemComparisonSide {
    /// Immutable origin facts.
    Origin,
    /// Current repository facts.
    Current,
}

/// Item comparison input was not a strict stable-key sequence.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("{side:?} item groups are not strictly sorted at row {index}")]
pub struct ItemComparisonError {
    side: ItemComparisonSide,
    index: usize,
}

impl ItemComparisonError {
    /// Return which input sequence was invalid.
    #[must_use]
    pub const fn side(self) -> ItemComparisonSide {
        self.side
    }

    /// Return the first invalid row.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}
