//! Geometry acceptance: screen ownership, narrow modes, declared policies and boundary sizes.

use super::*;

fn request(columns: u16, rows: u16) -> LayoutRequest {
    LayoutRequest {
        columns,
        rows,
        requested_composer_rows: 3,
        changes_open: true,
        split: SplitPreference::default(),
        active_upper_pane: UpperPane::Changes,
    }
}

#[test]
fn narrow_threshold_and_odd_surplus() -> Result<(), LayoutError> {
    assert_eq!(LayoutPolicy::default().split_threshold(), 81);
    let narrow = Layout::calculate(request(80, 20), LayoutPolicy::default())?;
    assert!(matches!(
        narrow,
        Layout::Ready {
            upper: UpperLayout::Single {
                pane: UpperPane::Changes,
                area: Rect { width: 80, .. }
            },
            composer: Rect {
                column: 0,
                width: 80,
                row: 17,
                height: 3
            },
        }
    ));
    for (width, left, right) in [(81, 40, 40), (82, 41, 40)] {
        let actual = Layout::calculate(request(width, 20), LayoutPolicy::default())?;
        assert_eq!(
            actual,
            Layout::Ready {
                upper: UpperLayout::Split {
                    conversation: Rect {
                        column: 0,
                        row: 0,
                        width: left,
                        height: 17
                    },
                    divider: Rect {
                        column: left,
                        row: 0,
                        width: 1,
                        height: 17
                    },
                    changes: Rect {
                        column: left + 1,
                        row: 0,
                        width: right,
                        height: 17
                    },
                },
                composer: Rect {
                    column: 0,
                    row: 17,
                    width,
                    height: 3
                },
            }
        );
    }
    Ok(())
}

#[test]
fn zero_and_resize_required_keep_the_request_unchanged() -> Result<(), LayoutError> {
    for (cols, rows) in [(0, 0), (80, 0), (0, 20)] {
        assert_eq!(
            Layout::calculate(request(cols, rows), LayoutPolicy::default())?,
            Layout::NoPaint
        );
    }
    assert_eq!(
        Layout::calculate(request(80, 1), LayoutPolicy::default())?,
        Layout::ResizeRequired {
            area: Rect {
                column: 0,
                row: 0,
                width: 80,
                height: 1
            },
        }
    );
    assert!(matches!(
        Layout::calculate(request(80, 2), LayoutPolicy::default())?,
        Layout::Ready {
            composer: Rect {
                row: 1,
                height: 1,
                ..
            },
            ..
        }
    ));
    Ok(())
}

#[test]
fn closed_changes_never_hides_conversation() -> Result<(), LayoutError> {
    for width in [1, 80, 81, u16::MAX] {
        let mut input = request(width, 20);
        input.changes_open = false;
        assert!(matches!(
            Layout::calculate(input, LayoutPolicy::default())?,
            Layout::Ready {
                upper: UpperLayout::Single {
                    pane: UpperPane::Conversation,
                    ..
                },
                ..
            }
        ));
        input.changes_open = true;
        input.active_upper_pane = UpperPane::Conversation;
        if width < 81 {
            assert!(matches!(
                Layout::calculate(input, LayoutPolicy::default())?,
                Layout::Ready {
                    upper: UpperLayout::Single {
                        pane: UpperPane::Conversation,
                        ..
                    },
                    ..
                }
            ));
        }
    }
    Ok(())
}

#[test]
fn composer_is_full_width_and_capped_without_footer() -> Result<(), LayoutError> {
    for (rows, requested, expected) in [(20, 0, 1), (20, u16::MAX, 10), (80, 40, 12)] {
        let mut input = request(120, rows);
        input.requested_composer_rows = requested;
        let layout = Layout::calculate(input, LayoutPolicy::default())?;
        assert!(matches!(layout, Layout::Ready { composer, .. }
            if composer.column == 0 && composer.width == 120
                && composer.height == expected && composer.row + composer.height == rows));
    }
    Ok(())
}

#[test]
fn custom_policy_and_preference_survive_shrink_and_widen() -> Result<(), Box<dyn std::error::Error>>
{
    let policy = LayoutPolicy::new(NonZeroU16::try_from(10)?, NonZeroU16::try_from(4)?);
    let mut input = request(101, 40);
    input.split = SplitPreference::new(NonZeroU16::try_from(3)?, NonZeroU16::MIN);
    input.requested_composer_rows = 12;
    let original = input;
    let wide = Layout::calculate(input, policy)?;
    assert!(matches!(
        wide,
        Layout::Ready {
            upper: UpperLayout::Split {
                conversation: Rect { width: 75, .. },
                changes: Rect { width: 25, .. },
                ..
            },
            composer: Rect { height: 4, .. },
        }
    ));
    for width in [21, 20, 1, 0, 21, 50, 101] {
        input.columns = width;
        let layout = Layout::calculate(input, policy)?;
        assert_eq!(input.split.weights(), (3, 1));
        if width == 21 {
            assert!(matches!(
                layout,
                Layout::Ready {
                    upper: UpperLayout::Split {
                        conversation: Rect { width: 10, .. },
                        changes: Rect { width: 10, .. },
                        ..
                    },
                    ..
                }
            ));
        }
    }
    assert_eq!(input, original);
    assert_eq!(Layout::calculate(input, policy)?, wide);
    Ok(())
}

fn assert_contained(rect: Rect, cols: u16, rows: u16) {
    assert!(rect.width > 0 && rect.height > 0);
    assert!(u32::from(rect.column) + u32::from(rect.width) <= u32::from(cols));
    assert!(u32::from(rect.row) + u32::from(rect.height) <= u32::from(rows));
}

#[test]
fn extreme_geometry_and_weighted_splits_never_overlap() -> Result<(), Box<dyn std::error::Error>> {
    let extreme = SplitPreference::new(NonZeroU16::MAX, NonZeroU16::MIN);
    let reverse = SplitPreference::new(NonZeroU16::MIN, NonZeroU16::MAX);
    let huge_min = LayoutPolicy::new(NonZeroU16::MAX, NonZeroU16::MAX);
    for policy in [LayoutPolicy::default(), huge_min] {
        for split in [SplitPreference::default(), extreme, reverse] {
            for columns in [1, 40, 80, 81, 82, 120, u16::MAX] {
                for rows in [2, 3, 20, u16::MAX] {
                    let mut input = request(columns, rows);
                    input.split = split;
                    input.requested_composer_rows = u16::MAX;
                    let result = Layout::calculate(input, policy)?;
                    let Layout::Ready { upper, composer } = result else {
                        return Err(std::io::Error::other(format!(
                            "usable geometry produced {result:?}"
                        ))
                        .into());
                    };
                    assert_contained(composer, columns, rows);
                    assert_eq!(composer.width, columns);
                    assert_eq!(composer.row + composer.height, rows);
                    match upper {
                        UpperLayout::Single { area, .. } => {
                            assert_contained(area, columns, rows);
                            assert_eq!(area.row + area.height, composer.row);
                            assert_eq!(area.width, columns);
                        }
                        UpperLayout::Split {
                            conversation,
                            divider,
                            changes,
                        } => {
                            for area in [conversation, divider, changes] {
                                assert_contained(area, columns, rows);
                                assert_eq!(area.row + area.height, composer.row);
                            }
                            assert_eq!(conversation.column + conversation.width, divider.column);
                            assert_eq!(divider.column + divider.width, changes.column);
                            assert_eq!(changes.column + changes.width, columns);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
