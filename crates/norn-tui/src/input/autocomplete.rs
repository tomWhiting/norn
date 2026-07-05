//! Trigger detection, candidate generation, and popup state.
//!
//! Autocomplete in the TUI is built from three small, decoupled pieces:
//!
//! - [`detect_trigger`] walks back from the cursor over the input buffer
//!   to find an open `/` (slash command, column-zero only) or `@` (file
//!   path, anywhere) trigger and returns the byte offset at which the
//!   eventual replacement will start.
//! - [`filter_slash_candidates`] filters a caller-supplied snapshot of
//!   [`SlashCandidate`]s by typed prefix and returns them sorted
//!   alphabetically.
//! - [`generate_file_candidates`] walks the working tree with
//!   `ignore::WalkBuilder` (respecting `.gitignore`), then fuzzy-matches
//!   path strings with `nucleo-matcher`, returning [`FileCandidate`]s
//!   ranked best-first up to a caller-supplied cap.
//!
//! [`AutocompletePopup`] holds a candidate snapshot plus the currently
//! filtered/visible window. Selection navigation wraps, narrowing
//! re-filters from the snapshot, and acceptance returns a splice
//! description ([`Acceptance`]) that the event loop applies to the editor
//! — the popup never mutates the editor itself.
//!
//! DECSTBM is *not* issued from this module. The event loop polls the
//! popup's [`AutocompletePopup::height`] and forwards it to
//! [`crate::render::fixed_panel::FixedPanel::set_autocomplete_popup`];
//! the fixed panel's `height_dirty` flag then drives the scroll-region
//! reissue from NT-011.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use ignore::WalkBuilder;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern as NucleoPattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher as NucleoMatcher};
use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::Intensity;

use crate::render::sync_render;
use crate::render::text::truncate_to_width;
use crate::terminal::caps::TerminalCaps;

/// Maximum number of candidate rows visible at once in the popup.
const MAX_VISIBLE_ROWS: u16 = 8;

/// Whether a detected trigger is for a slash command or a file path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    /// `/` at the start of a line — slash command autocomplete.
    SlashCommand,
    /// `@` anywhere in the input — file path autocomplete.
    FilePath,
}

/// A trigger character detected behind the cursor.
///
/// `trigger_start_byte` is the byte offset *of the trigger character
/// itself* within the input string — the event loop replaces from this
/// offset (inclusive) up to the cursor with the chosen replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteTrigger {
    /// Whether the trigger is a slash command or file path.
    pub kind: TriggerKind,
    /// The characters typed *after* the trigger, in input order.
    pub prefix: String,
    /// Byte offset of the trigger character within the input string.
    pub trigger_start_byte: usize,
}

/// Origin of a registered slash command — surfaced as a tag in the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceTag {
    /// CLI built-in slash command.
    Builtin,
    /// Profile-registered slash command (including skill-backed handlers).
    Profile,
}

impl SourceTag {
    /// Lowercase string form shown next to a slash candidate.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Profile => "profile",
        }
    }
}

/// A slash command candidate as offered to the popup.
///
/// `name` is the bare command name (no leading `/`); the popup builds
/// the `/name` replacement at acceptance time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCandidate {
    /// Bare command name without the leading slash.
    pub name: String,
    /// Origin tag — built-in or profile-registered.
    pub source_tag: SourceTag,
    /// Human-readable description rendered next to the name.
    pub description: String,
}

/// A file-path candidate produced by the fuzzy walker.
///
/// `path` is the path string used both for display and as the
/// replacement value. Directories have a trailing `/` appended so the
/// user can chain further completions without typing a separator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCandidate {
    /// Display + replacement string (with trailing `/` for directories).
    pub path: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Nucleo match score; higher is better.
    pub score: u32,
}

/// A row rendered in the popup — either a slash or a file candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateRow {
    /// Slash command row.
    Slash(SlashCandidate),
    /// File path row.
    File(FileCandidate),
}

