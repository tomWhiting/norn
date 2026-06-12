//! Shared accumulator for the subtree usage delivered by child agents
//! (Wave 3, W3.6 usage rollup).
//!
//! Every [`ChildAgentResult`](crate::agent::result_channel::ChildAgentResult)
//! drained into a running loop carries the child's `subtree_usage` — the
//! child's own provider spend plus everything its own descendants
//! delivered. [`ChildrenUsage`] is where the draining loop folds those
//! values, and it is deliberately a *shared handle* (cheap-clone
//! `Arc<Mutex<Usage>>`) rather than a plain field:
//!
//! - The fold happens inside the loop task
//!   ([`drain_child_results`](crate::r#loop::delivery) is the single
//!   injection path), which runs on its own `tokio::spawn` inside the
//!   spawn/fork completion wrappers.
//! - The wrapper must still read the folded value when that inner task
//!   **panics** (a dependency unwinding inside a tool or provider) or the
//!   run ends in a hard [`NornError`](crate::error::NornError): the
//!   panicked node's own usage is honestly unknown (zeros), but its
//!   children's *delivered* subtree usage was real spend and must still
//!   roll up — partial truth beats silent loss.
//!
//! A plain `Usage` field on [`LoopContext`](crate::r#loop::loop_context::LoopContext)
//! would unwind with the panicked task; the shared handle survives in the
//! wrapper's clone.
//!
//! Double-counting within a step is impossible by construction: a
//! parent's own `total_usage` only ever accumulates its *own* provider
//! calls, each child result is consumed exactly once from the bounded
//! mpsc channel (while the receiver is installed on the loop), and
//! every consumed result is folded here exactly once. Across steps the
//! guarantee is held by [`ChildrenUsage::reset`]: `run_agent_step`
//! clears the accumulator at entry, so each step's snapshots cover only
//! that step's deliveries even when one `LoopContext` serves a whole
//! interactive session.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::provider::usage::Usage;

/// Shared accumulator of the `subtree_usage` carried by every child
/// result delivered into one agent's loop.
///
/// Clones share the same underlying value: the loop folds into it via
/// [`Self::add`] as results are drained, and the spawn/fork completion
/// wrapper reads it via [`Self::snapshot`] — including after a panic or
/// hard error, when no [`AgentStepResult`](crate::r#loop::config::AgentStepResult)
/// exists to carry the value out (see the module docs for why this is a
/// shared handle). Uses `parking_lot::Mutex`, which does not poison, so
/// no lock-failure path exists to mishandle.
#[derive(Clone, Debug, Default)]
pub struct ChildrenUsage(Arc<Mutex<Usage>>);

impl ChildrenUsage {
    /// Fold one delivered child result's `subtree_usage` into the
    /// accumulator.
    pub fn add(&self, subtree_usage: &Usage) {
        let mut total = self.0.lock();
        *total += subtree_usage.clone();
    }

    /// Read the current accumulated total.
    #[must_use]
    pub fn snapshot(&self) -> Usage {
        self.0.lock().clone()
    }

    /// Clear the accumulator to zero.
    ///
    /// Called by `run_agent_step` at step entry: every step-result arm
    /// documents its `children_usage` as the results delivered into
    /// **that step**, and a reused `LoopContext` (interactive surfaces
    /// run many steps over one context) would otherwise carry an
    /// earlier turn's children into every later snapshot — multi-
    /// counting them in any cross-step sum (REVIEW W3.6 HIGH-1).
    pub fn reset(&self) {
        *self.0.lock() = Usage::default();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    #[test]
    fn default_snapshot_is_zero() {
        let acc = ChildrenUsage::default();
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 0);
        assert_eq!(snap.output_tokens, 0);
        assert!(snap.cost_usd.is_none());
    }

    #[test]
    fn add_accumulates_and_clones_share_state() {
        let acc = ChildrenUsage::default();
        let handle = acc.clone();
        acc.add(&usage(10, 5));
        handle.add(&usage(3, 2));
        let snap = acc.snapshot();
        assert_eq!(snap.input_tokens, 13);
        assert_eq!(snap.output_tokens, 7);
        let via_clone = handle.snapshot();
        assert_eq!(via_clone.input_tokens, 13, "clones share one accumulator");
    }

    #[test]
    fn snapshot_survives_dropping_the_folding_clone() {
        // Models the panic path: the loop-side handle folds and is then
        // dropped (task unwound); the wrapper-side clone still reads
        // the folded value.
        let loop_side = ChildrenUsage::default();
        let wrapper_handle = loop_side.clone();
        loop_side.add(&usage(7, 3));
        drop(loop_side);
        let snap = wrapper_handle.snapshot();
        assert_eq!(snap.input_tokens, 7);
        assert_eq!(snap.output_tokens, 3);
    }

