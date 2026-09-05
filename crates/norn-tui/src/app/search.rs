//! Lazy literal search over approved original body prefixes; no loading, rendering or runtime effects.

use std::iter::Peekable;
use std::ops::Range;

use norn::session_view::{BodyOrigin, BodyRef, ItemId, ViewSource};
use unicode_segmentation::{GraphemeIndices, UnicodeSegmentation};

/// The operator-selected scope. None of these tags grants a history/body read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SearchScope {
    LoadedTranscript,
    SelectedBody,
    RequestedOlderHistory,
}

/// A nonempty literal, case-sensitive query; no regex or normalization is applied.
#[derive(Clone, Copy)]
pub(super) struct SearchQuery<'a> {
    literal: &'a str,
}

impl<'a> SearchQuery<'a> {
    pub fn new(literal: &'a str) -> Result<Self, SearchError> {
        if literal.is_empty() {
            Err(SearchError::EmptyQuery)
        } else {
            Ok(Self { literal })
        }
    }
}

/// One approved body from its currently accepted owning item. The original
/// bytes are a contiguous loaded prefix starting at zero, not wrapped display.
/// An old cache entry alone does not establish that this revision is current.
#[derive(Clone, Copy)]
pub(super) struct SearchBody<'a> {
    pub item: &'a ItemId,
    pub reference: &'a BodyRef,
    pub original: &'a str,
    pub complete: bool,
}

impl<'a> SearchBody<'a> {
    /// Matches are nonoverlapping original ranges with whole-grapheme edges.
    /// Evaluation stays lazy; stopping iteration retains an incomplete scan.
    pub fn matches<'q>(
        self,
        query: SearchQuery<'q>,
        source: &ViewSource,
    ) -> Result<BodyMatches<'a, 'q>, SearchError> {
        validate_source(source, self.item, self.reference)?;
        let safe_end = if self.complete {
            self.original.len()
        } else {
            // More bytes can extend the final cluster. Its start is known, but
            // its current end is not yet a proven original grapheme boundary.
            self.original
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(start, _)| start)
        };
        let searchable = &self.original[..safe_end];
        Ok(BodyMatches {
            body: self,
            query,
            candidates: searchable.bytes().enumerate(),
            fallback: literal_fallback(query.literal.as_bytes()),
            matched: 0,
            start_boundaries: searchable.grapheme_indices(true).peekable(),
            end_boundaries: searchable.grapheme_indices(true).peekable(),
            coverage: BodySearchCoverage {
                examined: 0..0,
                loaded_bytes: self.original.len(),
                safely_searchable_bytes: safe_end,
                matches_found: 0,
                scan_exhausted: false,
                body_complete: self.complete,
            },
        })
    }
}

/// Measured progress through one exact body revision, not through a whole session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BodySearchCoverage {
    pub examined: Range<usize>,
    pub loaded_bytes: usize,
    pub safely_searchable_bytes: usize,
    pub matches_found: usize,
    pub scan_exhausted: bool,
    pub body_complete: bool,
}

/// Borrowed candidate and boundary iterators; no match list or body copy is retained.
/// Matching is linear in source plus query bytes, with one query-sized fallback table.
pub(super) struct BodyMatches<'a, 'q> {
    body: SearchBody<'a>,
    query: SearchQuery<'q>,
    candidates: std::iter::Enumerate<std::str::Bytes<'a>>,
    fallback: Vec<usize>,
    matched: usize,
    start_boundaries: Peekable<GraphemeIndices<'a>>,
    end_boundaries: Peekable<GraphemeIndices<'a>>,
    coverage: BodySearchCoverage,
}

impl BodyMatches<'_, '_> {
    pub const fn item(&self) -> &ItemId {
        self.body.item
    }

    pub const fn reference(&self) -> &BodyRef {
        self.body.reference
    }

    pub fn coverage(&self) -> BodySearchCoverage {
        self.coverage.clone()
    }
}

impl Iterator for BodyMatches<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let literal = self.query.literal.as_bytes();
        for (offset, byte) in self.candidates.by_ref() {
            while self.matched > 0 && literal[self.matched] != byte {
                self.matched = self.fallback[self.matched - 1];
            }
            if literal[self.matched] == byte {
                self.matched += 1;
            }
            let end = offset + 1;
            self.coverage.examined.end = end;
            if self.matched != literal.len() {
                continue;
            }
            let start = end - literal.len();
            // Rejected byte matches may overlap a later whole-grapheme hit.
            // Only an accepted hit resets matching to enforce nonoverlap.
            self.matched = self.fallback[self.matched - 1];
            let safe_end = self.coverage.safely_searchable_bytes;
            if is_boundary(&mut self.start_boundaries, start, safe_end)
                && is_boundary(&mut self.end_boundaries, end, safe_end)
            {
                self.matched = 0;
                // Nonempty, nonoverlapping accepted matches bound this count
                // by source byte length without an invented search limit.
                self.coverage.matches_found += 1;
                return Some(start..end);
            }
        }
        self.coverage.examined.end = self.coverage.safely_searchable_bytes;
        self.coverage.scan_exhausted = true;
        None
    }
}

fn is_boundary(
    boundaries: &mut Peekable<GraphemeIndices<'_>>,
    offset: usize,
    safe_end: usize,
) -> bool {
    if offset == safe_end {
        return true;
    }
    while boundaries.peek().is_some_and(|(start, _)| *start < offset) {
        boundaries.next();
    }
    boundaries.peek().is_some_and(|(start, _)| *start == offset)
}

