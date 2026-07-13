//! Bounded streaming scanner backing the [`super::read`] tool.
//!
//! The read tool used to load the entire file into memory before any
//! budget was applied, so a multi-gigabyte file could exhaust the
//! process. This module renders the requested window while streaming the
//! file in fixed-size chunks: memory held at any moment is bounded by
//! the configured [`ToolOutputBudget`] (the rendered window plus one
//! per-line retention cap) and one I/O chunk, never by the file size.
//!
//! Classification parity with the old whole-file path is preserved:
//! binary detection scans the first [`BINARY_SCAN_BYTES`] bytes for a
//! NUL before any UTF-8 decoding, invalid UTF-8 anywhere in the file is
//! reported (the scan continues past the rendered window), and
//! `total_lines` is exact because line counting also continues to EOF —
//! only *retention* stops once the window budgets are exhausted.

use std::path::Path;

use tokio::io::AsyncReadExt;

use crate::resource::{DescriptorGovernor, PRIVATE_FS_OPERATION_PEAK};
use crate::tool::output_budget::ToolOutputBudget;

/// Number of leading bytes scanned for NUL bytes when classifying binary
/// files; also the streaming chunk size.
pub(super) const BINARY_SCAN_BYTES: usize = 8192;

/// Substring patterns whose density marks Cargo fingerprint noise.
pub(super) const FINGERPRINT_PATTERNS: [&str; 3] = [
    "target/debug/.fingerprint",
    "target/release/.fingerprint",
    ".fingerprint/",
];

/// Outcome of streaming one file through the scanner.
pub(super) enum ScannedFile {
    /// A NUL byte appeared in the leading scan window.
    Binary,
    /// The byte stream is not valid UTF-8.
    NotUtf8 {
        /// Human-readable description with the absolute byte offset.
        message: String,
    },
    /// UTF-8 text, rendered into the budgeted window.
    Text(RenderedRead),
}

/// The rendered window and its bookkeeping.
#[derive(Debug)]
pub(super) struct RenderedRead {
    pub(super) content: String,
    pub(super) offset: u64,
    pub(super) effective_line_limit: u64,
    pub(super) content_char_limit: usize,
    pub(super) returned_lines: u64,
    pub(super) total_lines: u64,
    pub(super) content_chars: usize,
    pub(super) max_line_chars: usize,
    pub(super) next_offset: Option<u64>,
    pub(super) truncated_by_line_limit: bool,
    pub(super) truncated_by_char_limit: bool,
    pub(super) truncated_long_lines: u64,
    /// Total [`FINGERPRINT_PATTERNS`] hits across the whole file.
    pub(super) fingerprint_hits: usize,
}

impl RenderedRead {
    pub(super) fn truncated(&self) -> bool {
        self.truncated_by_line_limit
            || self.truncated_by_char_limit
            || self.truncated_long_lines > 0
    }

    pub(super) fn truncated_by(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.truncated_by_line_limit {
            reasons.push("line_limit");
        }
        if self.truncated_by_char_limit {
            reasons.push("char_limit");
        }
        if self.truncated_long_lines > 0 {
            reasons.push("long_line_limit");
        }
        reasons
    }
}

