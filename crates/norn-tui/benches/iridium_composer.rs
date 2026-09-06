//! Local samples of the public composer/kernel and retained Frame assembly, with no timing gate.
//! This does not measure the private App measurement cache, terminal I/O or provider startup.
//! Iridium parser absence is dependency-graph evidence, not a synthetic runtime counter.

use std::hint::black_box;
use std::num::NonZeroUsize;
use std::time::Instant;

use iridium_editor::cell_layout::{CellRowMap, CellWrapParameters, ScreenRow};
use iridium_editor::editor::{CellInputOptions, CellReplacementCursor};
use iridium_editor::{
    CommandArgs, CursorState, EditorKeyResult, KeyCode, KeyEvent, Position, Range,
};
use iridium_tui::cell::CellBuffer;
use iridium_tui::frame::{CellFrameOptions, Frame as IridiumFrame};
use norn_tui::input::{InputEditor, InputHistory};
use norn_tui::render::frame::{ComposerLayer, Frame, PreparedFrame};
use norn_tui::render::layout::{
    Layout, LayoutPolicy, LayoutRequest, SplitPreference, UpperPane, composer_input_area,
};
use norn_tui::terminal::caps::TerminalCaps;
use serde_json::json;

type BenchResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn main() -> BenchResult {
    // Sample count controls this experiment only; it is not an editor capacity or performance bar.
    let samples = sample_count(std::env::args().skip(1))?;
    let unit = "e\u{301} 🇦🇺 👩‍💻 界 ";
    let long = unit.repeat(1_000);
    for (name, original, middle, end) in [
        (
            "short",
            "hello world".to_owned(),
            Position::new(0, 5),
            Position::new(0, 11),
        ),
        (
            "multiline",
            "first line\n    e\u{301} 🇦🇺 👩‍💻\nlast line".to_owned(),
            Position::new(1, 4),
            Position::new(2, 9),
        ),
        (
            "long_unicode",
            long,
            Position::new(0, unit.chars().count() * 500),
            Position::new(0, unit.chars().count() * 1_000),
        ),
    ] {
        for (location, caret) in [("middle", middle), ("end", end)] {
            for (columns, rows) in [(120, 40), (240, 80)] {
                sample(name, &original, location, caret, columns, rows, samples)?;
            }
        }
    }
    Ok(())
}

fn sample_count(arguments: impl IntoIterator<Item = String>) -> BenchResult<NonZeroUsize> {
    let mut samples = None;
    for argument in arguments {
        match argument.as_str() {
            "--test" | "--bench" | "--nocapture" => {}
            value => {
                let count = value.parse::<usize>().map_err(|source| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput,
                        format!("unknown benchmark argument {value:?}: expected a positive sample count or --test/--bench/--nocapture: {source}"))
                })?;
                if samples.is_some() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("duplicate benchmark sample count {value:?}"),
                    )
                    .into());
                }
                samples = Some(count);
            }
        }
    }
    NonZeroUsize::new(samples.unwrap_or(50))
        .ok_or_else(|| "benchmark sample count must be positive".into())
}

