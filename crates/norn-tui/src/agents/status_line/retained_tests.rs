//! Pure retained tree facts, display deadlines and existing terminal-recovery ownership.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
use norn::agent::registry::AgentRegistry;
use parking_lot::RwLock;
use uuid::Uuid;

use super::{AgentActivity, AgentStatusPanel, HOLD_DURATION, RetainedAgentRowKind};
use crate::render::retained_text::TextAttribute;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn confirm(
    registry: &Arc<RwLock<AgentRegistry>>,
    path: &str,
    parent: Option<Uuid>,
    depth: u32,
) -> TestResult<Uuid> {
    let reservation = AgentRegistry::reserve(
        registry,
        path.to_owned(),
        "worker".to_owned(),
        "test-model".to_owned(),
        parent,
        ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: depth,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
            loop_config: None,
        },
        None,
    )?;
    let id = reservation.id();
    reservation.confirm()?;
    Ok(id)
}

#[test]
fn root_only_has_no_status_rows_or_age_repaint() -> TestResult {
    let registry = AgentRegistry::shared();
    confirm(&registry, "/root", None, 3)?;
    let mut panel = AgentStatusPanel::new(registry);
    let snapshot = panel.retained_snapshot(Instant::now(), Utc::now());
    assert!(snapshot.rows.is_empty());
    assert!(snapshot.next_refresh.is_none());
    assert_eq!(snapshot.all_rows.len(), 1);
    assert!(snapshot.pane_next_refresh.is_some());
    Ok(())
}

#[test]
fn typed_rows_keep_genealogy_fork_identity_palette_activity_and_tokens() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm(&registry, "/root", None, 3)?;
    let child = confirm(
        &registry,
        "/root/fork/12345678-1234-1234-1234-123456789012",
        Some(root),
        2,
    )?;
    let grandchild = confirm(&registry, "/root/independent-path", Some(child), 1)?;
    let mut panel = AgentStatusPanel::new(registry);
    panel.set_activity(child, AgentActivity::Running("checking".to_owned()));
    panel.set_tokens(child, 1_200, 200);
    panel.set_activity(grandchild, AgentActivity::Idle);
    let snapshot = panel.retained_snapshot(Instant::now(), Utc::now());
    let identities: Vec<_> = snapshot.rows.iter().map(|row| row.kind.clone()).collect();
    assert_eq!(
        identities,
        vec![
            RetainedAgentRowKind::Agent {
                id: root,
                parent_id: None
            },
            RetainedAgentRowKind::Agent {
                id: child,
                parent_id: Some(root)
            },
            RetainedAgentRowKind::Agent {
                id: grandchild,
                parent_id: Some(child)
            },
        ]
    );
    let row = snapshot.rows.get(1).ok_or("missing fork row")?;
    assert!(row.text.starts_with("╰─ ● fork/12345678  checking  1k  "));
    assert_eq!(row.style.foreground, Some([95, 215, 95]));
    let leaf = snapshot.rows.get(2).ok_or("missing grandchild row")?;
    assert!(leaf.text.starts_with("  ╰─ ◌ independent-path  idle"));
    assert!(leaf.style.attributes.contains(TextAttribute::Dim));
    assert_eq!(leaf.style.foreground, None);
    assert!(snapshot.next_refresh.is_some());
    Ok(())
}

#[test]
fn five_row_collapse_keeps_exact_overflow_count() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm(&registry, "/root", None, 2)?;
    for index in 0..8 {
        confirm(&registry, &format!("/root/child-{index}"), Some(root), 1)?;
    }
    let mut panel = AgentStatusPanel::new(registry);
    let snapshot = panel.retained_snapshot(Instant::now(), Utc::now());
    assert_eq!(snapshot.rows.len(), 6);
    assert_eq!(snapshot.all_rows.len(), 9);
    assert!(
        snapshot
            .all_rows
            .iter()
            .all(|row| matches!(row.kind, RetainedAgentRowKind::Agent { .. }))
    );
    assert_eq!(
        snapshot.rows.last().ok_or("missing overflow")?.kind,
        RetainedAgentRowKind::Overflow { count: 4 }
    );
    assert_eq!(
        snapshot.rows.last().ok_or("missing overflow")?.text,
        "⋯ 4 more active agents"
    );
    Ok(())
}