fn literal_fallback(literal: &[u8]) -> Vec<usize> {
    let mut fallback = vec![0; literal.len()];
    let mut matched = 0;
    for offset in 1..literal.len() {
        while matched > 0 && literal[matched] != literal[offset] {
            matched = fallback[matched - 1];
        }
        if literal[matched] == literal[offset] {
            matched += 1;
        }
        fallback[offset] = matched;
    }
    fallback
}

/// Coverage supplied explicitly by the frontend's accepted history/live owners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SearchHistoryCoverage {
    pub older_history_not_loaded: bool,
    pub live_coverage_uncertain: bool,
}

/// A summary always names its scope and every known unsearched dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SearchSummary {
    pub scope: SearchScope,
    pub body_scans: usize,
    pub matches_found: usize,
    pub partial_body_scans: usize,
    pub unavailable_bodies: usize,
    pub history: SearchHistoryCoverage,
}

impl SearchSummary {
    /// True only for this declared scope, with no known body/history/live gap.
    /// A loaded-scope summary never becomes a claim about an entire session.
    pub const fn complete_within_scope(&self) -> bool {
        self.partial_body_scans == 0
            && self.unavailable_bodies == 0
            && !self.history.older_history_not_loaded
            && !self.history.live_coverage_uncertain
    }
}

/// Aggregate consumed scan observations, not original strings or a second hit store.
pub(super) struct SearchReport<'q> {
    source: ViewSource,
    query: SearchQuery<'q>,
    summary: SearchSummary,
}

impl<'q> SearchReport<'q> {
    pub fn new(
        source: &ViewSource,
        scope: SearchScope,
        query: SearchQuery<'q>,
        history: SearchHistoryCoverage,
    ) -> Self {
        Self {
            source: source.clone(),
            query,
            summary: SearchSummary {
                scope,
                body_scans: 0,
                matches_found: 0,
                partial_body_scans: 0,
                unavailable_bodies: 0,
                history,
            },
        }
    }

    /// Consume the actual observation without implicitly exhausting its iterator.
    /// A caller that stops after its desired hit reports that partial scan honestly.
    pub fn observe(
        &mut self,
        scan: BodyMatches<'_, '_>,
    ) -> Result<BodySearchCoverage, SearchError> {
        validate_source(&self.source, scan.item(), scan.reference())?;
        if scan.query.literal != self.query.literal {
            return Err(SearchError::QueryChanged);
        }
        let coverage = scan.coverage;
        let body_scans = self.add(self.summary.body_scans, 1, "body scans")?;
        let matches_found = self.add(
            self.summary.matches_found,
            coverage.matches_found,
            "matches",
        )?;
        let partial_body_scans = self.add(
            self.summary.partial_body_scans,
            usize::from(!coverage.scan_exhausted || !coverage.body_complete),
            "partial body scans",
        )?;
        self.summary.body_scans = body_scans;
        self.summary.matches_found = matches_found;
        self.summary.partial_body_scans = partial_body_scans;
        Ok(coverage)
    }

    /// Record an explicitly encountered missing body; no read is attempted.
    pub fn unavailable(&mut self, item: &ItemId, reference: &BodyRef) -> Result<(), SearchError> {
        validate_source(&self.source, item, reference)?;
        self.summary.unavailable_bodies =
            self.add(self.summary.unavailable_bodies, 1, "unavailable bodies")?;
        Ok(())
    }

    pub const fn summary(&self) -> SearchSummary {
        self.summary
    }

    fn add(
        &self,
        previous: usize,
        increment: usize,
        counter: &'static str,
    ) -> Result<usize, SearchError> {
        previous
            .checked_add(increment)
            .ok_or_else(|| SearchError::CounterExhausted {
                owner: Box::new(self.source.clone()),
                counter,
            })
    }
}

/// Identity and query errors contain no searched body or query contents.
#[derive(Debug, thiserror::Error)]
pub(super) enum SearchError {
    #[error("literal search requires a nonempty query")]
    EmptyQuery,
    #[error("body scan belongs to a different literal search query")]
    QueryChanged,
    #[error("search source {actual:?} does not match owner {expected:?}")]
    SourceMismatch {
        expected: Box<ViewSource>,
        actual: Box<ViewSource>,
    },
    #[error("search {counter} count exhausted for source {owner:?}")]
    CounterExhausted {
        owner: Box<ViewSource>,
        counter: &'static str,
    },
}

fn validate_source(
    source: &ViewSource,
    item: &ItemId,
    reference: &BodyRef,
) -> Result<(), SearchError> {
    let item_source = match item {
        ItemId::Committed { cursor, .. } => cursor.source(),
        ItemId::Provisional(key) => &key.source,
        ItemId::Local { source, .. } => source,
    };
    let body_source = match reference.origin() {
        BodyOrigin::Committed { cursor, .. } => cursor.source(),
        BodyOrigin::Provisional { key, .. } => &key.source,
        BodyOrigin::Local { source, .. } => source,
    };
    for actual in [item_source, body_source] {
        if actual != source {
            return Err(SearchError::SourceMismatch {
                expected: Box::new(source.clone()),
                actual: Box::new(actual.clone()),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
