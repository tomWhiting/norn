//! pulldown-cmark event emitter for the streaming markdown renderer.
//!
//! [`Emitter`] consumes the parser's event stream for a single segment
//! and produces a styled byte string plus a `paragraph_pending` flag
//! that the parent renderer threads across segments to handle paragraph
//! breaks at segment boundaries.

use std::fmt::Write as _;

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, CellAlignment, ContentArrangement, Table};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use termina::escape::csi::{Csi, Sgr};
use termina::style::{ColorSpec, Intensity};

use crate::render::style::{colour_for, hyperlink, italic};
use crate::render::syntax::SyntaxHighlighter;
use crate::terminal::caps::TerminalCaps;

use super::{BLOCKQUOTE_PREFIX, INLINE_CODE_COLOUR, italic_off};

/// Per-segment event handler — owns the running ANSI output buffer and
/// the inline/list/link state that spans multiple events.
pub(super) struct Emitter<'a> {
    caps: &'a TerminalCaps,
    width: u16,
    highlighter: &'a SyntaxHighlighter,
    output: String,
    in_code_block: bool,
    code_block_lang: Option<String>,
    code_block_buffer: String,
    list_stack: Vec<Option<u64>>,
    in_link: bool,
    link_text: String,
    link_url: String,
    paragraph_pending: bool,
    current_heading: Option<HeadingLevel>,
    /// Byte offset in `output` where [`Emitter::start_item`] wrote the
    /// current item's bullet or number prefix. When
    /// [`pulldown_cmark::Event::TaskListMarker`] arrives next, R6 uses
    /// this to truncate the bullet and replace it with a checkbox
    /// glyph. Cleared on `TagEnd::Item` so a non-task item never picks
    /// up a stale position.
    item_bullet_start: Option<usize>,
    /// Whether the parser is currently inside an `![alt](url)` image
    /// span. Text events between `Tag::Image` and `TagEnd::Image`
    /// accumulate into [`Emitter::image_alt`] rather than the live
    /// output buffer so R9 can emit a single dim `[image: alt]`
    /// placeholder on close.
    in_image: bool,
    /// Accumulated alt-text for the active image span. Drained and
    /// cleared on `TagEnd::Image`.
    image_alt: String,
    /// Current blockquote nesting depth. One unit of
    /// [`BLOCKQUOTE_PREFIX`] is emitted per level when a quote opens,
    /// on line breaks inside the quote, and inside the paragraph break
    /// emitted by [`Emitter::resolve_pending_paragraph`]. Decremented
    /// on `TagEnd::BlockQuote` so subsequent content drops back to its
    /// outer-context formatting.
    blockquote_depth: usize,
    /// Whether a `**bold**` span is currently open. Tracked because
    /// [`BLOCKQUOTE_PREFIX`] ends in SGR 22 (normal intensity), which
    /// cancels bold along with the prefix's own dim — bold and dim
    /// share the single SGR intensity slot. When a bold span crosses a
    /// line break inside a quote, [`Emitter::emit_blockquote_prefix`]
    /// re-emits SGR 1 after the prefix so the bold survives. Italic
    /// (SGR 3/23) and strikethrough (SGR 9/29) live in independent
    /// slots and are not disturbed by the prefix, so only bold needs
    /// this.
    bold_active: bool,
    /// Column alignments captured from `Tag::Table(alignments)` —
    /// applied to the comfy-table render in [`Emitter::end_table`].
    /// Cleared at table close.
    table_alignments: Vec<Alignment>,
    /// Header row cells captured between `Tag::TableHead` and its end.
    /// Drained into the comfy-table header (bold) at table close.
    table_header: Vec<String>,
    /// Body rows. Each `TagEnd::TableRow` pushes the live
    /// [`Emitter::current_row`] into this stack; `TagEnd::Table`
    /// drains the stack into the rendered comfy-table.
    table_rows: Vec<Vec<String>>,
    /// Cells accumulated for the currently-open row. Drained into
    /// either [`Emitter::table_header`] (on `TagEnd::TableHead`) or
    /// [`Emitter::table_rows`] (on `TagEnd::TableRow`).
    current_row: Vec<String>,
    /// Output buffer parked while a table cell is collecting its
    /// content. `Tag::TableCell` swaps the live output buffer out so
    /// every nested write (text, SGR, hyperlink markers) lands in a
    /// fresh string; `TagEnd::TableCell` swaps the parked buffer back
    /// in and pushes the cell content into
    /// [`Emitter::current_row`]. Wrapped in `Option` so the resting
    /// state never allocates an unused buffer.
    parked_output: Option<String>,
}

