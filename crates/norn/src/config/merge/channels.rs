//! Merge optional channel sections without inventing policy or retention values.

use crate::config::channels::ChannelSettings;

pub(super) fn merge_channels(
    user: &mut Option<ChannelSettings>,
    project: &mut Option<ChannelSettings>,
    local: &mut Option<ChannelSettings>,
    cli: &mut Option<ChannelSettings>,
) -> Option<ChannelSettings> {
    let mut merged: Option<ChannelSettings> = None;
    for layer in [user, project, local, cli] {
        if let Some(higher) = layer.take() {
            match &mut merged {
                Some(lower) => lower.overlay(higher),
                None => merged = Some(higher),
            }
        }
    }
    merged
}
