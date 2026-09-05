//! Pure agent tab identity and navigation state; history is read through source-bound pages.

use uuid::Uuid;

/// State for the multi-agent tab strip.
///
/// `TabState` does not hold the [`norn::agent::registry::AgentRegistry`]
/// — display names live with the registry and the caller threads it
/// separately. Keeping state to bare [`Uuid`]s lets the type stay
/// `Send + Sync` without an `Arc<RwLock<_>>` on every field.
#[derive(Clone, Debug)]
pub struct TabState {
    root_id: Uuid,
    active_agent_id: Uuid,
    background_agents: Vec<Uuid>,
    focused_index: Option<usize>,
}

impl TabState {
    /// Construct with `root_id` as the initial active tab and an empty
    /// background-agent list.
    #[must_use]
    pub const fn new(root_id: Uuid) -> Self {
        Self {
            root_id,
            active_agent_id: root_id,
            background_agents: Vec::new(),
            focused_index: None,
        }
    }

    /// The id originally registered as the root for this session.
    ///
    /// Stored separately from `active_agent_id` so that
    /// [`Self::remove_agent`] can fall back to the root even after the
    /// user switched the active tab to a child.
    #[must_use]
    pub const fn root_id(&self) -> Uuid {
        self.root_id
    }

    /// The currently active agent — the one whose live output streams
    /// into the scroll region.
    #[must_use]
    pub const fn active_agent_id(&self) -> Uuid {
        self.active_agent_id
    }

    /// Background-agent ids in insertion order.
    #[must_use]
    pub fn background_agents(&self) -> &[Uuid] {
        &self.background_agents
    }

    /// The id of the currently focused agent, if any.
    ///
    /// Focus is independent of `active_agent_id`: it is the cursor
    /// position the next [`Self::cycle_focus`] will advance from. The
    /// visual highlight that exposes focus to the user is wired in
    /// NT-011.
    #[must_use]
    pub fn focused_agent_id(&self) -> Option<Uuid> {
        let i = self.focused_index?;
        self.cycle_list().get(i).copied()
    }

    /// Add a child agent to the background-tab set.
    ///
    /// No-op when `id` is already tracked (active or background) — the
    /// brief explicitly forbids duplicates so the cycle list stays
    /// 1:1 with the agent set.
    pub fn add_agent(&mut self, id: Uuid) {
        if id == self.active_agent_id {
            return;
        }
        if self.background_agents.contains(&id) {
            return;
        }
        self.background_agents.push(id);
    }

    /// Remove an agent from the tab set.
    ///
    /// When the removed id matches the active tab, fall back to the
    /// root id when it is still tracked, then to the first background
    /// agent, otherwise leave the active id unchanged (nothing to
    /// switch to). Focus is cleared because the cycle list shrank.
    pub fn remove_agent(&mut self, id: Uuid) {
        self.background_agents.retain(|x| *x != id);
        if id == self.active_agent_id {
            self.fall_back_active(id);
        }
        self.focused_index = None;
    }

    fn fall_back_active(&mut self, removed: Uuid) {
        if removed != self.root_id
            && let Some(pos) = self
                .background_agents
                .iter()
                .position(|x| *x == self.root_id)
        {
            self.background_agents.remove(pos);
            self.active_agent_id = self.root_id;
            return;
        }
        if !self.background_agents.is_empty() {
            self.active_agent_id = self.background_agents.remove(0);
        }
    }

    /// Advance focus to the next agent in the cycle list.
    ///
    /// Cycle order is `[active, ..background_agents]`. First call from
    /// a cleared focus lands on index `0`. Wraps after the last entry.
    /// Single-tracked-agent sessions are a no-op (focus stays cleared).
    pub fn cycle_focus(&mut self) {
        let len = self.cycle_list().len();
        if len <= 1 {
            self.focused_index = None;
            return;
        }
        let next = match self.focused_index {
            None => 0,
            Some(i) => (i + 1) % len,
        };
        self.focused_index = Some(next);
    }

    /// Switch the active tab to `target`.
    ///
    /// Returns the previously active id when a switch happens. Returns
    /// `None` and is a no-op when `target == active_agent_id`. Focus
    /// is cleared — the cycle order changes when the active id moves.
    ///
    /// The empty-input gate (`Enter on focused agent only when the
    /// input buffer is empty`) is the event loop's responsibility
    /// (NT-011); this method runs the state transition unconditionally.
    pub fn switch_to(&mut self, target: Uuid) -> Option<Uuid> {
        if target == self.active_agent_id {
            return None;
        }
        let previous = self.active_agent_id;
        self.background_agents.retain(|x| *x != target);
        self.background_agents.push(previous);
        self.active_agent_id = target;
        self.focused_index = None;
        Some(previous)
    }