impl CandidateRow {
    /// The replacement value the editor splices in on acceptance.
    fn replacement(&self) -> String {
        match self {
            Self::Slash(c) => format!("/{}", c.name),
            Self::File(c) => c.path.clone(),
        }
    }

    /// The unstyled display string drawn into the popup row.
    fn display(&self) -> String {
        match self {
            Self::Slash(c) => format!(
                "  /{}  ({}) {}",
                c.name,
                c.source_tag.as_str(),
                c.description,
            ),
            Self::File(c) => format!("  {}", c.path),
        }
    }
}

/// Splice description returned by [`AutocompletePopup::accept`].
///
/// The event loop applies this by replacing the bytes in the editor's
/// input from `trigger_start_byte` up to the cursor with `replacement`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acceptance {
    /// Byte offset of the trigger character in the input string.
    pub trigger_start_byte: usize,
    /// Text to insert in place of the trigger + typed prefix.
    pub replacement: String,
}

/// Untyped snapshot held inside [`AutocompletePopup`] for narrowing.
#[derive(Clone, Debug)]
enum CandidateSource {
    /// Slash-command snapshot — narrowing prefix-filters this list.
    Slash(Vec<SlashCandidate>),
    /// File-path snapshot — narrowing re-runs nucleo against the held
    /// `(path, is_dir)` pairs.
    File(Vec<(String, bool)>),
}

/// Autocomplete popup rendered above the input area in the fixed panel.
///
/// Holds the untyped candidate snapshot (so narrowing can re-filter
/// without re-walking the filesystem or re-querying the registry), the
/// current visible rows, the selection index, and a scrolling window
/// offset (`visible_offset`). The popup itself never mutates the editor
/// — acceptance produces an [`Acceptance`] for the caller to apply.
#[derive(Clone, Debug)]
pub struct AutocompletePopup {
    /// Original snapshot used by [`narrow`](AutocompletePopup::narrow).
    snapshot: CandidateSource,
    /// Currently visible candidate list after filtering/narrowing.
    pub candidates: Vec<CandidateRow>,
    /// Index of the selected row within `candidates`.
    pub selected_index: usize,
    /// First candidate index visible in the popup window.
    pub visible_offset: usize,
    /// Byte offset of the trigger character — replayed into [`Acceptance`].
    trigger_start_byte: usize,
}

impl AutocompletePopup {
    /// Build a slash-command popup from a registry snapshot.
    ///
    /// `snapshot` must contain the full set of registered slash commands
    /// (built-ins and profile-registered); the popup keeps it so that
    /// subsequent [`narrow`](AutocompletePopup::narrow) calls can
    /// re-filter without re-consulting the registry. `prefix` is the
    /// characters already typed after the leading `/`.
    #[must_use]
    pub fn new_slash(
        snapshot: Vec<SlashCandidate>,
        prefix: &str,
        trigger_start_byte: usize,
    ) -> Self {
        let filtered = filter_slash_candidates(&snapshot, prefix);
        let candidates = filtered.into_iter().map(CandidateRow::Slash).collect();
        Self {
            snapshot: CandidateSource::Slash(snapshot),
            candidates,
            selected_index: 0,
            visible_offset: 0,
            trigger_start_byte,
        }
    }

    /// Build a file-path popup from a pre-walked path snapshot.
    ///
    /// `paths` must contain `(path, is_dir)` pairs already gathered from
    /// the working tree — typically by calling
    /// [`generate_file_candidates`] and discarding the score. The popup
    /// applies its own nucleo fuzzy match against `prefix` and retains
    /// the full list for narrowing.
    #[must_use]
    pub fn new_file(paths: Vec<(String, bool)>, prefix: &str, trigger_start_byte: usize) -> Self {
        let candidates = fuzzy_file_rows(&paths, prefix);
        Self {
            snapshot: CandidateSource::File(paths),
            candidates,
            selected_index: 0,
            visible_offset: 0,
            trigger_start_byte,
        }
    }