impl<'a> Emitter<'a> {
    pub(super) fn new(
        caps: &'a TerminalCaps,
        width: u16,
        highlighter: &'a SyntaxHighlighter,
    ) -> Self {
        Self {
            caps,
            width,
            highlighter,
            output: String::new(),
            in_code_block: false,
            code_block_lang: None,
            code_block_buffer: String::new(),
            list_stack: Vec::new(),
            in_link: false,
            link_text: String::new(),
            link_url: String::new(),
            paragraph_pending: false,
            current_heading: None,
            item_bullet_start: None,
            in_image: false,
            image_alt: String::new(),
            blockquote_depth: 0,
            table_alignments: Vec::new(),
            table_header: Vec::new(),
            table_rows: Vec::new(),
            current_row: Vec::new(),
            parked_output: None,
            bold_active: false,
        }
    }

    pub(super) fn into_output(self) -> (String, bool) {
        (self.output, self.paragraph_pending)
    }

    fn resolve_pending_paragraph(&mut self) {
        if self.paragraph_pending {
            if self.blockquote_depth > 0 {
                // Multi-paragraph quotes keep the prefix on the blank
                // separator line AND on the next content line so the
                // rendered shape mirrors the source's `> p1`/`>`/`> p2`
                // layout instead of breaking the column.
                self.output.push('\n');
                self.emit_blockquote_prefix();
                self.output.push('\n');
                self.emit_blockquote_prefix();
            } else {
                self.output.push_str("\n\n");
            }
            self.paragraph_pending = false;
        }
    }

