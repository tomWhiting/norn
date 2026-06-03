//! Effect-based parallel execution.

use serde::{Deserialize, Serialize};

/// Declared side effect of a tool, used for scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolEffect {
    /// No side effects — safe to run concurrently.
    ReadOnly,
    /// Writes to disk — must be serialized.
    Write,
    /// Runs an external process — serialized by default.
    Process,
    /// Network I/O — safe to run concurrently.
    Network,
    /// Effect unknown — serialized for safety.
    Unknown,
}

impl ToolEffect {
    /// Returns true if this effect is safe for concurrent execution.
    fn is_concurrent(self) -> bool {
        matches!(self, Self::ReadOnly | Self::Network)
    }
}

/// A single step in a scheduling plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionStep {
    /// Multiple tool calls that can run concurrently.
    Concurrent {
        /// Tool call IDs to execute in parallel.
        tool_call_ids: Vec<String>,
    },
    /// A single tool call that must run alone.
    Serial {
        /// The tool call ID to execute.
        tool_call_id: String,
    },
}

/// An execution plan that orders tool calls by effect.
///
/// Adjacent concurrent-eligible calls are batched together.
/// Serial calls each get their own step.
#[derive(Clone, Debug)]
pub struct SchedulingPlan {
    /// Ordered execution steps.
    pub steps: Vec<ExecutionStep>,
}

impl SchedulingPlan {
    /// Builds a scheduling plan from tool calls and their effects.
    ///
    /// Scans left to right, batching adjacent concurrent-eligible calls.
    /// Serial calls flush any pending batch and become their own step.
    pub fn build(calls: &[(String, ToolEffect)]) -> Self {
        let mut steps = Vec::new();
        let mut concurrent_batch: Vec<String> = Vec::new();

        for (id, effect) in calls {
            if effect.is_concurrent() {
                concurrent_batch.push(id.clone());
            } else {
                if !concurrent_batch.is_empty() {
                    steps.push(ExecutionStep::Concurrent {
                        tool_call_ids: std::mem::take(&mut concurrent_batch),
                    });
                }
                steps.push(ExecutionStep::Serial {
                    tool_call_id: id.clone(),
                });
            }
        }

        if !concurrent_batch.is_empty() {
            steps.push(ExecutionStep::Concurrent {
                tool_call_ids: concurrent_batch,
            });
        }

        Self { steps }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    #[test]
    fn mixed_effects_scheduling() {
        let calls = vec![
            ("read1".to_string(), ToolEffect::ReadOnly),
            ("read2".to_string(), ToolEffect::ReadOnly),
            ("read3".to_string(), ToolEffect::ReadOnly),
            ("write1".to_string(), ToolEffect::Write),
            ("net1".to_string(), ToolEffect::Network),
            ("write2".to_string(), ToolEffect::Write),
        ];

        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 4);

        assert_eq!(
            plan.steps[0],
            ExecutionStep::Concurrent {
                tool_call_ids: vec![
                    "read1".to_string(),
                    "read2".to_string(),
                    "read3".to_string()
                ]
            }
        );
        assert_eq!(
            plan.steps[1],
            ExecutionStep::Serial {
                tool_call_id: "write1".to_string()
            }
        );
        assert_eq!(
            plan.steps[2],
            ExecutionStep::Concurrent {
                tool_call_ids: vec!["net1".to_string()]
            }
        );
        assert_eq!(
            plan.steps[3],
            ExecutionStep::Serial {
                tool_call_id: "write2".to_string()
            }
        );
    }

    #[test]
    fn all_serial_effects() {
        let calls = vec![
            ("w1".to_string(), ToolEffect::Write),
            ("p1".to_string(), ToolEffect::Process),
            ("u1".to_string(), ToolEffect::Unknown),
        ];
        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 3);
        assert!(
            plan.steps
                .iter()
                .all(|s| matches!(s, ExecutionStep::Serial { .. }))
        );
    }

    #[test]
    fn all_concurrent_effects() {
        let calls = vec![
            ("r1".to_string(), ToolEffect::ReadOnly),
            ("n1".to_string(), ToolEffect::Network),
            ("r2".to_string(), ToolEffect::ReadOnly),
        ];
        let plan = SchedulingPlan::build(&calls);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(
            plan.steps[0],
            ExecutionStep::Concurrent {
                tool_call_ids: vec!["r1".to_string(), "n1".to_string(), "r2".to_string()]
            }
        );
    }

    #[test]
    fn empty_plan() {
        let plan = SchedulingPlan::build(&[]);
        assert!(plan.steps.is_empty());
    }
}