    /// Byte offset of the trigger character — replayed on acceptance and
    /// also used by the event loop to detect when a new trigger replaces
    /// the popup's snapshot (different kind or different start byte).
    #[must_use]
    pub fn trigger_start_byte(&self) -> usize {
        self.trigger_start_byte
    }

    /// Whether the popup's snapshot matches the supplied trigger.
    ///
    /// Used by the event loop to decide between narrowing the existing
    /// popup (kind and `trigger_start_byte` both match — only the typed
    /// prefix grew) and rebuilding from a fresh snapshot. A `false`
    /// result implies a different trigger character, a different
    /// `trigger_start_byte`, or a kind change — any of which requires a
    /// new candidate source.
    #[must_use]
    pub fn matches_trigger(&self, trigger: &AutocompleteTrigger) -> bool {
        if self.trigger_start_byte != trigger.trigger_start_byte {
            return false;
        }
        match (&self.snapshot, trigger.kind) {
            (CandidateSource::Slash(_), TriggerKind::SlashCommand)
            | (CandidateSource::File(_), TriggerKind::FilePath) => true,
            (CandidateSource::Slash(_), TriggerKind::FilePath)
            | (CandidateSource::File(_), TriggerKind::SlashCommand) => false,
        }
    }

    /// Number of rows the popup contributes to the fixed panel height.
    ///
    /// Up to [`MAX_VISIBLE_ROWS`] candidate rows, plus one overflow row
    /// when there are more candidates than fit on screen. Returns zero
    /// when the candidate list is empty.
    #[must_use]
    pub fn height(&self) -> u16 {
        let visible = u16::try_from(self.candidates.len())
            .unwrap_or(MAX_VISIBLE_ROWS)
            .min(MAX_VISIBLE_ROWS);
        let overflow = u16::from(self.candidates.len() > usize::from(MAX_VISIBLE_ROWS));
        visible.saturating_add(overflow)
    }