    /// Append one [`BLOCKQUOTE_PREFIX`] per active blockquote level.
    /// No-op outside a quote. Called after every line break that should
    /// continue inside the quote — `Tag::BlockQuote` open, soft/hard
    /// breaks, and the paragraph-pending resolve.
    ///
    /// The prefix ends in SGR 22 (normal intensity), so when a bold
    /// span is open across the break the prefix would silently drop the
    /// bold (bold and dim share the intensity slot). Re-emit SGR 1 after
    /// the prefix so `**bold text spanning a line**` inside a quote
    /// stays bold on every line.
    fn emit_blockquote_prefix(&mut self) {
        if self.blockquote_depth == 0 {
            return;
        }
        for _ in 0..self.blockquote_depth {
            self.output.push_str(BLOCKQUOTE_PREFIX);
        }
        if self.bold_active {
            let _ = write!(self.output, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
        }
    }

    pub(super) fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(end) => self.handle_end(end),
            Event::Text(text) => self.handle_text(&text),
            Event::Code(text) => self.handle_inline_code(&text),
            Event::InlineMath(text) => self.handle_inline_math(&text),
            Event::DisplayMath(text) => self.handle_display_math(&text),
            Event::Html(text) => {
                self.resolve_pending_paragraph();
                self.handle_html(&text);
            }
            Event::InlineHtml(text) => self.handle_html(&text),
            Event::TaskListMarker(checked) => self.handle_task_list_marker(checked),
            Event::SoftBreak | Event::HardBreak => {
                self.output.push('\n');
                self.emit_blockquote_prefix();
            }
            Event::Rule => {
                self.resolve_pending_paragraph();
                self.handle_rule();
            }
            Event::FootnoteReference(_) => {}
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        self.resolve_pending_paragraph();
        match tag {
            Tag::Heading { level, .. } => self.start_heading(level),
            Tag::Strong => {
                self.bold_active = true;
                let _ = write!(self.output, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
            }
            Tag::Emphasis => {
                let _ = write!(self.output, "{}", Csi::Sgr(italic(self.caps)));
            }
            Tag::Strikethrough => {
                self.output.push_str("\x1b[9m");
            }
            Tag::CodeBlock(kind) => {
                self.in_code_block = true;
                self.code_block_buffer.clear();
                self.code_block_lang = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let s = lang.into_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => self.start_item(),
            Tag::Link { dest_url, .. } => {
                self.in_link = true;
                self.link_text.clear();
                self.link_url = dest_url.into_string();
            }
            Tag::Image { .. } => {
                self.in_image = true;
                self.image_alt.clear();
            }
            Tag::BlockQuote(_) => self.start_blockquote(),
            Tag::Table(alignments) => self.start_table(alignments),
            Tag::TableHead | Tag::TableRow => self.current_row.clear(),
            Tag::TableCell => {
                self.parked_output = Some(std::mem::take(&mut self.output));
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph if self.list_stack.is_empty() => {
                self.paragraph_pending = true;
            }
            TagEnd::Heading(_) => {
                let level = self.current_heading.take();
                let _ = write!(
                    self.output,
                    "{}",
                    Csi::Sgr(Sgr::Intensity(Intensity::Normal)),
                );
                self.output.push('\n');
                if matches!(level, Some(HeadingLevel::H1)) {
                    let rule_width = usize::from(self.width.min(40));
                    let _ = write!(self.output, "\x1b[2m");
                    for _ in 0..rule_width {
                        self.output.push('─');
                    }
                    let _ = writeln!(self.output, "\x1b[22m");
                }
            }
            TagEnd::Strong => {
                self.bold_active = false;
                let _ = write!(
                    self.output,
                    "{}",
                    Csi::Sgr(Sgr::Intensity(Intensity::Normal)),
                );
            }
            TagEnd::Emphasis => {
                let _ = write!(self.output, "{}", Csi::Sgr(italic_off(self.caps)));
            }
            TagEnd::Strikethrough => {
                self.output.push_str("\x1b[29m");
            }
            TagEnd::CodeBlock => self.end_code_block(),
            TagEnd::List(_) => {
                self.list_stack.pop();
            }
            TagEnd::Item => {
                self.output.push('\n');
                self.item_bullet_start = None;
            }
            TagEnd::Link => self.end_link(),
            TagEnd::Image => self.end_image(),
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
            }
            TagEnd::TableCell => {
                // `parked_output` is `Some` for the whole span between
                // `Tag::TableCell` and this end — pulldown-cmark emits
                // balanced Start/End events and R2's table-boundary
                // buffering delivers the complete table to a single
                // segment (one `Emitter`), so the park always precedes
                // this. The `if let` trusts that invariant: on the
                // (unreachable) `None` path it is a no-op that leaves
                // `self.output` intact rather than swapping the live
                // output buffer into a phantom cell and losing it.
                if let Some(parked) = self.parked_output.take() {
                    let cell = std::mem::replace(&mut self.output, parked);
                    self.current_row.push(cell);
                }
            }
            TagEnd::TableHead => {
                self.table_header = std::mem::take(&mut self.current_row);
            }
            TagEnd::TableRow => {
                let row = std::mem::take(&mut self.current_row);
                self.table_rows.push(row);
            }
            TagEnd::Table => self.end_table(),
            _ => {}
        }
    }

    fn handle_text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_block_buffer.push_str(text);
        } else if self.in_image {
            self.image_alt.push_str(text);
        } else if self.in_link {
            self.link_text.push_str(text);
        } else {
            self.output.push_str(text);
        }
    }