/// Stream `path` through the classifier and window renderer.
///
/// `offset` is the 1-based starting line (defaults to 1); `limit` caps
/// lines but cannot bypass the character budget.
///
/// # Errors
///
/// Returns the underlying I/O error when the file cannot be opened or
/// read.
pub(super) async fn scan_file(
    path: &Path,
    offset: Option<u64>,
    limit: Option<u64>,
    budget: ToolOutputBudget,
) -> std::io::Result<ScannedFile> {
    let permit = DescriptorGovernor::global()
        .and_then(|governor| governor.try_acquire(PRIVATE_FS_OPERATION_PEAK))
        .map_err(std::io::Error::other)?;
    let path = path.to_path_buf();
    let (file, _permit) =
        tokio::task::spawn_blocking(move || std::fs::File::open(path).map(|file| (file, permit)))
            .await
            .map_err(std::io::Error::other)??;
    let mut file = tokio::fs::File::from_std(file);
    let mut chunk = vec![0u8; BINARY_SCAN_BYTES];

    // Phase 1: buffer the binary-scan prefix so NUL detection takes
    // precedence over UTF-8 classification exactly as the whole-file
    // implementation ordered its checks.
    let mut prefix: Vec<u8> = Vec::with_capacity(BINARY_SCAN_BYTES);
    let mut at_eof = false;
    while prefix.len() < BINARY_SCAN_BYTES {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            at_eof = true;
            break;
        }
        prefix.extend_from_slice(&chunk[..n]);
    }
    let scan_len = prefix.len().min(BINARY_SCAN_BYTES);
    if prefix[..scan_len].contains(&0u8) {
        return Ok(ScannedFile::Binary);
    }

    // Phase 2: incremental UTF-8 decode + line rendering. `pending`
    // holds at most one chunk plus an incomplete trailing character.
    let mut renderer = LineRenderer::new(offset, limit, budget);
    let mut pending = prefix;
    let mut consumed: u64 = 0;
    loop {
        if let Err(message) = drain_pending(&mut pending, &mut renderer, at_eof, &mut consumed) {
            return Ok(ScannedFile::NotUtf8 { message });
        }
        if at_eof {
            break;
        }
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            at_eof = true;
        } else {
            pending.extend_from_slice(&chunk[..n]);
        }
    }

    Ok(ScannedFile::Text(renderer.finish()))
}

/// Decode as much of `pending` as is valid UTF-8 into the renderer,
/// retaining an incomplete trailing character for the next chunk.
/// `consumed` tracks the absolute offset of `pending[0]` for error
/// reporting.
fn drain_pending(
    pending: &mut Vec<u8>,
    renderer: &mut LineRenderer,
    at_eof: bool,
    consumed: &mut u64,
) -> Result<(), String> {
    let (valid_len, hard_error) = match std::str::from_utf8(pending) {
        Ok(s) => {
            renderer.push_str(s);
            (pending.len(), false)
        }
        Err(e) => {
            let valid = e.valid_up_to();
            match std::str::from_utf8(&pending[..valid]) {
                Ok(head) => renderer.push_str(head),
                Err(inner) => {
                    // Unreachable by `valid_up_to`'s contract; surface it
                    // rather than assuming.
                    return Err(format!("file is not valid UTF-8: {inner}"));
                }
            }
            (valid, e.error_len().is_some() || at_eof)
        }
    };
    if hard_error {
        return Err(format!(
            "file is not valid UTF-8: invalid byte sequence at offset {}",
            *consumed + u64::try_from(valid_len).unwrap_or(u64::MAX),
        ));
    }
    pending.drain(..valid_len);
    *consumed = consumed.saturating_add(u64::try_from(valid_len).unwrap_or(u64::MAX));
    Ok(())
}

/// Incremental `cat -n` window renderer.
///
/// Retention is budget-bounded: per line at most
/// `read_line_char_limit` characters are kept, the rendered window stops
/// growing once the line/char budgets are hit, and every later line only
/// bumps counters. Truncation flags mirror the original whole-file
/// renderer: the line-limit flag requires a next line to actually exist,
/// and the line that overflows the char budget still contributes to
/// `max_line_chars` / `truncated_long_lines` without being emitted.
struct LineRenderer {
    skip: usize,
    effective_line_limit: u64,
    content_char_limit: usize,
    line_char_limit: usize,
    offset: u64,
    out: String,
    line_idx: usize,
    acc_text: String,
    acc_chars: usize,
    collecting: bool,
    content_chars: usize,
    returned_lines: u64,
    total_lines: u64,
    max_line_chars: usize,
    next_offset: Option<u64>,
    truncated_by_line_limit: bool,
    truncated_by_char_limit: bool,
    truncated_long_lines: u64,
    fingerprints: SubstringCounter,
}

