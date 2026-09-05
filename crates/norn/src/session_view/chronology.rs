//! Observed event-gap order and exact pending completion identities; no rows or body ownership.

use std::collections::{HashMap, HashSet};

use super::contract::{AttemptKey, ItemId};
use super::error::ViewError;

/// Local observations in gap N precede the next committed event N. This is
/// source-bound observed order, not wall-clock order of unseen appends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Order {
    boundary: usize,
    position: Position,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Position {
    Local(u64),
    Event(usize),
}

impl Order {
    pub const fn committed(ordinal: usize, part: usize) -> Self {
        Self {
            boundary: ordinal,
            position: Position::Event(part),
        }
    }

    pub const fn committed_ordinal(self) -> Option<usize> {
        match self.position {
            Position::Event(_) => Some(self.boundary),
            Position::Local(_) => None,
        }
    }

    pub fn after_event(self, ordinal: usize) -> Result<Self, ViewError> {
        if !matches!(self.position, Position::Local(_)) {
            return Err(ViewError::AttemptMismatch);
        }
        Ok(Self {
            boundary: after(ordinal)?,
            ..self
        })
    }
}

fn after(ordinal: usize) -> Result<usize, ViewError> {
    ordinal.checked_add(1).ok_or(ViewError::CounterExhausted {
        counter: "observed event boundary",
    })
}

/// Monotonic observed boundary plus only unresolved Done-to-attempt bindings.
pub(super) struct Chronology {
    boundary: usize,
    next_local: u64,
    completions: HashMap<AttemptKey, HashSet<ItemId>>,
    owners: HashMap<ItemId, AttemptKey>,
}

impl Chronology {
    pub fn new() -> Self {
        Self {
            boundary: 0,
            next_local: 0,
            completions: HashMap::new(),
            owners: HashMap::new(),
        }
    }

    pub fn observe(&mut self, ordinal: usize) -> Result<(), ViewError> {
        self.boundary = self.boundary.max(after(ordinal)?);
        Ok(())
    }

    pub fn next_order(&mut self) -> Result<Order, ViewError> {
        let order = Order {
            boundary: self.boundary,
            position: Position::Local(self.next_local),
        };
        self.next_local = self
            .next_local
            .checked_add(1)
            .ok_or(ViewError::CounterExhausted {
                counter: "local item order",
            })?;
        Ok(order)
    }

    pub fn owner(&self, item: &ItemId) -> Option<&AttemptKey> {
        self.owners.get(item)
    }

    pub fn bind(&mut self, item: &ItemId, attempt: &AttemptKey) -> Result<(), ViewError> {
        if self.owners.get(item).is_some_and(|owner| owner != attempt) {
            return Err(ViewError::AttemptMismatch);
        }
        self.owners.insert(item.clone(), attempt.clone());
        self.completions
            .entry(attempt.clone())
            .or_default()
            .insert(item.clone());
        Ok(())
    }

    pub fn unbind(&mut self, item: &ItemId) {
        if let Some(attempt) = self.owners.remove(item)
            && let Some(items) = self.completions.get_mut(&attempt)
        {
            items.remove(item);
            if items.is_empty() {
                self.completions.remove(&attempt);
            }
        }
    }

    pub fn take(&mut self, attempt: &AttemptKey) -> HashSet<ItemId> {
        let items = self.completions.remove(attempt).unwrap_or_default();
        for item in &items {
            self.owners.remove(item);
        }
        items
    }
}