#[test]
fn failed_row_hold_and_pending_recovery_use_the_existing_owner() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm(&registry, "/root", None, 2)?;
    let child = confirm(&registry, "/root/child", Some(root), 1)?;
    let mut panel = AgentStatusPanel::new(Arc::clone(&registry));
    let now = Instant::now();
    let wall = Utc::now();
    panel.retained_snapshot(now, wall);
    registry.write().mark_failed(child)?;
    let failed = panel.retained_snapshot(now, wall);
    assert_eq!(
        failed
            .rows
            .get(1)
            .ok_or("missing failed row")?
            .style
            .foreground,
        Some([215, 95, 95])
    );
    panel.set_terminal_recovery_probe(Arc::new(move |id| id == child));
    let expiry = now + HOLD_DURATION;
    let held = panel.retained_snapshot(expiry, wall + chrono::Duration::seconds(3));
    assert!(
        held.rows
            .iter()
            .any(|row| matches!(row.kind, RetainedAgentRowKind::Agent { id, .. } if id == child))
    );
    assert!(registry.read().get(child).is_some());
    panel.set_pending_messages(None);
    assert!(panel.retained_snapshot(expiry, wall).rows.is_empty());
    assert!(registry.read().get(child).is_none());
    assert!(registry.read().tombstone(child).is_some());
    Ok(())
}

#[test]
fn elapsed_refresh_uses_actual_fractional_second_and_minute_boundaries() -> TestResult {
    let now = Instant::now();
    let start =
        chrono::DateTime::parse_from_rfc3339("2026-09-06T00:00:00.250Z")?.with_timezone(&Utc);
    assert_eq!(
        super::retained::next_age_boundary(now, start + chrono::Duration::milliseconds(250), start),
        now + Duration::from_millis(750)
    );
    assert_eq!(
        super::retained::next_age_boundary(
            now,
            start + chrono::Duration::milliseconds(3_605_500),
            start
        ),
        now + Duration::from_millis(54_500)
    );
    Ok(())
}

#[test]
fn next_snapshot_observes_activity_without_root_history_or_provider_text() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm(&registry, "/root", None, 2)?;
    let child = confirm(&registry, "/root/child", Some(root), 1)?;
    let mut panel = AgentStatusPanel::new(registry);
    let now = Instant::now();
    let wall = Utc::now();
    let before = panel.retained_snapshot(now, wall);
    panel.set_activity(child, AgentActivity::Running("checking files".to_owned()));
    let after = panel.retained_snapshot(now, wall);
    assert_eq!(before.rows.first(), after.rows.first());
    assert_ne!(before.rows.get(1), after.rows.get(1));
    assert!(
        after
            .rows
            .get(1)
            .ok_or("missing child status")?
            .text
            .contains("checking files")
    );
    Ok(())
}

#[test]
fn retained_tree_uses_tombstone_parent_authority_after_ancestor_reclamation() -> TestResult {
    let registry = AgentRegistry::shared();
    let root = confirm(&registry, "/root", None, 3)?;
    let parent = confirm(&registry, "/root/mid", Some(root), 2)?;
    let child = confirm(&registry, "/root/leaf", Some(parent), 1)?;
    registry.write().mark_completed(parent)?;
    assert!(registry.write().remove_terminal(parent));
    let mut panel = AgentStatusPanel::new(registry);
    let snapshot = panel.retained_snapshot(Instant::now(), Utc::now());
    assert_eq!(snapshot.rows.len(), 2);
    let leaf = snapshot.rows.get(1).ok_or("missing leaf")?;
    assert_eq!(
        leaf.kind,
        RetainedAgentRowKind::Agent {
            id: child,
            parent_id: Some(parent)
        }
    );
    assert!(leaf.text.starts_with("╰─ ● leaf"));
    Ok(())
}