    /// Whether the popup currently has any candidates to display.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// Move the selection up by one row, wrapping at the top to the
    /// bottom of the list. Adjusts the visible window so the selection
    /// stays in view.
    pub fn select_up(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            self.candidates.len() - 1
        } else {
            self.selected_index - 1
        };
        self.adjust_window();
    }

    /// Move the selection down by one row, wrapping at the bottom to
    /// the top of the list. Adjusts the visible window so the selection
    /// stays in view.
    pub fn select_down(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected_index = if self.selected_index + 1 >= self.candidates.len() {
            0
        } else {
            self.selected_index + 1
        };
        self.adjust_window();
    }

    /// Return the splice description for the currently selected
    /// candidate, or `None` when the popup has no candidates.
    #[must_use]
    pub fn accept(&self) -> Option<Acceptance> {
        let candidate = self.candidates.get(self.selected_index)?;
        Some(Acceptance {
            trigger_start_byte: self.trigger_start_byte,
            replacement: candidate.replacement(),
        })
    }

    /// Re-filter the popup against a new typed prefix.
    ///
    /// Slash snapshots are prefix-filtered; file snapshots are re-fuzzy
    /// matched. The selection clamps to the new list and the visible
    /// window is rebased. Returns `true` when the popup still has
    /// candidates (i.e. should stay open), `false` when narrowing has
    /// emptied the list and the caller should dismiss the popup.
    pub fn narrow(&mut self, new_prefix: &str) -> bool {
        self.candidates = match &self.snapshot {
            CandidateSource::Slash(snapshot) => filter_slash_candidates(snapshot, new_prefix)
                .into_iter()
                .map(CandidateRow::Slash)
                .collect(),
            CandidateSource::File(paths) => fuzzy_file_rows(paths, new_prefix),
        };
        if self.candidates.is_empty() {
            self.selected_index = 0;
            self.visible_offset = 0;
            return false;
        }
        if self.selected_index >= self.candidates.len() {
            self.selected_index = self.candidates.len() - 1;
        }
        self.adjust_window();
        true
    }

    /// Render the popup at `top_row`, with rows truncated to `width`
    /// display columns.
    ///
    /// Each row is preceded by a cursor-position escape and a line
    /// erase so stale content is wiped. The selected row is bracketed
    /// by `SGR 7` (reverse video) / `SGR 0` (reset). When more
    /// candidates exist than fit, a dimmed `N more...` row is appended
    /// below the visible window. The whole redraw is wrapped in
    /// [`sync_render`] so it presents atomically.
    pub fn render<W: io::Write>(
        &self,
        top_row: u16,
        width: u16,
        writer: &mut W,
        caps: &TerminalCaps,
    ) -> io::Result<()> {
        sync_render(caps, writer, |w| {
            let visible_count = self.visible_count();
            for offset in 0..visible_count {
                let candidate_idx = self.visible_offset + offset;
                let row = top_row.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
                let display = self
                    .candidates
                    .get(candidate_idx)
                    .map(CandidateRow::display)
                    .unwrap_or_default();
                let line = truncate_to_width(&display, width);
                let is_selected = candidate_idx == self.selected_index;
                if is_selected {
                    write!(
                        w,
                        "{}{}{}{line}{}",
                        Csi::Cursor(Cursor::Position {
                            line: OneBased::from_zero_based(row),
                            col: OneBased::from_zero_based(0),
                        }),
                        Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine)),
                        Csi::Sgr(Sgr::Reverse(true)),
                        Csi::Sgr(Sgr::Reset),
                    )?;
                } else {
                    write!(
                        w,
                        "{}{}{line}",
                        Csi::Cursor(Cursor::Position {
                            line: OneBased::from_zero_based(row),
                            col: OneBased::from_zero_based(0),
                        }),
                        Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine)),
                    )?;
                }
            }

            if self.candidates.len() > usize::from(MAX_VISIBLE_ROWS) {
                let hidden = self.candidates.len() - usize::from(MAX_VISIBLE_ROWS);
                let row = top_row.saturating_add(MAX_VISIBLE_ROWS);
                let text = format!("  {hidden} more...");
                let line = truncate_to_width(&text, width);
                write!(
                    w,
                    "{}{}{}{line}{}",
                    Csi::Cursor(Cursor::Position {
                        line: OneBased::from_zero_based(row),
                        col: OneBased::from_zero_based(0),
                    }),
                    Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine)),
                    Csi::Sgr(Sgr::Intensity(Intensity::Dim)),
                    Csi::Sgr(Sgr::Reset),
                )?;
            }

            Ok(())
        })
    }

    /// Number of candidate rows currently visible (excluding the
    /// overflow indicator).
    fn visible_count(&self) -> usize {
        self.candidates.len().min(usize::from(MAX_VISIBLE_ROWS))
    }

    /// Shift `visible_offset` so `selected_index` is inside the window.
    fn adjust_window(&mut self) {
        let cap = usize::from(MAX_VISIBLE_ROWS);
        if self.candidates.len() <= cap {
            self.visible_offset = 0;
            return;
        }
        if self.selected_index < self.visible_offset {
            self.visible_offset = self.selected_index;
        } else if self.selected_index >= self.visible_offset + cap {
            self.visible_offset = self.selected_index + 1 - cap;
        }
        let max_offset = self.candidates.len() - cap;
        if self.visible_offset > max_offset {
            self.visible_offset = max_offset;
        }
    }
}