    fn cycle_list(&self) -> Vec<Uuid> {
        let mut v = Vec::with_capacity(self.background_agents.len() + 1);
        v.push(self.active_agent_id);
        v.extend(self.background_agents.iter().copied());
        v
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    // ---------------- TabState construction (R1) ----------------

    #[test]
    fn new_marks_root_as_active_and_keeps_background_empty() {
        let root = Uuid::new_v4();
        let tabs = TabState::new(root);
        assert_eq!(tabs.active_agent_id(), root);
        assert_eq!(tabs.root_id(), root);
        assert!(tabs.background_agents().is_empty());
        assert!(tabs.focused_agent_id().is_none());
    }

    // ---------------- add_agent (R1) ----------------

    #[test]
    fn add_agent_appends_to_background_in_insertion_order() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        assert_eq!(tabs.background_agents(), &[a, b]);
    }

    #[test]
    fn add_agent_skips_when_id_matches_active() {
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(root);
        assert!(tabs.background_agents().is_empty());
    }

    #[test]
    fn add_agent_skips_when_id_already_in_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(a);
        assert_eq!(tabs.background_agents(), &[a]);
    }

    #[test]
    fn spawning_a_child_adds_it_to_background_agents() {
        // Brief R1 acceptance test, verbatim.
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        assert!(tabs.background_agents().contains(&child));
    }

    // ---------------- remove_agent (R1) ----------------

    #[test]
    fn remove_agent_drops_from_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        tabs.remove_agent(a);
        assert_eq!(tabs.background_agents(), &[b]);
        assert_eq!(tabs.active_agent_id(), root);
    }

    #[test]
    fn remove_agent_falls_back_to_root_when_active_removed() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        // Promote child to active (root pushed to background).
        tabs.switch_to(child);
        assert_eq!(tabs.active_agent_id(), child);
        tabs.remove_agent(child);
        assert_eq!(
            tabs.active_agent_id(),
            root,
            "removing the active child must promote the known root"
        );
        assert!(
            !tabs.background_agents().contains(&root),
            "root must be removed from background when promoted"
        );
    }

    #[test]
    fn remove_active_root_falls_back_to_first_background() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);
        // Active is root, root_id is root — removing root falls back
        // to the first background entry.
        tabs.remove_agent(root);
        assert_eq!(tabs.active_agent_id(), a);
        assert_eq!(tabs.background_agents(), &[b]);
    }

    #[test]
    fn remove_only_active_with_no_background_leaves_active_unchanged() {
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        // Removing the only known agent — nothing to fall back to.
        tabs.remove_agent(root);
        assert_eq!(tabs.active_agent_id(), root);
        assert!(tabs.background_agents().is_empty());
    }

    // ---------------- cycle_focus (R2) ----------------

    #[test]
    fn cycle_focus_single_agent_is_noop() {
        // Brief R2 acceptance: 'Tab on a single-agent session is a no-op'.
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.cycle_focus();
        assert!(tabs.focused_agent_id().is_none());
    }

    #[test]
    fn cycle_focus_three_agent_tree_visits_all_three() -> Result<(), &'static str> {
        // Brief R2 acceptance: 'Tab on 3-agent tree cycles through all three'.
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);
        tabs.add_agent(b);

        tabs.cycle_focus();
        let first = tabs
            .focused_agent_id()
            .ok_or("first cycle must focus an agent")?;
        tabs.cycle_focus();
        let second = tabs
            .focused_agent_id()
            .ok_or("second cycle must focus an agent")?;
        tabs.cycle_focus();
        let third = tabs
            .focused_agent_id()
            .ok_or("third cycle must focus an agent")?;

        let visited: HashSet<Uuid> = [first, second, third].into_iter().collect();
        assert_eq!(visited, HashSet::from([root, a, b]));
        Ok(())
    }

    #[test]
    fn cycle_focus_wraps_after_last_entry() {
        let root = Uuid::new_v4();
        let a = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(a);

        tabs.cycle_focus();
        let first = tabs.focused_agent_id();
        tabs.cycle_focus();
        let second = tabs.focused_agent_id();
        tabs.cycle_focus();
        let third = tabs.focused_agent_id();
        assert_eq!(first, third, "third press wraps back to first");
        assert_ne!(first, second, "second press differs from first");
    }

    // ---------------- switch_to (R3) ----------------

    #[test]
    fn switch_to_root_to_child_changes_active_agent_id() {
        // Brief R3 acceptance: 'switching from root to child changes
        // active_agent_id'.
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        let previous = tabs.switch_to(child);
        assert_eq!(previous, Some(root));
        assert_eq!(tabs.active_agent_id(), child);
        assert!(
            tabs.background_agents().contains(&root),
            "previously active root must move into the background tabs"
        );
    }

    #[test]
    fn switch_to_currently_active_agent_is_noop() {
        // Brief R3 acceptance: 'Enter on the currently active agent is
        // a no-op'.
        let root = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        let result = tabs.switch_to(root);
        assert_eq!(result, None);
        assert_eq!(tabs.active_agent_id(), root);
    }

    #[test]
    fn switch_to_clears_focus() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let mut tabs = TabState::new(root);
        tabs.add_agent(child);
        tabs.cycle_focus();
        assert!(tabs.focused_agent_id().is_some());
        tabs.switch_to(child);
        assert!(tabs.focused_agent_id().is_none());
    }
}
