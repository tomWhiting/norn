//! Current kernel cell measurement and displayed pointer authority; no text buffer or terminal I/O.

use iridium_editor::cell_layout::{
    Affinity, CellColumn, CellRowMap, CellWrapParameters, ScreenRow,
};
use iridium_editor::editor::CellInputOptions;
use iridium_editor::{CursorState, Editor};
use iridium_tui::cell::CellBuffer;
use iridium_tui::frame::{CellFrameOptions, Frame as IridiumFrame};

use crate::TuiError;
use crate::input::editor::InputEditor;
use crate::render::frame::ComposerLayer;
use crate::render::layout::{
    Layout, LayoutPolicy, LayoutRequest, Rect, SplitPreference, UpperPane, composer_input_area,
};

#[derive(Debug, PartialEq, Eq)]
struct Source {
    document: u64,
    revision: u64,
    folds: u64,
    cursor: CursorState,
    affinities: Vec<Option<Affinity>>,
    wrap: CellWrapParameters,
}

impl Source {
    /// Pointer coordinates describe document geometry, not the selection left by
    /// a previous mouse event. Queued drags must query the current selection from
    /// the kernel without waiting for a paint between each event.
    fn same_document_geometry(&self, other: &Self) -> bool {
        self.document == other.document
            && self.revision == other.revision
            && self.folds == other.folds
            && self.wrap == other.wrap
    }

    fn capture(editor: &Editor, options: CellInputOptions) -> Self {
        let state = editor.state();
        Self {
            document: state.document.id(),
            revision: state.document.revision(),
            folds: state.fold_state.generation(),
            cursor: state.cursor.clone(),
            affinities: state
                .cursor
                .all_selections()
                .enumerate()
                .map(|(index, _)| editor.cell_cursor_affinity(index, options))
                .collect(),
            wrap: options.wrap,
        }
    }
}

struct Displayed {
    source: Source,
    extent: (u16, u16),
    area: Rect,
    first_row: ScreenRow,
}

#[derive(Debug, thiserror::Error)]
enum GeometryError {
    #[error(
        "composer pointer refers to displayed document {document} revision {displayed_revision}, but current revision or geometry changed (current revision {current_revision})"
    )]
    Stale {
        document: u64,
        displayed_revision: u64,
        current_revision: u64,
    },
    #[error("composer cells have not been displayed at the current extent")]
    Unpainted,
}

/// Fresh screen coordinates for the kernel's own pointer transaction.
pub(crate) struct ComposerPointer {
    pub row: ScreenRow,
    pub column: CellColumn,
    pub options: CellInputOptions,
}

/// One frontend owns only viewport/measurement metadata and the Iridium paint cache.
/// The editor stays in `InputEditor`; no document text or row map is retained here.
pub struct ComposerGeometry {
    extent: (u16, u16),
    options: CellInputOptions,
    first: ScreenRow,
    total: usize,
    cursor: Option<ScreenRow>,
    measured: Option<Source>,
    staged: Option<Displayed>,
    displayed: Option<Displayed>,
    renderer: IridiumFrame,
}

impl Default for ComposerGeometry {
    fn default() -> Self {
        Self {
            extent: (0, 0),
            options: CellInputOptions {
                wrap: CellWrapParameters::new(0, iridium_editor::EditorConfig::default().tab_width),
                visible_rows: 0,
            },
            first: ScreenRow(0),
            total: 0,
            cursor: None,
            measured: None,
            staged: None,
            displayed: None,
            renderer: IridiumFrame::new(),
        }
    }
}

impl ComposerGeometry {
    /// Start one host frame; a frame without composer cells commits no pointer area.
    pub(crate) fn begin_frame(&mut self) {
        self.staged = None;
    }

    /// Commit pointer authority only with the existing frame publication outcome.
    /// A failed terminal write/flush leaves the screen uncertain, so previous
    /// pointer authority is revoked too; the original error is propagated.
    pub(crate) fn finish_publication(
        &mut self,
        outcome: Result<(), TuiError>,
    ) -> Result<(), TuiError> {
        match outcome {
            Ok(()) => {
                self.displayed = self.staged.take();
                Ok(())
            }
            Err(error) => {
                self.staged = None;
                self.displayed = None;
                Err(error)
            }
        }
    }

    /// Current kernel key/pointer options after synchronization.
    #[must_use]
    pub const fn input_options(&self) -> CellInputOptions {
        self.options
    }
    /// First visible document screen row.
    #[must_use]
    pub const fn first_row(&self) -> ScreenRow {
        self.first
    }
    /// Total rows at the currently measured cell width.
    #[must_use]
    pub const fn total_rows(&self) -> usize {
        self.total
    }
    /// Primary logical caret row, including when outside a hidden viewport.
    #[must_use]
    pub const fn cursor_row(&self) -> Option<ScreenRow> {
        self.cursor
    }

