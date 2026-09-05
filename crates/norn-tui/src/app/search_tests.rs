//! Literal search regressions for original graphemes, exact ownership and honest coverage.

use std::ops::Range;

use norn::session_view::{
    BodyRef, BodyRepresentation, ItemId, SessionIdentity, SessionProjection, ViewItemKind,
    ViewSource,
};
use uuid::Uuid;

use super::{
    SearchBody, SearchError, SearchHistoryCoverage, SearchQuery, SearchReport, SearchScope,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    source: ViewSource,
    item: ItemId,
    reference: BodyRef,
}

impl Fixture {
    fn new(text: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let source = ViewSource {
            session: SessionIdentity::Ephemeral(Uuid::new_v4()),
            agent_id: Uuid::new_v4(),
            parent_agent_id: None,
            store_generation: Uuid::new_v4(),
        };
        let mut projection = SessionProjection::new(source.clone());
        let item = projection.record_local_body(
            ViewItemKind::Notice,
            "search fixture",
            text,
            BodyRepresentation::Text,
        )?;
        let reference = projection
            .item(&item)
            .and_then(|item| item.bodies.first())
            .ok_or("search fixture has no owner-minted body")?
            .clone();
        Ok(Self {
            source,
            item,
            reference,
        })
    }

    fn body<'a>(&'a self, original: &'a str, complete: bool) -> SearchBody<'a> {
        SearchBody {
            item: &self.item,
            reference: &self.reference,
            original,
            complete,
        }
    }

    fn ranges(
        &self,
        original: &str,
        literal: &str,
        complete: bool,
    ) -> Result<Vec<Range<usize>>, Box<dyn std::error::Error>> {
        Ok(self
            .body(original, complete)
            .matches(SearchQuery::new(literal)?, &self.source)?
            .collect())
    }
}

fn known_history() -> SearchHistoryCoverage {
    SearchHistoryCoverage {
        older_history_not_loaded: false,
        live_coverage_uncertain: false,
    }
}

#[test]
fn query_is_nonempty_literal_case_sensitive_and_not_normalized() -> TestResult {
    assert!(matches!(SearchQuery::new(""), Err(SearchError::EmptyQuery)));
    let text = ".* Hit hit é e\u{301} aaaa";
    let fixture = Fixture::new(text)?;
    let literal = fixture.ranges(text, ".*", true)?;
    assert_eq!(literal.len(), 1);
    assert_eq!(literal.first(), Some(&(0..2)));
    let lowercase = fixture.ranges(text, "hit", true)?;
    assert_eq!(lowercase.len(), 1);
    assert_eq!(lowercase.first(), Some(&(7..10)));
    let composed = fixture.ranges(text, "é", true)?;
    assert_eq!(composed.len(), 1);
    assert_eq!(composed.first(), Some(&(11..13)));
    let overlap = fixture.ranges(text, "aa", true)?;
    assert_eq!(overlap, vec![18..20, 20..22]);
    Ok(())
}

#[test]
fn matches_require_whole_original_grapheme_edges() -> TestResult {
    let text = "e\u{301} e 👩‍💻 👩 界";
    let fixture = Fixture::new(text)?;
    let ascii = fixture.ranges(text, "e", true)?;
    assert_eq!(ascii.len(), 1);
    assert_eq!(ascii.first(), Some(&(4..5)));
    assert!(fixture.ranges(text, "\u{301}", true)?.is_empty());
    assert!(fixture.ranges(text, "💻", true)?.is_empty());
    let woman = fixture.ranges(text, "👩", true)?;
    assert_eq!(woman.len(), 1);
    assert_eq!(woman.first(), Some(&(18..22)));
    let joined = fixture.ranges(text, "👩‍💻", true)?;
    assert_eq!(joined.len(), 1);
    assert_eq!(joined.first(), Some(&(6..17)));
    Ok(())
}

#[test]
fn rejected_interior_match_does_not_hide_an_overlapping_whole_grapheme_hit() -> TestResult {
    // U+0600 has Grapheme_Cluster_Break=Prepend: the first 'a' belongs
    // to its cluster, but the later two 'a' characters are complete clusters.
    let text = "\u{600}aaa";
    let fixture = Fixture::new(text)?;
    let hits = fixture.ranges(text, "aa", true)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits.first(), Some(&(3..5)));
    let repeated = "\u{600}aaaaaa";
    let fixture = Fixture::new(repeated)?;
    assert_eq!(fixture.ranges(repeated, "aa", true)?, vec![3..5, 5..7]);
    // After a successful hit the nonoverlap contract still applies.
    assert_eq!(fixture.ranges("aaaaa", "aa", true)?, vec![0..2, 2..4]);
    Ok(())
}