impl LineRenderer {
    fn new(offset: Option<u64>, limit: Option<u64>, budget: ToolOutputBudget) -> Self {
        let offset_usize = match offset {
            None | Some(0) => 1usize,
            Some(n) => usize::try_from(n).unwrap_or(usize::MAX),
        };
        Self {
            skip: offset_usize.saturating_sub(1),
            effective_line_limit: limit
                .unwrap_or(budget.read_default_line_limit)
                .min(budget.read_hard_line_limit),
            content_char_limit: budget
                .read_output_char_limit
                .min(budget.read_hard_output_char_limit),
            line_char_limit: budget.read_line_char_limit,
            offset: u64::try_from(offset_usize).unwrap_or(u64::MAX),
            out: String::new(),
            line_idx: 0,
            acc_text: String::new(),
            acc_chars: 0,
            collecting: true,
            content_chars: 0,
            returned_lines: 0,
            total_lines: 0,
            max_line_chars: 0,
            next_offset: None,
            truncated_by_line_limit: false,
            truncated_by_char_limit: false,
            truncated_long_lines: 0,
            fingerprints: SubstringCounter::new(&FINGERPRINT_PATTERNS),
        }
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                self.fingerprints.reset_line();
                self.finish_line();
            } else {
                self.fingerprints.push(c);
                self.push_char(c);
            }
        }
    }

    fn push_char(&mut self, c: char) {
        if self.collecting && self.line_idx >= self.skip && self.acc_chars < self.line_char_limit {
            self.acc_text.push(c);
        }
        self.acc_chars = self.acc_chars.saturating_add(1);
    }

    fn finish_line(&mut self) {
        self.total_lines = self.total_lines.saturating_add(1);
        let idx = self.line_idx;
        self.line_idx = self.line_idx.saturating_add(1);
        let text = std::mem::take(&mut self.acc_text);
        let chars = std::mem::take(&mut self.acc_chars);

        if !self.collecting || idx < self.skip {
            return;
        }
        // A line beyond a full window proves more content exists — the
        // condition for flagging line-limit truncation.
        if self.returned_lines >= self.effective_line_limit {
            self.truncated_by_line_limit = true;
            self.next_offset = Some(u64::try_from(idx + 1).unwrap_or(u64::MAX));
            self.collecting = false;
            return;
        }

        let lineno = idx + 1;
        self.max_line_chars = self.max_line_chars.max(chars);
        let line_truncated = chars > self.line_char_limit;
        if line_truncated {
            self.truncated_long_lines = self.truncated_long_lines.saturating_add(1);
        }
        let suffix = if line_truncated {
            format!(" … [line truncated; original_chars={chars}]")
        } else {
            String::new()
        };
        let candidate = format!("{lineno}\t{text}{suffix}\n");
        let candidate_chars = candidate.chars().count();
        if self.content_chars.saturating_add(candidate_chars) > self.content_char_limit {
            self.truncated_by_char_limit = true;
            self.next_offset = Some(u64::try_from(lineno).unwrap_or(u64::MAX));
            self.collecting = false;
            return;
        }

        self.out.push_str(&candidate);
        self.content_chars = self.content_chars.saturating_add(candidate_chars);
        self.returned_lines = self.returned_lines.saturating_add(1);
    }

    fn finish(mut self) -> RenderedRead {
        // A trailing line without a newline still counts and renders.
        if self.acc_chars > 0 {
            self.finish_line();
        }
        RenderedRead {
            content: self.out,
            offset: self.offset,
            effective_line_limit: self.effective_line_limit,
            content_char_limit: self.content_char_limit,
            returned_lines: self.returned_lines,
            total_lines: self.total_lines,
            content_chars: self.content_chars,
            max_line_chars: self.max_line_chars,
            next_offset: self.next_offset,
            truncated_by_line_limit: self.truncated_by_line_limit,
            truncated_by_char_limit: self.truncated_by_char_limit,
            truncated_long_lines: self.truncated_long_lines,
            fingerprint_hits: self.fingerprints.total(),
        }
    }
}

/// Streaming multi-pattern substring counter (KMP per pattern), fed one
/// character at a time so pattern hits are counted across the whole scan
/// without retaining line text. Patterns never span lines, so the state
/// resets on newline.
struct SubstringCounter {
    patterns: Vec<PatternState>,
}

struct PatternState {
    needle: Vec<char>,
    /// KMP failure table: `failure[i]` is the length of the longest
    /// proper prefix of `needle[..=i]` that is also a suffix of it.
    failure: Vec<usize>,
    matched: usize,
    hits: usize,
}

impl SubstringCounter {
    fn new(patterns: &[&str]) -> Self {
        Self {
            patterns: patterns
                .iter()
                .map(|p| {
                    let needle: Vec<char> = p.chars().collect();
                    let failure = kmp_failure_table(&needle);
                    PatternState {
                        needle,
                        failure,
                        matched: 0,
                        hits: 0,
                    }
                })
                .collect(),
        }
    }