fn sample(
    name: &str,
    original: &str,
    location: &str,
    caret: Position,
    columns: u16,
    rows: u16,
    samples: NonZeroUsize,
) -> BenchResult {
    let startup = Instant::now();
    let mut editor = InputEditor::new(InputHistory::in_memory());
    let construction_ns = startup.elapsed().as_nanos();
    assert_eq!(editor.kernel().language(), None);
    editor.paste_cells(original)?;
    editor.replace_cells(
        Range::empty(Position::zero()),
        "",
        CellReplacementCursor::Exact(CursorState::at(caret)),
    )?;
    let mut renderer = IridiumFrame::new();
    let (mut previous, options) = prepare(&editor, &mut renderer, columns, rows)?;
    let initial_bytes = previous.encode_delta(None)?.len();
    let mut edit_ns = Vec::with_capacity(samples.get());
    let mut unchanged_ns = Vec::with_capacity(samples.get());
    let mut emitted_bytes = Vec::with_capacity(samples.get());
    for _ in 0..samples.get() {
        let start = Instant::now();
        assert_eq!(
            editor.handle_cell_key(&KeyEvent::simple(KeyCode::Char('x')), options)?,
            EditorKeyResult::None
        );
        let (current, _) = prepare(&editor, &mut renderer, columns, rows)?;
        let bytes = current.encode_delta(Some(&previous))?;
        edit_ns.push(start.elapsed().as_nanos());
        assert!(!bytes.is_empty(), "visible insertion produced no delta");
        emitted_bytes.push(black_box(bytes).len());
        let start = Instant::now();
        let (unchanged, _) = prepare(&editor, &mut renderer, columns, rows)?;
        let bytes = unchanged.encode_delta(Some(&current))?;
        unchanged_ns.push(start.elapsed().as_nanos());
        assert!(
            black_box(bytes).is_empty(),
            "unchanged composed frame emitted bytes"
        );
        assert_eq!(
            editor.run_cell_command("history.undo", CommandArgs::NONE, options)?,
            EditorKeyResult::None
        );
        (previous, _) = prepare(&editor, &mut renderer, columns, rows)?;
    }
    assert_eq!(
        editor.text(),
        original,
        "benchmark undo did not restore original bytes"
    );
    assert_eq!(editor.kernel().language(), None);
    println!(
        "{}",
        json!({
            "scope":"public InputEditor + kernel geometry + Iridium cells + retained Norn Frame; no private App cache or TTY",
            "case":name, "location":location, "columns":columns, "rows":rows,
            "original_bytes":original.len(), "samples":samples.get(), "construction_ns":construction_ns,
            "key_prepare_paint_ns":summary(&mut edit_ns), "unchanged_prepare_paint_ns":summary(&mut unchanged_ns),
            "initial_frame_bytes":initial_bytes, "changed_frame_bytes":emitted_bytes,
            "unchanged_frame_bytes":0, "kernel_frame_preparations":1+3*samples.get(),
            "host_frame_preparations":1+3*samples.get(), "language_is_unset":editor.kernel().language().is_none(),
            "syntax_evidence":{
                "kind":"structural dependency graph; not a runtime measurement",
                "required_companion_check":"Iridium syntax feature and iridium-syntax package absent from the exact resolved graph",
                "runtime_counters":"unavailable in the parser-free kernel; no zero counts synthesized"
            }
        })
    );
    Ok(())
}

fn summary(values: &mut [u128]) -> serde_json::Value {
    values.sort_unstable();
    json!({"minimum":values.first(), "median":values.get(values.len()/2), "maximum":values.last()})
}

fn prepare(
    editor: &InputEditor,
    renderer: &mut IridiumFrame,
    columns: u16,
    rows: u16,
) -> BenchResult<(PreparedFrame, CellInputOptions)> {
    let kernel = editor.kernel();
    let wrap = CellWrapParameters::new(usize::from(columns), kernel.get_config().tab_width);
    let map = CellRowMap::prepare(&kernel.state().document, &kernel.state().fold_state, wrap)?;
    let layout = Layout::calculate(
        LayoutRequest {
            columns,
            rows,
            requested_composer_rows: u16::try_from(map.total_rows())?,
            changes_open: false,
            split: SplitPreference::default(),
            active_upper_pane: UpperPane::Conversation,
        },
        LayoutPolicy::default(),
    )?;
    let Layout::Ready { composer, .. } = layout else {
        return Err("benchmark positive viewport did not yield a composer".into());
    };
    let area = composer_input_area(composer);
    let options = CellInputOptions {
        wrap,
        visible_rows: usize::from(area.height),
    };
    let placement = map
        .place(kernel.cursor(), kernel.cell_affinity(options))?
        .ok_or("benchmark caret is unavailable")?;
    let first_row = ScreenRow(
        placement
            .row
            .0
            .saturating_sub(usize::from(area.height).saturating_sub(1)),
    );
    let prepared = IridiumFrame::prepare_cells(
        kernel,
        CellFrameOptions {
            columns: usize::from(area.width),
            rows: usize::from(area.height),
            first_row,
            chrome: None,
        },
    )?;
    let mut cells = CellBuffer::new(usize::from(area.width), usize::from(area.height));
    let result = renderer.render_cells_with_primary_caret(&prepared, &mut cells, false)?;
    let caret = result
        .caret()
        .ok_or("benchmark primary caret was not visible")?;
    let frame = Frame {
        layout,
        rows: Vec::new(),
        composer: Some(ComposerLayer { area, cells }),
        cursor: Some((
            area.column + u16::try_from(caret.column)?,
            area.row + u16::try_from(caret.row)?,
        )),
    };
    Ok((
        frame.prepare(&TerminalCaps {
            true_colour: true,
            ..TerminalCaps::baseline()
        })?,
        options,
    ))
}