    // -- Rollup model: associativity over arbitrary trees -------------
    //
    // The workspace carries no property-testing crate (no proptest /
    // quickcheck in any Cargo.toml), so this is a deterministic
    // randomized-shape test: a seeded LCG generates arbitrary trees and
    // the rollup invariants are checked over every generated shape.

    /// A node in a synthetic usage tree.
    struct Node {
        own: Usage,
        children: Vec<Node>,
    }

    /// The rollup under test: `subtree(n) = own(n) + Σ subtree(child)`,
    /// folding children **in the given order** through a
    /// [`ChildrenUsage`] accumulator — exactly the production fold shape
    /// (drain folds each delivered `subtree_usage`, the wrapper adds the
    /// node's own usage).
    fn rollup(node: &Node, order: &mut dyn FnMut(usize) -> Vec<usize>) -> Usage {
        let acc = ChildrenUsage::default();
        for idx in order(node.children.len()) {
            let child_subtree = rollup(&node.children[idx], order);
            acc.add(&child_subtree);
        }
        node.own.clone() + acc.snapshot()
    }

    /// Ground truth: the flat sum of every node's own usage — each node
    /// counted exactly once, independent of tree shape or fold order.
    fn flat_sum(node: &Node, total: &mut Usage) {
        *total += node.own.clone();
        for child in &node.children {
            flat_sum(child, total);
        }
    }

    /// Minimal deterministic LCG (Numerical Recipes constants) so the
    /// shapes are reproducible without a new dependency.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    /// Generate a random tree of bounded depth/fan-out with distinct
    /// per-node token counts (so a double count or a dropped node always
    /// changes the total).
    fn gen_tree(rng: &mut Lcg, depth: u64, counter: &mut u64) -> Node {
        *counter += 1;
        let own = Usage {
            input_tokens: 1 + rng.below(1_000),
            output_tokens: 1 + rng.below(1_000),
            cache_read_tokens: rng.below(100),
            cache_write_tokens: rng.below(100),
            cost_usd: Some(f64::from(u32::try_from(rng.below(10_000)).unwrap_or(0)) / 100.0),
        };
        let fanout = if depth == 0 { 0 } else { rng.below(4) };
        let children = (0..fanout)
            .map(|_| gen_tree(rng, depth - 1, counter))
            .collect();
        Node { own, children }
    }

    /// Property (W3.6 test strategy): the rollup is associative over
    /// arbitrary trees — folding children forward, reversed, or by
    /// rotated order yields the identical root subtree total, and that
    /// total always equals the flat sum of every node's own usage
    /// (each node exactly once: no double count, no loss).
    #[test]
    fn rollup_is_associative_and_sums_each_node_exactly_once() {
        let mut rng = Lcg(0x5707_F636);
        for round in 0..200 {
            let mut node_count = 0;
            let tree = gen_tree(&mut rng, 3, &mut node_count);

            let forward = rollup(&tree, &mut |n| (0..n).collect());
            let reversed = rollup(&tree, &mut |n| (0..n).rev().collect());
            let rotated = rollup(&tree, &mut |n| {
                if n == 0 {
                    Vec::new()
                } else {
                    (0..n).map(|i| (i + 1) % n).collect()
                }
            });

            let mut expected = Usage::default();
            flat_sum(&tree, &mut expected);

            for (label, got) in [
                ("forward", &forward),
                ("reversed", &reversed),
                ("rotated", &rotated),
            ] {
                assert_eq!(
                    got.input_tokens, expected.input_tokens,
                    "round {round} ({node_count} nodes, {label}): input tokens",
                );
                assert_eq!(
                    got.output_tokens, expected.output_tokens,
                    "round {round} ({node_count} nodes, {label}): output tokens",
                );
                assert_eq!(
                    got.cache_read_tokens, expected.cache_read_tokens,
                    "round {round} ({node_count} nodes, {label}): cache reads",
                );
                assert_eq!(
                    got.cache_write_tokens, expected.cache_write_tokens,
                    "round {round} ({node_count} nodes, {label}): cache writes",
                );
                let got_cost = got.cost_usd.unwrap_or(0.0);
                let expected_cost = expected.cost_usd.unwrap_or(0.0);
                assert!(
                    (got_cost - expected_cost).abs() < 1e-9,
                    "round {round} ({node_count} nodes, {label}): cost {got_cost} vs {expected_cost}",
                );
            }
        }
    }
}