#[test]
fn unfinished_final_grapheme_is_withheld_until_more_original_bytes_arrive() -> TestResult {
    let full = "hit e\u{301} end";
    let fixture = Fixture::new(full)?;
    let query = SearchQuery::new("e")?;
    let mut scan = fixture
        .body("hit e", false)
        .matches(query, &fixture.source)?;
    assert_eq!(scan.next(), None);
    let coverage = scan.coverage();
    assert_eq!(coverage.examined, 0..4);
    assert_eq!(coverage.loaded_bytes, 5);
    assert_eq!(coverage.safely_searchable_bytes, 4);
    assert!(coverage.scan_exhausted);
    assert!(!coverage.body_complete);
    assert!(fixture.ranges("hit e\u{301}", "e", false)?.is_empty());
    let completed = fixture.ranges(full, "e", true)?;
    assert_eq!(completed.len(), 1);
    assert_eq!(completed.first(), Some(&(8..9)));

    let retained = fixture.ranges("hit e", "hit", false)?;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained.first(), Some(&(0..3)));
    assert!(fixture.ranges("e", "e", false)?.is_empty());
    let complete = fixture.ranges("e", "e", true)?;
    assert_eq!(complete.len(), 1);
    assert_eq!(complete.first(), Some(&(0..1)));
    Ok(())
}

#[test]
fn observed_first_hit_keeps_unsearched_suffix_partial_without_exhausting_scan() -> TestResult {
    let text = format!("hit {} hit", "unchanged ".repeat(8192));
    let fixture = Fixture::new(&text)?;
    let query = SearchQuery::new("hit")?;
    let mut scan = fixture.body(&text, true).matches(query, &fixture.source)?;
    assert!(std::ptr::eq(scan.item(), &raw const fixture.item));
    assert!(std::ptr::eq(scan.reference(), &raw const fixture.reference));
    assert_eq!(scan.next(), Some(0..3));
    let mut report = SearchReport::new(
        &fixture.source,
        SearchScope::LoadedTranscript,
        query,
        known_history(),
    );
    let coverage = report.observe(scan)?;
    assert_eq!(coverage.examined, 0..3);
    assert_eq!(coverage.loaded_bytes, text.len());
    assert_eq!(coverage.matches_found, 1);
    assert!(!coverage.scan_exhausted);
    let summary = report.summary();
    assert_eq!(summary.body_scans, 1);
    assert_eq!(summary.matches_found, 1);
    assert_eq!(summary.partial_body_scans, 1);
    assert!(!summary.complete_within_scope());
    Ok(())
}

#[test]
fn exhausted_original_scan_preserves_logical_newlines_and_declared_scope() -> TestResult {
    let text = "alpha beta\ngamma **literal**";
    let fixture = Fixture::new(text)?;
    let query = SearchQuery::new("beta\ngamma")?;
    for scope in [
        SearchScope::LoadedTranscript,
        SearchScope::SelectedBody,
        SearchScope::RequestedOlderHistory,
    ] {
        let mut scan = fixture.body(text, true).matches(query, &fixture.source)?;
        let hits: Vec<_> = scan.by_ref().collect();
        assert_eq!(hits.len(), 1);
        let range = hits.first().ok_or("expected original newline search hit")?;
        assert_eq!(&text[range.clone()], "beta\ngamma");
        let mut report = SearchReport::new(&fixture.source, scope, query, known_history());
        let coverage = report.observe(scan)?;
        assert_eq!(coverage.examined, 0..text.len());
        assert!(coverage.scan_exhausted);
        assert!(coverage.body_complete);
        assert_eq!(report.summary().scope, scope);
        assert!(report.summary().complete_within_scope());
    }
    let markup = fixture.ranges(text, "**literal**", true)?;
    assert_eq!(markup.len(), 1);
    assert_eq!(markup.first(), Some(&(17..28)));
    Ok(())
}