/// Walk back from the cursor to find an open trigger character.
///
/// `cursor_col` is a *character* offset within the cursor's line (the
/// invariant maintained by [`crate::input::editor::InputEditor`]). The
/// returned `trigger_start_byte` is a *byte* offset within `input` —
/// the event loop splices its replacement at this byte position. The
/// scan stops at the first whitespace character before the cursor: a
/// trigger that has been "closed" by a word boundary is not a live
/// completion target.
///
/// `/` triggers slash-command completion only when the slash sits at
/// the start of its line (either the first character of the input or
/// immediately after a newline). `@` triggers file-path completion
/// anywhere in the input.
#[must_use]
pub fn detect_trigger(input: &str, cursor_col: usize) -> Option<AutocompleteTrigger> {
    let chars: Vec<char> = input.chars().collect();
    let scan_from = cursor_col.min(chars.len());

    let mut i = scan_from;
    while i > 0 {
        let ch = chars[i - 1];
        if ch == '/' {
            let at_line_start = i == 1 || chars[i - 2] == '\n';
            if at_line_start {
                let prefix: String = chars[i..scan_from].iter().collect();
                let trigger_start_byte = byte_offset_of_char(&chars, i - 1);
                return Some(AutocompleteTrigger {
                    kind: TriggerKind::SlashCommand,
                    prefix,
                    trigger_start_byte,
                });
            }
            return None;
        }
        if ch == '@' {
            let prefix: String = chars[i..scan_from].iter().collect();
            let trigger_start_byte = byte_offset_of_char(&chars, i - 1);
            return Some(AutocompleteTrigger {
                kind: TriggerKind::FilePath,
                prefix,
                trigger_start_byte,
            });
        }
        if ch.is_whitespace() {
            return None;
        }
        i -= 1;
    }
    None
}

/// Sum the byte widths of the first `n` characters of `chars`.
fn byte_offset_of_char(chars: &[char], n: usize) -> usize {
    chars.iter().take(n).map(|ch| ch.len_utf8()).sum()
}

/// Filter `snapshot` to entries whose `name` starts with `prefix`, then
/// sort the result alphabetically by name.
///
/// Mirrors the prefix-match + alphabetical-sort behaviour of the
/// reedline-era `NornCompleter::slash_suggestions` so the two
/// completion surfaces stay consistent during the REPL → TUI cut-over.
#[must_use]
pub fn filter_slash_candidates(snapshot: &[SlashCandidate], prefix: &str) -> Vec<SlashCandidate> {
    let mut out: Vec<SlashCandidate> = snapshot
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .cloned()
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Walk `root` and fuzzy-match its file paths against `prefix`.
///
/// The walker honours `.gitignore`, the global git ignore file, the
/// repository's `.git/info/exclude`, and hidden-file filtering — the
/// canonical settings used elsewhere in the workspace (see
/// `libcorpus::walker::file_walker`). Symlinks are not followed.
/// Per-entry errors are logged at `tracing::debug!` and skipped so a
/// permission-denied subdirectory cannot abort the whole walk.
///
/// Path strings are computed relative to `root` and joined with the
/// platform's main separator. Directory entries have a trailing
/// separator appended. Scores come from `nucleo-matcher` with
/// case-insensitive matching and smart normalisation; results are
/// already sorted best-first by `match_list`. The output is capped at
/// `max_results` — pass `usize::MAX` to disable the cap.
#[must_use]
pub fn generate_file_candidates(
    root: &Path,
    prefix: &str,
    max_results: usize,
) -> Vec<FileCandidate> {
    let entries = walk_entries(root);
    if entries.is_empty() {
        return Vec::new();
    }

    let path_strings: Vec<String> = entries.iter().map(|(path, _)| path.clone()).collect();
    let dir_map: HashMap<String, bool> = entries.into_iter().collect();

    let mut matcher = NucleoMatcher::new(NucleoConfig::DEFAULT.match_paths());
    let pat = NucleoPattern::parse(prefix, CaseMatching::Ignore, Normalization::Smart);
    let scored: Vec<(String, u32)> = pat.match_list(path_strings, &mut matcher);

    let mut out: Vec<FileCandidate> = scored
        .into_iter()
        .filter_map(|(path, score)| {
            let is_dir = *dir_map.get(&path)?;
            Some(FileCandidate {
                path,
                is_dir,
                score,
            })
        })
        .collect();

    if out.len() > max_results {
        out.truncate(max_results);
    }
    out
}

/// Walk `root` with the canonical `.gitignore`-aware settings, returning
/// `(display_path, is_dir)` pairs. The root entry itself is skipped.
///
/// Dotfiles are included — `hidden(false)` — so the `@` popup surfaces
/// `.norn/`, `.gitignore`, and other dot-prefixed paths the user may
/// want to attach. `.gitignore` (and the other ignore files) still
/// filter the walk so build artefacts and explicitly-ignored paths do
/// not appear. `.git` itself is pruned explicitly: with the hidden
/// filter off nothing else excludes it, and VCS internals are never
/// attachment candidates.
pub fn walk_entries(root: &Path) -> Vec<(String, bool)> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .filter_entry(|entry| {
            entry.depth() == 0 || entry.file_name() != std::ffi::OsStr::new(".git")
        })
        .build();

    let mut entries: Vec<(String, bool)> = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(error = %err, "skipping directory entry during autocomplete walk");
                continue;
            }
        };
        if entry.path() == root {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let mut display = relative.to_string_lossy().into_owned();
        if display.is_empty() {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if is_dir {
            display.push(std::path::MAIN_SEPARATOR);
        }
        entries.push((display, is_dir));
    }
    entries
}

