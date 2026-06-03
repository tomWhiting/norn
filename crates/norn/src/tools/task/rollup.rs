//! Parent-task status roll-up.
//!
//! A parent task's effective status is derived from its children's statuses
//! with the priority ordering `Failed > Blocked > InProgress > Pending >
//! Completed`. A parent with a mix of `Pending` and `Completed` children is
//! treated as `InProgress` because work has started but is not finished.

use super::types::TaskStatus;

/// Compute the effective status of a parent task from its children.
///
/// When `children_statuses` is empty the parent's `own` status is returned
/// unchanged — a task with no children owns its status outright.
///
/// Otherwise the highest-priority status present wins:
/// `Failed > Blocked > InProgress`. With none of those present, a mix of
/// `Pending` and `Completed` children rolls up to `InProgress`; all-`Pending`
/// stays `Pending`, and all-`Completed` rolls up to `Completed`.
#[must_use]
pub fn effective_status(children_statuses: &[TaskStatus], own: TaskStatus) -> TaskStatus {
    if children_statuses.is_empty() {
        return own;
    }

    let mut has_failed = false;
    let mut has_blocked = false;
    let mut has_in_progress = false;
    let mut pending_count = 0_usize;
    let mut completed_count = 0_usize;

    for status in children_statuses {
        match status {
            TaskStatus::Failed => has_failed = true,
            TaskStatus::Blocked => has_blocked = true,
            TaskStatus::InProgress => has_in_progress = true,
            TaskStatus::Pending => pending_count += 1,
            TaskStatus::Completed => completed_count += 1,
        }
    }

    if has_failed {
        return TaskStatus::Failed;
    }
    if has_blocked {
        return TaskStatus::Blocked;
    }
    if has_in_progress {
        return TaskStatus::InProgress;
    }
    // Only Pending and/or Completed children remain.
    if pending_count > 0 && completed_count > 0 {
        return TaskStatus::InProgress;
    }
    if completed_count > 0 {
        return TaskStatus::Completed;
    }
    TaskStatus::Pending
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_children_returns_own_status() {
        assert_eq!(
            effective_status(&[], TaskStatus::Blocked),
            TaskStatus::Blocked
        );
        assert_eq!(
            effective_status(&[], TaskStatus::Completed),
            TaskStatus::Completed
        );
    }

    #[test]
    fn any_failed_wins() {
        let children = [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::InProgress,
            TaskStatus::Blocked,
        ];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::Failed
        );
    }

    #[test]
    fn any_blocked_wins_when_no_failed() {
        let children = [
            TaskStatus::Completed,
            TaskStatus::Blocked,
            TaskStatus::InProgress,
        ];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::Blocked
        );
    }

    #[test]
    fn any_in_progress_wins_when_no_failed_or_blocked() {
        let children = [TaskStatus::Completed, TaskStatus::InProgress];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::InProgress
        );
    }

    #[test]
    fn pending_and_completed_mix_rolls_up_to_in_progress() {
        let children = [TaskStatus::Pending, TaskStatus::Completed];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::InProgress
        );
    }

    #[test]
    fn all_pending_rolls_up_to_pending() {
        let children = [TaskStatus::Pending, TaskStatus::Pending];
        assert_eq!(
            effective_status(&children, TaskStatus::Completed),
            TaskStatus::Pending
        );
    }

    #[test]
    fn all_completed_rolls_up_to_completed() {
        let children = [TaskStatus::Completed, TaskStatus::Completed];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::Completed
        );
    }

    #[test]
    fn mixed_blocked_and_in_progress_yields_blocked() {
        let children = [TaskStatus::Blocked, TaskStatus::InProgress];
        assert_eq!(
            effective_status(&children, TaskStatus::Pending),
            TaskStatus::Blocked
        );
    }
}