#[test]
fn no_match_never_covers_unavailable_partial_older_or_uncertain_live_text() -> TestResult {
    let text = "prefix";
    let fixture = Fixture::new(text)?;
    let query = SearchQuery::new("absent")?;
    let mut report = SearchReport::new(
        &fixture.source,
        SearchScope::RequestedOlderHistory,
        query,
        SearchHistoryCoverage {
            older_history_not_loaded: true,
            live_coverage_uncertain: true,
        },
    );
    let mut scan = fixture.body(text, false).matches(query, &fixture.source)?;
    assert_eq!(scan.next(), None);
    report.observe(scan)?;
    report.unavailable(&fixture.item, &fixture.reference)?;
    let summary = report.summary();
    assert_eq!(summary.matches_found, 0);
    assert_eq!(summary.partial_body_scans, 1);
    assert_eq!(summary.unavailable_bodies, 1);
    assert!(summary.history.older_history_not_loaded);
    assert!(summary.history.live_coverage_uncertain);
    assert!(!summary.complete_within_scope());
    for history in [
        SearchHistoryCoverage {
            older_history_not_loaded: true,
            live_coverage_uncertain: false,
        },
        SearchHistoryCoverage {
            older_history_not_loaded: false,
            live_coverage_uncertain: true,
        },
    ] {
        let report = SearchReport::new(
            &fixture.source,
            SearchScope::LoadedTranscript,
            query,
            history,
        );
        assert!(!report.summary().complete_within_scope());
    }
    Ok(())
}

#[test]
fn source_validation_covers_both_item_and_exact_body_generation() -> TestResult {
    let text = "private-body-marker";
    let fixture = Fixture::new(text)?;
    let foreign = Fixture::new(text)?;
    let query = SearchQuery::new("private-query-marker")?;
    let mut wrong_generation = fixture.source.clone();
    wrong_generation.store_generation = Uuid::new_v4();
    let Err(error) = fixture.body(text, true).matches(query, &wrong_generation) else {
        return Err("foreign generation accepted".into());
    };
    assert!(matches!(error, SearchError::SourceMismatch { .. }));
    assert!(!format!("{error} {error:?}").contains("private-"));
    for body in [
        SearchBody {
            item: &foreign.item,
            ..fixture.body(text, true)
        },
        SearchBody {
            reference: &foreign.reference,
            ..fixture.body(text, true)
        },
    ] {
        assert!(matches!(
            body.matches(query, &fixture.source),
            Err(SearchError::SourceMismatch { .. })
        ));
    }
    let mut report = SearchReport::new(
        &fixture.source,
        SearchScope::SelectedBody,
        query,
        known_history(),
    );
    let previous = report.summary();
    let foreign_scan = foreign.body(text, true).matches(query, &foreign.source)?;
    assert!(matches!(
        report.observe(foreign_scan),
        Err(SearchError::SourceMismatch { .. })
    ));
    assert!(matches!(
        report.unavailable(&fixture.item, &foreign.reference),
        Err(SearchError::SourceMismatch { .. })
    ));
    assert_eq!(report.summary(), previous);
    Ok(())
}

#[test]
fn query_mismatch_and_count_overflow_leave_report_unchanged_without_payloads() -> TestResult {
    let text = "private-body-marker";
    let fixture = Fixture::new(text)?;
    let query = SearchQuery::new("private-query-marker")?;
    let mut report = SearchReport::new(
        &fixture.source,
        SearchScope::SelectedBody,
        query,
        known_history(),
    );
    let previous = report.summary();
    let scan = fixture
        .body(text, true)
        .matches(SearchQuery::new("other-private-marker")?, &fixture.source)?;
    let Err(error) = report.observe(scan) else {
        return Err("changed query accepted".into());
    };
    assert!(matches!(error, SearchError::QueryChanged));
    assert!(!format!("{error} {error:?}").contains("private-"));
    assert_eq!(report.summary(), previous);

    report.summary.body_scans = usize::MAX;
    let previous = report.summary();
    let scan = fixture.body(text, true).matches(query, &fixture.source)?;
    assert!(matches!(
        report.observe(scan),
        Err(SearchError::CounterExhausted {
            counter: "body scans",
            ..
        })
    ));
    assert_eq!(report.summary(), previous);
    Ok(())
}

#[test]
fn empty_complete_body_is_distinct_from_an_incomplete_empty_prefix() -> TestResult {
    let fixture = Fixture::new("")?;
    let query = SearchQuery::new("needle")?;
    for complete in [false, true] {
        let mut scan = fixture.body("", complete).matches(query, &fixture.source)?;
        assert_eq!(scan.next(), None);
        let mut report = SearchReport::new(
            &fixture.source,
            SearchScope::SelectedBody,
            query,
            known_history(),
        );
        let coverage = report.observe(scan)?;
        assert_eq!(coverage.examined, 0..0);
        assert!(coverage.scan_exhausted);
        assert_eq!(report.summary().complete_within_scope(), complete);
    }
    Ok(())
}