/// Fuzzy-match `paths` against `prefix`, returning popup-ready rows.
///
/// Used by both [`AutocompletePopup::new_file`] (initial filtering) and
/// [`AutocompletePopup::narrow`] (re-filtering on additional typed
/// characters). The `(path, is_dir)` shape mirrors the snapshot held
/// inside the popup so the same nucleo call serves both code paths.
fn fuzzy_file_rows(paths: &[(String, bool)], prefix: &str) -> Vec<CandidateRow> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut matcher = NucleoMatcher::new(NucleoConfig::DEFAULT.match_paths());
    let pat = NucleoPattern::parse(prefix, CaseMatching::Ignore, Normalization::Smart);
    let path_strings: Vec<String> = paths.iter().map(|(p, _)| p.clone()).collect();
    let dir_map: HashMap<&str, bool> = paths.iter().map(|(p, d)| (p.as_str(), *d)).collect();
    let scored: Vec<(String, u32)> = pat.match_list(path_strings, &mut matcher);
    scored
        .into_iter()
        .filter_map(|(path, score)| {
            let is_dir = *dir_map.get(path.as_str())?;
            Some(CandidateRow::File(FileCandidate {
                path,
                is_dir,
                score,
            }))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ---------- detect_trigger ----------

    #[test]
    fn slash_at_column_zero_triggers_slash_command() {
        let trigger = detect_trigger("/", 1).unwrap();
        assert_eq!(trigger.kind, TriggerKind::SlashCommand);
        assert_eq!(trigger.prefix, "");
        assert_eq!(trigger.trigger_start_byte, 0);
    }

    #[test]
    fn slash_mid_word_does_not_trigger() {
        assert_eq!(detect_trigger("text /", 6), None);
    }

    #[test]
    fn at_symbol_anywhere_triggers_file_path() {
        let trigger = detect_trigger("@src", 4).unwrap();
        assert_eq!(trigger.kind, TriggerKind::FilePath);
        assert_eq!(trigger.prefix, "src");
        assert_eq!(trigger.trigger_start_byte, 0);
    }

    #[test]
    fn at_symbol_with_prefix_captures_typed_chars() {
        let trigger = detect_trigger("hello @sr", 9).unwrap();
        assert_eq!(trigger.kind, TriggerKind::FilePath);
        assert_eq!(trigger.prefix, "sr");
        assert_eq!(trigger.trigger_start_byte, 6);
    }

    #[test]
    fn slash_after_newline_is_treated_as_column_zero() {
        let trigger = detect_trigger("hi\n/he", 6).unwrap();
        assert_eq!(trigger.kind, TriggerKind::SlashCommand);
        assert_eq!(trigger.prefix, "he");
        assert_eq!(trigger.trigger_start_byte, 3);
    }

    #[test]
    fn whitespace_after_at_aborts_detection() {
        assert_eq!(detect_trigger("@src file", 9), None);
    }

    #[test]
    fn empty_input_yields_no_trigger() {
        assert_eq!(detect_trigger("", 0), None);
    }

    #[test]
    fn multibyte_prefix_byte_offset_is_correct() {
        // 'é' is two bytes; `@é` should report trigger_start_byte = 0
        // and 'éx' (cursor at char 3) prefix should be "éx".
        let trigger = detect_trigger("@éx", 3).unwrap();
        assert_eq!(trigger.kind, TriggerKind::FilePath);
        assert_eq!(trigger.prefix, "éx");
        assert_eq!(trigger.trigger_start_byte, 0);
    }

    // ---------- filter_slash_candidates ----------

    fn sample_slash_snapshot() -> Vec<SlashCandidate> {
        vec![
            SlashCandidate {
                name: "help".to_owned(),
                source_tag: SourceTag::Builtin,
                description: "Show help".to_owned(),
            },
            SlashCandidate {
                name: "tools".to_owned(),
                source_tag: SourceTag::Builtin,
                description: "List tools".to_owned(),
            },
            SlashCandidate {
                name: "compact".to_owned(),
                source_tag: SourceTag::Builtin,
                description: "Compact context".to_owned(),
            },
            SlashCandidate {
                name: "review".to_owned(),
                source_tag: SourceTag::Profile,
                description: "Custom review skill".to_owned(),
            },
        ]
    }

    #[test]
    fn filter_slash_candidates_keeps_prefix_matches_and_sorts() {
        let filtered = filter_slash_candidates(&sample_slash_snapshot(), "he");
        let names: Vec<_> = filtered.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["help"]);
        assert!(!names.contains(&"tools"));
    }

    #[test]
    fn filter_slash_candidates_returns_full_list_for_empty_prefix_sorted() {
        let filtered = filter_slash_candidates(&sample_slash_snapshot(), "");
        let names: Vec<_> = filtered.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["compact", "help", "review", "tools"]);
    }

    // ---------- generate_file_candidates ----------

    #[test]
    fn generate_file_candidates_matches_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), "x").unwrap();
        let candidates = generate_file_candidates(tmp.path(), "main", 50);
        let hit = candidates
            .iter()
            .find(|c| c.path.ends_with("main.rs"))
            .expect("main.rs must be matched");
        assert!(hit.score > 0, "score must be positive");
        assert!(!hit.is_dir);
    }

    #[test]
    fn generate_file_candidates_flags_directories_with_trailing_separator() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("nested")).unwrap();
        std::fs::write(tmp.path().join("nested/leaf.txt"), "x").unwrap();
        let candidates = generate_file_candidates(tmp.path(), "nested", 50);
        let dir_hit = candidates
            .iter()
            .find(|c| c.is_dir)
            .expect("directory candidate must be present");
        assert!(
            dir_hit.path.ends_with(std::path::MAIN_SEPARATOR),
            "directory display must end with separator: {:?}",
            dir_hit.path,
        );
    }

    // ---------- popup render ----------

    fn slash_popup_with(n: usize) -> AutocompletePopup {
        let snapshot: Vec<SlashCandidate> = (0..n)
            .map(|i| SlashCandidate {
                name: format!("cmd{i:02}"),
                source_tag: SourceTag::Builtin,
                description: format!("Description for cmd{i:02}"),
            })
            .collect();
        AutocompletePopup::new_slash(snapshot, "", 0)
    }

    #[test]
    fn popup_renders_one_row_per_visible_candidate() {
        let popup = slash_popup_with(5);
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        popup.render(10, 80, &mut buf, &caps).unwrap();
        let out = String::from_utf8(buf).unwrap();
        for row_one_based in 11..=15u16 {
            assert!(
                out.contains(&format!("\x1b[{row_one_based};1H")),
                "row {row_one_based} must be addressed",
            );
        }
        assert!(
            !out.contains("\x1b[16;1H"),
            "no row beyond the candidate count should be addressed",
        );
    }

    #[test]
    fn popup_render_highlights_the_selected_row() {
        let popup = slash_popup_with(3);
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        popup.render(10, 80, &mut buf, &caps).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // SGR 7 is the reverse-video opener; assert it appears exactly once.
        let occurrences = out.matches("\x1b[7m").count();
        assert_eq!(
            occurrences, 1,
            "selected row must be highlighted exactly once",
        );
    }

    #[test]
    fn popup_height_matches_candidate_count_below_eight() {
        let popup = slash_popup_with(3);
        assert_eq!(popup.height(), 3);
    }

    #[test]
    fn popup_height_caps_at_eight_when_overflowing_with_indicator() {
        let popup = slash_popup_with(12);
        // 8 visible rows + 1 overflow indicator = 9.
        assert_eq!(popup.height(), 9);
    }

    #[test]
    fn popup_renders_overflow_indicator_when_candidates_exceed_window() {
        let popup = slash_popup_with(12);
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        popup.render(10, 80, &mut buf, &caps).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("4 more"),
            "overflow indicator missing: {out:?}"
        );
    }

    // ---------- popup navigation, narrow, accept ----------

    #[test]
    fn select_down_wraps_at_end() {
        let mut popup = slash_popup_with(3);
        popup.select_down();
        popup.select_down();
        popup.select_down();
        assert_eq!(popup.selected_index, 0, "select_down past end wraps to 0");
    }

    #[test]
    fn select_up_wraps_at_start() {
        let mut popup = slash_popup_with(3);
        popup.select_up();
        assert_eq!(popup.selected_index, 2, "select_up at start wraps to end");
    }

    #[test]
    fn accept_returns_slashed_replacement_for_selected_candidate() {
        let popup = slash_popup_with(3);
        let acceptance = popup.accept().unwrap();
        assert_eq!(acceptance.replacement, "/cmd00");
        assert_eq!(acceptance.trigger_start_byte, 0);
    }

    #[test]
    fn accept_returns_none_when_popup_has_no_candidates() {
        let popup = AutocompletePopup::new_slash(Vec::new(), "", 0);
        assert!(popup.accept().is_none());
    }

    #[test]
    fn narrow_filters_and_keeps_popup_open() {
        let snapshot = sample_slash_snapshot();
        let mut popup = AutocompletePopup::new_slash(snapshot, "", 0);
        assert!(popup.narrow("he"));
        let names: Vec<&str> = popup
            .candidates
            .iter()
            .filter_map(|row| match row {
                CandidateRow::Slash(c) => Some(c.name.as_str()),
                CandidateRow::File(_) => None,
            })
            .collect();
        assert_eq!(names, vec!["help"]);
    }

    #[test]
    fn narrow_to_no_matches_closes_popup() {
        let snapshot = sample_slash_snapshot();
        let mut popup = AutocompletePopup::new_slash(snapshot, "", 0);
        assert!(
            !popup.narrow("zzz"),
            "narrowing to no candidates must signal closed",
        );
        assert!(popup.candidates.is_empty());
        assert!(!popup.is_open());
    }

    #[test]
    fn accept_after_navigation_returns_navigated_candidate() {
        let mut popup = slash_popup_with(3);
        popup.select_down();
        let acceptance = popup.accept().unwrap();
        assert_eq!(acceptance.replacement, "/cmd01");
    }

    #[test]
    fn navigation_scrolls_visible_window_past_eight() {
        let mut popup = slash_popup_with(12);
        for _ in 0..8 {
            popup.select_down();
        }
        assert_eq!(popup.selected_index, 8);
        assert_eq!(popup.visible_offset, 1, "window must scroll past row 8");
    }

    #[test]
    fn file_popup_accept_uses_path_verbatim() {
        let paths = vec![
            ("src/main.rs".to_owned(), false),
            ("src/lib.rs".to_owned(), false),
        ];
        let popup = AutocompletePopup::new_file(paths, "main", 0);
        let acceptance = popup.accept().unwrap();
        assert!(
            acceptance.replacement.ends_with("main.rs"),
            "file replacement must be the matched path: {:?}",
            acceptance.replacement,
        );
    }
}
