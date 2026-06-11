//! Layer-combination primitives shared by the per-section merge functions.
//!
//! All helpers operate on the four-layer precedence order established in
//! the module documentation (`user < project < local < cli`) and never
//! invent values: when every layer is [`None`] the result is [`None`].

use crate::config::types::HookEntry;

/// Scalar precedence: highest-layer `Some` wins; `None` falls through.
///
/// Uses [`Option::take`] on the winning layer to move without cloning.
pub(super) fn pick_scalar<T>(
    usr: &mut Option<T>,
    prj: &mut Option<T>,
    lcl: &mut Option<T>,
    ovr: &mut Option<T>,
) -> Option<T> {
    ovr.take()
        .or_else(|| lcl.take())
        .or_else(|| prj.take())
        .or_else(|| usr.take())
}

/// Concatenate the layers' string vectors in precedence order
/// (`user -> project -> local -> cli`), preserving the first occurrence of
/// each string and discarding subsequent duplicates.
///
/// Returns [`None`] when *every* layer's slot is [`None`]. A layer with
/// `Some(empty_vec)` still counts as a present (empty) contribution, so the
/// result is `Some([])` in that case --- which is what we need for the
/// deny-additive contract: an explicit empty list at one layer cannot mask
/// entries from another layer that did contribute.
pub(super) fn merge_dedup(layers: &[&Option<Vec<String>>]) -> Option<Vec<String>> {
    if layers.iter().all(|opt| opt.is_none()) {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for layer in layers {
        let Some(items) = layer.as_ref() else {
            continue;
        };
        for item in items {
            if !out.iter().any(|existing| existing == item) {
                out.push(item.clone());
            }
        }
    }
    Some(out)
}

/// Concatenate hook-entry vectors across layers, preserving precedence
/// order. Identical-looking entries are *not* deduplicated --- the same
/// command at the same matcher may be an intentional duplicate (e.g. the
/// operator wants a hook to fire twice). Returns [`None`] when every layer
/// is [`None`].
pub(super) fn concat_hook_slot(layers: &[&Option<Vec<HookEntry>>]) -> Option<Vec<HookEntry>> {
    if layers.iter().all(|opt| opt.is_none()) {
        return None;
    }
    let mut out: Vec<HookEntry> = Vec::new();
    for layer in layers {
        if let Some(entries) = layer.as_ref() {
            out.extend(entries.iter().cloned());
        }
    }
    Some(out)
}

/// Concatenate path-string vectors in precedence order, deduplicating to
/// keep the first occurrence of each path. Behaviour matches
/// [`merge_dedup`] --- kept as a separate helper for readability since the
/// motivation (path discovery extends, not replaces) is documented next to
/// the call.
pub(super) fn concat_string_paths(layers: &[&Option<Vec<String>>]) -> Option<Vec<String>> {
    merge_dedup(layers)
}