    fn handle_inline_code(&mut self, text: &str) {
        if self.in_image {
            self.image_alt.push_str(text);
            return;
        }
        if self.in_link {
            self.link_text.push_str(text);
            return;
        }
        self.output
            .push_str(&colour_for(INLINE_CODE_COLOUR, self.caps));
        self.output.push_str(text);
        let _ = write!(
            self.output,
            "{}",
            Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)),
        );
    }

    /// Render an HTML span — block-level or inline — as dim text so the
    /// raw markup remains visible but visually secondary to markdown
    /// content. Inside a link's display text, the raw HTML is appended
    /// verbatim (mirroring [`Emitter::handle_inline_code`]) so the
    /// terminal-side hyperlink formatter receives a clean string.
    fn handle_html(&mut self, text: &str) {
        if self.in_image {
            self.image_alt.push_str(text);
            return;
        }
        if self.in_link {
            self.link_text.push_str(text);
            return;
        }
        let _ = write!(self.output, "\x1b[2m{text}\x1b[22m");
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.current_heading = Some(level);
        if !self.output.ends_with('\n') && !self.output.is_empty() {
            self.output.push('\n');
        }
        match level {
            HeadingLevel::H4 | HeadingLevel::H5 | HeadingLevel::H6 => {
                let _ = write!(self.output, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Dim)));
            }
            _ => {
                let _ = write!(self.output, "{}", Csi::Sgr(Sgr::Intensity(Intensity::Bold)));
            }
        }
    }

    /// Open a blockquote level. `resolve_pending_paragraph` ran in
    /// [`Emitter::handle_start`] already, so the only remaining work is
    /// to enter a fresh line when this is the first level and the
    /// output currently sits mid-line, increment the depth, and write
    /// one unit of prefix for the level we just opened. Outer levels
    /// already painted their own prefixes when their start fired.
    fn start_blockquote(&mut self) {
        if self.blockquote_depth == 0 && !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.blockquote_depth += 1;
        self.output.push_str(BLOCKQUOTE_PREFIX);
    }

    fn start_item(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        let depth = self.list_stack.len().saturating_sub(1);
        for _ in 0..depth {
            self.output.push_str("  ");
        }
        // Record where the bullet/number starts so R6's
        // TaskListMarker arm can truncate back and swap in a checkbox
        // glyph when this item turns out to be a task-list item.
        self.item_bullet_start = Some(self.output.len());
        if let Some(last) = self.list_stack.last_mut() {
            if let Some(n) = last {
                let _ = write!(self.output, "{n}. ");
                *n = n.saturating_add(1);
            } else {
                self.output.push(bullet_for_depth(depth));
                self.output.push(' ');
            }
        }
    }

    /// Replace the bullet or number prefix written by
    /// [`Emitter::start_item`] with a checkbox glyph. Fires when GFM
    /// task-list parsing emits `Event::TaskListMarker` immediately
    /// after the item opens. Truncates the output buffer to the
    /// recorded prefix start and writes the glyph in its place; the
    /// recorded position is cleared so the next list item starts
    /// fresh.
    fn handle_task_list_marker(&mut self, checked: bool) {
        let Some(start) = self.item_bullet_start.take() else {
            return;
        };
        if start > self.output.len() {
            return;
        }
        self.output.truncate(start);
        self.output
            .push_str(if checked { "\u{2611} " } else { "\u{2610} " });
    }

    /// Render an inline math span as the raw `LaTeX` source in the
    /// inline-code foreground colour. The terminal cannot draw real
    /// mathematical notation, so passing the raw source through is the
    /// best signal we can give.
    fn handle_inline_math(&mut self, text: &str) {
        if self.in_image {
            self.image_alt.push_str(text);
            return;
        }
        if self.in_link {
            self.link_text.push_str(text);
            return;
        }
        self.output
            .push_str(&colour_for(INLINE_CODE_COLOUR, self.caps));
        self.output.push_str(text);
        let _ = write!(
            self.output,
            "{}",
            Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)),
        );
    }

    /// Render a display math block as an indented block in the
    /// inline-code colour. Newlines before and after detach the block
    /// from surrounding prose so the math reads as its own paragraph.
    fn handle_display_math(&mut self, text: &str) {
        if self.in_image {
            self.image_alt.push_str(text);
            return;
        }
        if self.in_link {
            self.link_text.push_str(text);
            return;
        }
        self.resolve_pending_paragraph();
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output.push_str("  ");
        self.output
            .push_str(&colour_for(INLINE_CODE_COLOUR, self.caps));
        self.output.push_str(text);
        let _ = write!(
            self.output,
            "{}",
            Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)),
        );
        self.output.push('\n');
    }

    fn end_code_block(&mut self) {
        let code = std::mem::take(&mut self.code_block_buffer);
        let lang = self.code_block_lang.take();
        let highlighted = self
            .highlighter
            .highlight(&code, lang.as_deref(), self.caps);
        self.output.push_str(&highlighted);
        self.in_code_block = false;
        if !highlighted.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn end_link(&mut self) {
        let text = std::mem::take(&mut self.link_text);
        let url = std::mem::take(&mut self.link_url);
        self.output.push_str(&hyperlink(&text, &url, self.caps));
        self.in_link = false;
    }

    fn handle_rule(&mut self) {
        let count = usize::from(self.width.max(1));
        for _ in 0..count {
            self.output.push('─');
        }
        self.output.push('\n');
    }

    /// Open a markdown table. Captures the column-alignment vector
    /// from `Tag::Table` and ensures the table starts on a fresh line.
    /// Cells, header, and row buffers are cleared so prior state from a
    /// previous table cannot leak in. `resolve_pending_paragraph`
    /// already ran in [`Emitter::handle_start`].
    fn start_table(&mut self, alignments: Vec<Alignment>) {
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.table_alignments = alignments;
        self.table_header.clear();
        self.table_rows.clear();
        self.current_row.clear();
    }

    /// Build and render the accumulated table through comfy-table.
    ///
    /// Uses [`UTF8_FULL_CONDENSED`] for clean Unicode box-drawing
    /// without per-row dividers, [`ContentArrangement::Dynamic`] so
    /// columns fit the terminal width, and [`set_width`] to clamp the
    /// total table width. Header cells receive the bold attribute;
    /// column alignments from the `Tag::Table` event are mapped to
    /// `comfy-table` `CellAlignment` values (markdown's `None` reads
    /// as left).
    ///
    /// [`UTF8_FULL_CONDENSED`]: comfy_table::presets::UTF8_FULL_CONDENSED
    /// [`ContentArrangement::Dynamic`]: comfy_table::ContentArrangement
    /// [`set_width`]: comfy_table::Table::set_width
    fn end_table(&mut self) {
        let header = std::mem::take(&mut self.table_header);
        let rows = std::mem::take(&mut self.table_rows);
        let alignments = std::mem::take(&mut self.table_alignments);

        if header.is_empty() && rows.is_empty() {
            return;
        }

        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_width(self.width)
            // Norn writes to a TTY but comfy-table can't detect that
            // through `Table::lines()`. Force ANSI styling so header
            // bold and other attributes survive into the rendered
            // output regardless of the host process's stdout type.
            .enforce_styling();

        if !header.is_empty() {
            let cells: Vec<Cell> = header
                .into_iter()
                .map(|c| Cell::new(c).add_attribute(Attribute::Bold))
                .collect();
            table.set_header(cells);
        }

        for row in rows {
            let cells: Vec<Cell> = row.into_iter().map(Cell::new).collect();
            table.add_row(cells);
        }

        for (idx, align) in alignments.iter().enumerate() {
            let mapped = pulldown_alignment_to_comfy(*align);
            if let Some(column) = table.column_mut(idx) {
                column.set_cell_alignment(mapped);
            }
        }

        for line in table.lines() {
            self.output.push_str(&line);
            self.output.push('\n');
        }
    }

    /// Emit a dim `[image: alt]` placeholder for the closed image span.
    /// The terminal cannot render the bitmap and `pulldown-cmark` does
    /// not give us a usable URL fragment to display, so the alt text is
    /// the only useful signal we can surface. Empty alt collapses to
    /// `[image]` so the placeholder is still visible.
    fn end_image(&mut self) {
        let alt = std::mem::take(&mut self.image_alt);
        self.in_image = false;
        if self.in_link {
            if alt.is_empty() {
                self.link_text.push_str("[image]");
            } else {
                let _ = write!(self.link_text, "[image: {alt}]");
            }
            return;
        }
        if alt.is_empty() {
            self.output.push_str("\x1b[2m[image]\x1b[22m");
        } else {
            let _ = write!(self.output, "\x1b[2m[image: {alt}]\x1b[22m");
        }
    }
}

/// Bullet glyph for unordered list items at a given nesting depth.
///
/// Depth 0 uses the standard bullet; deeper levels cycle through
/// white-bullet, small-square, and triangular bullet so nested lists
/// remain visually distinguishable at a glance. Ordered lists keep
/// sequential numbers at every depth (handled in
/// [`Emitter::start_item`]).
/// Map `pulldown-cmark` column alignment to comfy-table's enum.
/// Markdown's "no alignment specified" reads as left, matching the
/// default behaviour of every other markdown viewer.
fn pulldown_alignment_to_comfy(a: Alignment) -> CellAlignment {
    match a {
        Alignment::Center => CellAlignment::Center,
        Alignment::Right => CellAlignment::Right,
        Alignment::Left | Alignment::None => CellAlignment::Left,
    }
}

fn bullet_for_depth(depth: usize) -> char {
    match depth {
        0 => '\u{2022}', // •
        1 => '\u{25E6}', // ◦
        2 => '\u{25AA}', // ▪
        _ => '\u{2023}', // ‣
    }
}
