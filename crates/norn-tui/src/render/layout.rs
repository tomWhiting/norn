//! Pure retained-screen geometry; terminal I/O and transcript state belong to their owners.

use std::fmt;
use std::num::NonZeroU16;

/// Declared minimum content columns on either side of an open split.
pub const DEFAULT_MIN_PANE_COLUMNS: u16 = 40;
/// Declared maximum visible composer rows, preserving the existing editor policy.
pub const DEFAULT_MAX_COMPOSER_ROWS: u16 = 12;
/// Columns occupied by the divider between upper panes.
pub const DIVIDER_COLUMNS: u16 = 1;

/// A zero-based terminal rectangle, excluding any surrounding chrome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    /// First column.
    pub column: u16,
    /// First row.
    pub row: u16,
    /// Number of columns.
    pub width: u16,
    /// Number of rows.
    pub height: u16,
}

/// Upper pane selected when only one can fit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpperPane {
    /// Retained conversation.
    Conversation,
    /// Recorded tool changes.
    Changes,
}

/// Positive relative widths, retained unchanged when actual geometry is clamped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplitPreference {
    conversation: u16,
    changes: u16,
}

impl SplitPreference {
    /// Declare relative widths; neither pane can have zero weight.
    #[must_use]
    pub const fn new(conversation: NonZeroU16, changes: NonZeroU16) -> Self {
        Self {
            conversation: conversation.get(),
            changes: changes.get(),
        }
    }

    /// The original relative widths, independent of current terminal geometry.
    #[must_use]
    pub const fn weights(self) -> (u16, u16) {
        (self.conversation, self.changes)
    }
}

impl Default for SplitPreference {
    fn default() -> Self {
        Self::new(NonZeroU16::MIN, NonZeroU16::MIN)
    }
}

/// Explicit layout policy. Positive overrides are accepted at construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutPolicy {
    min_pane_columns: u16,
    max_composer_rows: u16,
}

impl LayoutPolicy {
    /// Override declared pane and composer sizes without introducing hidden limits.
    #[must_use]
    pub const fn new(min_pane_columns: NonZeroU16, max_composer_rows: NonZeroU16) -> Self {
        Self {
            min_pane_columns: min_pane_columns.get(),
            max_composer_rows: max_composer_rows.get(),
        }
    }

    /// Minimum total width at which a split fits, including its divider.
    #[must_use]
    pub fn split_threshold(self) -> u32 {
        u32::from(self.min_pane_columns) * 2 + u32::from(DIVIDER_COLUMNS)
    }
}

impl Default for LayoutPolicy {
    fn default() -> Self {
        Self {
            min_pane_columns: DEFAULT_MIN_PANE_COLUMNS,
            max_composer_rows: DEFAULT_MAX_COMPOSER_ROWS,
        }
    }
}

/// All geometry inputs; calculating a layout never mutates saved preferences or focus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutRequest {
    /// Current terminal width, including zero during a transient resize.
    pub columns: u16,
    /// Current terminal height, including zero during a transient resize.
    pub rows: u16,
    /// Editor's requested visual height before this layout applies its cap.
    pub requested_composer_rows: u16,
    /// Whether the person has opened Changes.
    pub changes_open: bool,
    /// Saved desired split, retained through narrow layouts.
    pub split: SplitPreference,
    /// Upper pane to display when an open split cannot fit.
    pub active_upper_pane: UpperPane,
}

/// Upper content geometry; no global header or footer consumes a row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpperLayout {
    /// One pane takes all available upper columns.
    Single {
        /// Which pane is visible.
        pane: UpperPane,
        /// Content rectangle.
        area: Rect,
    },
    /// Both upper panes are visible, separated by a vertical divider.
    Split {
        /// Conversation rectangle.
        conversation: Rect,
        /// Divider rectangle, confined to the upper region.
        divider: Rect,
        /// Changes rectangle.
        changes: Rect,
    },
}

/// One calculated screen state, with no terminal side effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// Zero-sized geometry: retain application state and write nothing.
    NoPaint,
    /// Insufficient height for composer and content; paint a clipped resize notice.
    ResizeRequired {
        /// The available terminal area.
        area: Rect,
    },
    /// Content above the full-width composer.
    Ready {
        /// Upper pane rectangles.
        upper: UpperLayout,
        /// Full-width input rectangle, reaching the last terminal row.
        composer: Rect,
    },
}

/// A calculated split width could not be represented as terminal coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutError {
    calculated_width: u32,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "calculated conversation width {} exceeds terminal coordinate range",
            self.calculated_width
        )
    }
}

impl std::error::Error for LayoutError {}

impl Layout {
    /// Calculate bounded pane rectangles, preserving the requested split and focus.
    ///
    /// # Errors
    /// Returns a named arithmetic error if a split cannot fit the coordinate type.
    pub fn calculate(request: LayoutRequest, policy: LayoutPolicy) -> Result<Self, LayoutError> {
        if request.columns == 0 || request.rows == 0 {
            return Ok(Self::NoPaint);
        }
        if request.rows < 2 {
            return Ok(Self::ResizeRequired {
                area: Rect {
                    column: 0,
                    row: 0,
                    width: request.columns,
                    height: request.rows,
                },
            });
        }
        let composer_cap = policy.max_composer_rows.min(request.rows / 2);
        let composer_rows = request.requested_composer_rows.clamp(1, composer_cap);
        let upper_rows = request.rows - composer_rows;
        let composer = Rect {
            column: 0,
            row: upper_rows,
            width: request.columns,
            height: composer_rows,
        };
        let upper =
            if request.changes_open && u32::from(request.columns) >= policy.split_threshold() {
                split_upper(request, policy, upper_rows)?
            } else {
                UpperLayout::Single {
                    pane: if request.changes_open {
                        request.active_upper_pane
                    } else {
                        UpperPane::Conversation
                    },
                    area: Rect {
                        column: 0,
                        row: 0,
                        width: request.columns,
                        height: upper_rows,
                    },
                }
            };
        Ok(Self::Ready { upper, composer })
    }
}

fn split_upper(
    request: LayoutRequest,
    policy: LayoutPolicy,
    upper_rows: u16,
) -> Result<UpperLayout, LayoutError> {
    let available = request.columns - DIVIDER_COLUMNS;
    let numerator = u32::from(available) * u32::from(request.split.conversation);
    let denominator = u32::from(request.split.conversation) + u32::from(request.split.changes);
    // Conversation receives an indivisible surplus column, including the default equal split.
    let preferred = numerator.div_ceil(denominator);
    let preferred = u16::try_from(preferred).map_err(|_| LayoutError {
        calculated_width: preferred,
    })?;
    let left = preferred.clamp(policy.min_pane_columns, available - policy.min_pane_columns);
    Ok(UpperLayout::Split {
        conversation: Rect {
            column: 0,
            row: 0,
            width: left,
            height: upper_rows,
        },
        divider: Rect {
            column: left,
            row: 0,
            width: DIVIDER_COLUMNS,
            height: upper_rows,
        },
        changes: Rect {
            column: left + DIVIDER_COLUMNS,
            row: 0,
            width: available - left,
            height: upper_rows,
        },
    })
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
