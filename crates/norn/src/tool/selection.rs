//! Exact operator tool selection retained independently of discovered tool views.

use std::collections::{BTreeSet, HashSet};

/// Original name predicates, including names that have not been registered yet.
#[derive(Clone, Default)]
pub(super) struct ToolSelectionPolicy {
    /// `None` permits every name; an empty set permits none.
    pub(super) available: Option<HashSet<String>>,
    /// Explicit denies win over availability and child selection.
    pub(super) disallowed: HashSet<String>,
}

impl ToolSelectionPolicy {
    pub(super) fn allows(&self, name: &str) -> bool {
        !self.disallowed.contains(name)
            && self
                .available
                .as_ref()
                .is_none_or(|available| available.contains(name))
    }

    /// Intersect an explicit child restriction without broadening this policy.
    pub(super) fn narrowed(&self, available: Option<&BTreeSet<String>>) -> Self {
        let Some(available) = available else {
            return self.clone();
        };
        Self {
            available: Some(
                available
                    .iter()
                    .filter(|name| self.allows(name))
                    .cloned()
                    .collect(),
            ),
            disallowed: self.disallowed.clone(),
        }
    }
}