    fn push(&mut self, c: char) {
        for state in &mut self.patterns {
            while state.matched > 0 && state.needle[state.matched] != c {
                state.matched = state.failure[state.matched - 1];
            }
            if state.needle[state.matched] == c {
                state.matched += 1;
            }
            if state.matched == state.needle.len() {
                state.hits = state.hits.saturating_add(1);
                state.matched = state.failure[state.matched - 1];
            }
        }
    }

    fn reset_line(&mut self) {
        for state in &mut self.patterns {
            state.matched = 0;
        }
    }

    fn total(&self) -> usize {
        self.patterns
            .iter()
            .fold(0usize, |acc, s| acc.saturating_add(s.hits))
    }
}

/// Classic KMP failure (border) table over `needle`.
fn kmp_failure_table(needle: &[char]) -> Vec<usize> {
    let mut failure = vec![0usize; needle.len()];
    let mut k = 0usize;
    for i in 1..needle.len() {
        while k > 0 && needle[i] != needle[k] {
            k = failure[k - 1];
        }
        if needle[i] == needle[k] {
            k += 1;
        }
        failure[i] = k;
    }
    failure
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::fmt::Write as _;

    use tempfile::tempdir;

    use super::*;

    async fn scan_str(
        content: &[u8],
        offset: Option<u64>,
        limit: Option<u64>,
        budget: ToolOutputBudget,
    ) -> ScannedFile {
        let dir = tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, content).unwrap();
        scan_file(&path, offset, limit, budget).await.expect("io")
    }

    fn text(scanned: ScannedFile) -> RenderedRead {
        match scanned {
            ScannedFile::Text(rendered) => rendered,
            ScannedFile::Binary => panic!("unexpected binary classification"),
            ScannedFile::NotUtf8 { message } => panic!("unexpected non-UTF-8: {message}"),
        }
    }

    #[tokio::test]
    async fn renders_window_and_counts_all_lines() {
        let mut body = String::new();
        for i in 1..=300 {
            let _ = writeln!(body, "line{i}");
        }
        let rendered = text(
            scan_str(
                body.as_bytes(),
                None,
                None,
                ToolOutputBudget::for_context_window(None),
            )
            .await,
        );
        assert_eq!(rendered.total_lines, 300);
        assert_eq!(rendered.returned_lines, 200);
        assert!(rendered.truncated_by_line_limit);
        assert_eq!(rendered.next_offset, Some(201));
        assert!(rendered.content.contains("200\tline200\n"));
        assert!(!rendered.content.contains("201\tline201\n"));
    }

    #[tokio::test]
    async fn exact_window_fit_is_not_flagged_truncated() {
        // Exactly as many lines as the limit: no next line exists, so no
        // line-limit truncation is reported.
        let body = "a\nb\nc\n";
        let rendered = text(
            scan_str(
                body.as_bytes(),
                None,
                Some(3),
                ToolOutputBudget::for_context_window(None),
            )
            .await,
        );
        assert_eq!(rendered.total_lines, 3);
        assert_eq!(rendered.returned_lines, 3);
        assert!(!rendered.truncated());
        assert_eq!(rendered.next_offset, None);
    }

    #[tokio::test]
    async fn nul_in_leading_window_classifies_binary() {
        let scanned = scan_str(
            &[b'h', b'i', 0u8, b'!'],
            None,
            None,
            ToolOutputBudget::for_context_window(None),
        )
        .await;
        assert!(matches!(scanned, ScannedFile::Binary));
    }

    #[tokio::test]
    async fn invalid_utf8_after_window_is_still_detected() {
        // The invalid byte sits far past the rendered window; the scan
        // must keep validating to EOF.
        let mut body: Vec<u8> = Vec::new();
        for i in 0..50 {
            body.extend_from_slice(format!("line{i}\n").as_bytes());
        }
        body.extend_from_slice(&[0xFF, 0xFE]);
        let scanned = scan_str(
            &body,
            None,
            Some(2),
            ToolOutputBudget::for_context_window(None),
        )
        .await;
        match scanned {
            ScannedFile::NotUtf8 { message } => {
                assert!(message.contains("not valid UTF-8"), "{message}");
            }
            _ => panic!("expected NotUtf8"),
        }
    }

    #[tokio::test]
    async fn multibyte_chars_split_across_chunk_boundaries_survive() {
        // 8192-byte chunks: place a 3-byte char straddling the boundary.
        let mut body = "a".repeat(BINARY_SCAN_BYTES - 1);
        body.push('€'); // 3 bytes: spans the first chunk boundary
        body.push_str("tail\n");
        let rendered = text(
            scan_str(
                body.as_bytes(),
                None,
                None,
                ToolOutputBudget {
                    read_default_line_limit: 10,
                    read_hard_line_limit: 10,
                    read_output_char_limit: 32_000,
                    read_hard_output_char_limit: 32_000,
                    read_line_char_limit: 16_000,
                    model_output_inline_char_limit: 64_000,
                },
            )
            .await,
        );
        assert_eq!(rendered.total_lines, 1);
        assert!(rendered.content.contains("€tail"), "boundary char intact");
    }

    #[tokio::test]
    async fn offset_deep_into_large_file_returns_correct_window() {
        let mut body = String::new();
        for i in 1..=10_000 {
            let _ = writeln!(body, "row {i}");
        }
        let rendered = text(
            scan_str(
                body.as_bytes(),
                Some(9_500),
                Some(3),
                ToolOutputBudget::for_context_window(None),
            )
            .await,
        );
        assert_eq!(rendered.returned_lines, 3);
        assert!(rendered.content.starts_with("9500\trow 9500\n"));
        assert!(rendered.content.contains("9502\trow 9502\n"));
        assert_eq!(rendered.total_lines, 10_000);
        assert_eq!(rendered.next_offset, Some(9_503));
        assert!(rendered.truncated_by_line_limit);
    }

    #[tokio::test]
    async fn char_budget_break_records_line_stats_without_emitting() {
        // Line 1 fits; line 2 is long and overflows the char budget: it
        // must contribute to max_line_chars / truncated_long_lines but
        // not to the rendered content (whole-file renderer parity).
        let body = format!("short\n{}\nafter\n", "x".repeat(200));
        let rendered = text(
            scan_str(
                body.as_bytes(),
                None,
                None,
                ToolOutputBudget {
                    read_default_line_limit: 10,
                    read_hard_line_limit: 10,
                    read_output_char_limit: 60,
                    read_hard_output_char_limit: 60,
                    read_line_char_limit: 40,
                    model_output_inline_char_limit: 64_000,
                },
            )
            .await,
        );
        assert_eq!(rendered.returned_lines, 1);
        assert!(rendered.truncated_by_char_limit);
        assert_eq!(rendered.next_offset, Some(2));
        assert_eq!(rendered.max_line_chars, 200);
        assert_eq!(rendered.truncated_long_lines, 1);
        assert_eq!(rendered.total_lines, 3);
        assert!(!rendered.content.contains("xxx"));
    }

    #[test]
    fn substring_counter_finds_overlapping_prefix_restarts() {
        let mut counter = SubstringCounter::new(&["target/debug/.fingerprint"]);
        // "targe" + full pattern: the naive-restart failure case.
        for c in "targetarget/debug/.fingerprint".chars() {
            counter.push(c);
        }
        assert_eq!(counter.total(), 1);
    }

    #[test]
    fn substring_counter_counts_across_patterns_and_resets_on_newline() {
        let mut counter = SubstringCounter::new(&FINGERPRINT_PATTERNS);
        for c in "target/debug/.fingerprint/abc".chars() {
            counter.push(c);
        }
        // Both the debug pattern and the bare ".fingerprint/" hit.
        assert_eq!(counter.total(), 2);
        counter.reset_line();
        for c in ".fingerprint/".chars() {
            counter.push(c);
        }
        assert_eq!(counter.total(), 3);
    }

    #[test]
    fn substring_counter_does_not_match_across_lines() {
        let mut counter = SubstringCounter::new(&[".fingerprint/"]);
        for c in ".finger".chars() {
            counter.push(c);
        }
        counter.reset_line();
        for c in "print/".chars() {
            counter.push(c);
        }
        assert_eq!(counter.total(), 0);
    }
}