    /// Unchanged metadata reuses measurement; changed source/affinity/extent prepares one map.
    pub(crate) fn measure(
        &mut self,
        editor: &InputEditor,
        columns: u16,
        rows: u16,
    ) -> Result<u16, TuiError> {
        let kernel = editor.kernel();
        let wrap = CellWrapParameters::new(usize::from(columns), kernel.state().config.tab_width);
        let candidate = CellInputOptions {
            wrap,
            visible_rows: self.options.visible_rows,
        };
        if self.extent == (columns, rows)
            && self.measured.as_ref() == Some(&Source::capture(kernel, candidate))
        {
            return coordinate(self.options.visible_rows);
        }
        let map = CellRowMap::prepare(&kernel.state().document, &kernel.state().fold_state, wrap)?;
        let total = map.total_rows();
        let height = height(total, columns, rows)?;
        let options = CellInputOptions {
            wrap,
            visible_rows: usize::from(height),
        };
        let cursor = map
            .place(kernel.cursor(), kernel.cell_affinity(options))?
            .map(|position| position.row);
        let mut first = self.first.0.min(total.saturating_sub(usize::from(height)));
        if let Some(cursor) = cursor {
            if cursor.0 < first {
                first = cursor.0;
            } else if cursor.0.saturating_sub(first) >= usize::from(height) {
                first = cursor
                    .0
                    .saturating_sub(usize::from(height).saturating_sub(1));
            }
        }
        self.extent = (columns, rows);
        self.options = options;
        self.first = ScreenRow(first);
        self.total = total;
        self.cursor = cursor;
        self.measured = Some(Source::capture(kernel, options));
        Ok(height)
    }

    /// The exact immutable borrow supplies both cell painting and the physical caret.
    pub(crate) fn prepare(
        &mut self,
        editor: &InputEditor,
        area: Rect,
    ) -> Result<(ComposerLayer, Option<(u16, u16)>), TuiError> {
        if usize::from(area.width) != self.options.wrap.columns()
            || usize::from(area.height) != self.options.visible_rows
        {
            return Err(TuiError::FrameBounds);
        }
        let current = Source::capture(editor.kernel(), self.options);
        if self.measured.as_ref() != Some(&current) {
            return Err(super::render::interaction(GeometryError::Unpainted));
        }
        let prepared = IridiumFrame::prepare_cells(
            editor.kernel(),
            CellFrameOptions {
                columns: usize::from(area.width),
                rows: usize::from(area.height),
                first_row: self.first,
                chrome: None,
            },
        )?;
        let mut cells = CellBuffer::new(usize::from(area.width), usize::from(area.height));
        let layout = self
            .renderer
            .render_cells_with_primary_caret(&prepared, &mut cells, false)?;
        let cursor = layout
            .caret()
            .map(|position| -> Result<_, TuiError> {
                Ok((
                    area.column
                        .checked_add(coordinate(position.column)?)
                        .ok_or(TuiError::FrameBounds)?,
                    area.row
                        .checked_add(coordinate(position.row)?)
                        .ok_or(TuiError::FrameBounds)?,
                ))
            })
            .transpose()?;
        self.staged = Some(Displayed {
            source: Source::capture(editor.kernel(), self.options),
            extent: self.extent,
            area,
            first_row: self.first,
        });
        Ok((ComposerLayer { area, cells }, cursor))
    }

    /// Refuse a stale displayed hit, then query fresh geometry from the actual kernel borrow.
    pub(crate) fn pointer(
        &self,
        editor: &InputEditor,
        column: u16,
        row: u16,
    ) -> Result<Option<ComposerPointer>, TuiError> {
        let displayed = self
            .displayed
            .as_ref()
            .ok_or_else(|| super::render::interaction(GeometryError::Unpainted))?;
        let current = Source::capture(editor.kernel(), self.options);
        if !displayed.source.same_document_geometry(&current)
            || displayed.extent != self.extent
            || displayed.first_row != self.first
        {
            return Err(super::render::interaction(GeometryError::Stale {
                document: displayed.source.document,
                displayed_revision: displayed.source.revision,
                current_revision: current.revision,
            }));
        }
        let Some(column) = column
            .checked_sub(displayed.area.column)
            .filter(|column| *column < displayed.area.width)
        else {
            return Ok(None);
        };
        let Some(row) = row
            .checked_sub(displayed.area.row)
            .filter(|row| *row < displayed.area.height)
        else {
            return Ok(None);
        };
        let prepared = IridiumFrame::prepare_cells(
            editor.kernel(),
            CellFrameOptions {
                columns: usize::from(displayed.area.width),
                rows: usize::from(displayed.area.height),
                first_row: displayed.first_row,
                chrome: None,
            },
        )?;
        if prepared
            .position_at(usize::from(column), usize::from(row))?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(ComposerPointer {
            row: ScreenRow(
                displayed
                    .first_row
                    .0
                    .checked_add(usize::from(row))
                    .ok_or(TuiError::FrameBounds)?,
            ),
            column: CellColumn(usize::from(column)),
            options: prepared.input_options(),
        }))
    }
}

fn height(total: usize, columns: u16, rows: u16) -> Result<u16, TuiError> {
    let requested = coordinate(total.min(usize::from(u16::MAX)))?;
    Ok(
        match Layout::calculate(
            LayoutRequest {
                columns,
                rows,
                requested_composer_rows: requested,
                changes_open: false,
                split: SplitPreference::default(),
                active_upper_pane: UpperPane::Conversation,
            },
            LayoutPolicy::default(),
        )? {
            Layout::Ready { composer, .. } => composer_input_area(composer).height,
            Layout::NoPaint | Layout::ResizeRequired { .. } => 0,
        },
    )
}

fn coordinate(value: usize) -> Result<u16, TuiError> {
    u16::try_from(value).map_err(|source| TuiError::FrameCoordinate { value, source })
}

#[cfg(test)]
#[path = "composer_geometry_tests.rs"]
mod tests;
