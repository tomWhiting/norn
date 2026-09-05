//! Single-owned ordered rows with direct item, attempt and tool-call indexes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound;

use super::chronology::{Chronology, Order};
use super::contract::{
    AttemptKey, HistoryCursor, HistoryPosition, ItemDirection, ItemId, ItemInclusion, ViewItem,
    ViewItemKind,
};
use super::error::ViewError;
use crate::session::events::EventId;

/// A borrowed range; direction changes which end advances, without collecting rows.
pub(super) struct ItemTraversal<'a> {
    range: std::collections::btree_map::Range<'a, Order, ViewItem>,
    direction: ItemDirection,
    #[cfg(test)]
    visited: &'a std::cell::Cell<usize>,
}

impl<'a> Iterator for ItemTraversal<'a> {
    type Item = &'a ViewItem;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = match self.direction {
            ItemDirection::Earlier => self.range.next_back(),
            ItemDirection::Later => self.range.next(),
        };
        #[cfg(test)]
        if entry.is_some() {
            self.visited.set(self.visited.get() + 1);
        }
        entry.map(|(_, row)| row)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl DoubleEndedIterator for ItemTraversal<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let entry = match self.direction {
            ItemDirection::Earlier => self.range.next(),
            ItemDirection::Later => self.range.next_back(),
        };
        #[cfg(test)]
        if entry.is_some() {
            self.visited.set(self.visited.get() + 1);
        }
        entry.map(|(_, row)| row)
    }
}

/// Index values are identities only. The ordered map owns each row exactly once.
pub(super) struct ItemIndex {
    rows: BTreeMap<Order, ViewItem>,
    ids: HashMap<ItemId, Order>,
    live_calls: HashMap<AttemptKey, HashMap<String, BTreeSet<Order>>>,
    pending_calls: HashMap<uuid::Uuid, HashMap<String, BTreeSet<Order>>>,
    invocations: HashMap<EventId, HashMap<String, BTreeSet<Order>>>,
    preceding_calls: HashMap<String, BTreeSet<Order>>,
    pending_parents: HashMap<EventId, BTreeSet<Order>>,
    pending_results: BTreeSet<Order>,
    attempts: HashMap<AttemptKey, BTreeSet<Order>>,
    executions: HashMap<uuid::Uuid, BTreeSet<Order>>,
    chronology: Chronology,
    #[cfg(test)]
    pub(super) completion_relocations: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) tool_lookup_visits: std::cell::Cell<usize>,
    #[cfg(test)]
    pub(super) traversal_visits: std::cell::Cell<usize>,
}

impl ItemIndex {
    pub fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
            ids: HashMap::new(),
            live_calls: HashMap::new(),
            pending_calls: HashMap::new(),
            invocations: HashMap::new(),
            preceding_calls: HashMap::new(),
            pending_parents: HashMap::new(),
            pending_results: BTreeSet::new(),
            attempts: HashMap::new(),
            executions: HashMap::new(),
            chronology: Chronology::new(),
            #[cfg(test)]
            completion_relocations: std::cell::Cell::new(0),
            #[cfg(test)]
            tool_lookup_visits: std::cell::Cell::new(0),
            #[cfg(test)]
            traversal_visits: std::cell::Cell::new(0),
        }
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ViewItem> + ExactSizeIterator {
        self.rows.values()
    }

    pub fn get(&self, id: &ItemId) -> Option<&ViewItem> {
        self.ids.get(id).and_then(|order| self.rows.get(order))
    }

    pub fn get_mut(&mut self, id: &ItemId) -> Option<&mut ViewItem> {
        self.ids.get(id).and_then(|order| self.rows.get_mut(order))
    }

    pub fn items_from(
        &self,
        anchor: &ItemId,
        direction: ItemDirection,
        inclusion: ItemInclusion,
    ) -> Result<ItemTraversal<'_>, ViewError> {
        let order = self
            .ids
            .get(anchor)
            .ok_or_else(|| ViewError::ItemUnavailable {
                item: Box::new(anchor.clone()),
            })?;
        let bound = match inclusion {
            ItemInclusion::Inclusive => Bound::Included(*order),
            ItemInclusion::Exclusive => Bound::Excluded(*order),
        };
        let bounds = match direction {
            ItemDirection::Earlier => (Bound::Unbounded, bound),
            ItemDirection::Later => (bound, Bound::Unbounded),
        };
        Ok(ItemTraversal {
            range: self.rows.range(bounds),
            direction,
            #[cfg(test)]
            visited: &self.traversal_visits,
        })
    }

    /// Only newly admitted results, results awaiting this exact parent, and
    /// pending results whose predecessor coverage just became complete.
    pub fn pending_result_ids(
        &self,
        event_id: &EventId,
        start: usize,
        end: usize,
        admitted: &[ViewItem],
    ) -> Vec<ItemId> {
        let mut orders = BTreeSet::new();
        for row in admitted {
            if let Some(order) = self.ids.get(&row.id)
                && self.pending_results.contains(order)
            {
                orders.insert(*order);
            }
        }
        for order in self.pending_parents.get(event_id).into_iter().flatten() {
            #[cfg(test)]
            self.inspect_tool_order();
            orders.insert(*order);
        }
        for order in self
            .pending_results
            .range(Order::committed(start, 0)..Order::committed(end, 0))
        {
            #[cfg(test)]
            self.inspect_tool_order();
            orders.insert(*order);
        }
        self.indexed_tool_ids(orders.iter())
    }

    /// At most two rows are needed to distinguish an exact unique invocation
    /// from an ambiguous duplicate call within that one invocation event.
    pub fn invocation_ids(&self, event_id: &EventId, call: &str) -> Vec<ItemId> {
        self.indexed_tool_ids(
            self.invocations
                .get(event_id)
                .and_then(|calls| calls.get(call))
                .into_iter()
                .flatten()
                .take(2),
        )
    }

    /// Ambiguous call-only evidence cannot become unique by ignoring already
    /// completed invocations. Inspect the first two preceding invocations only.
    pub fn preceding_invocation_ids(&self, call: &str, result: &ItemId) -> Vec<ItemId> {
        let Some(ordinal) = self
            .ids
            .get(result)
            .and_then(|order| order.committed_ordinal())
        else {
            return Vec::new();
        };
        self.indexed_tool_ids(
            self.preceding_calls
                .get(call)
                .into_iter()
                .flat_map(|orders| orders.range(..Order::committed(ordinal, 0)))
                .take(2),
        )
    }

    fn indexed_tool_ids<'a>(&self, orders: impl Iterator<Item = &'a Order>) -> Vec<ItemId> {
        orders
            .filter_map(|order| {
                #[cfg(test)]
                self.inspect_tool_order();
                self.rows.get(order).map(|row| row.id.clone())
            })
            .collect()
    }

    #[cfg(test)]
    fn inspect_tool_order(&self) {
        self.tool_lookup_visits
            .set(self.tool_lookup_visits.get() + 1);
    }

    /// Find the first supplied alias in this exact provisional response attempt.
    /// Prior responses and committed history never participate in this lookup.
    pub fn live_call_id(&self, attempt: &AttemptKey, call: &str) -> Option<ItemId> {
        self.indexed_tool_ids(
            self.live_calls
                .get(attempt)
                .and_then(|calls| calls.get(call))
                .into_iter()
                .flatten()
                .take(1),
        )
        .into_iter()
        .next()
    }

    /// Only invocations in the receiving execution that still lack a result.
    /// Two entries establish ambiguity without traversing further history.
    pub fn pending_call_ids(&self, execution: uuid::Uuid, call: &str) -> Vec<ItemId> {
        self.indexed_tool_ids(
            self.pending_calls
                .get(&execution)
                .and_then(|calls| calls.get(call))
                .into_iter()
                .flatten()
                .take(2),
        )
    }

    pub fn attempt_ids(&self, attempt: &AttemptKey) -> Vec<ItemId> {
        self.attempts
            .get(attempt)
            .into_iter()
            .flatten()
            .filter_map(|order| self.rows.get(order).map(|row| row.id.clone()))
            .collect()
    }

    pub fn execution_ids(&self, execution: uuid::Uuid) -> Vec<ItemId> {
        self.executions
            .get(&execution)
            .into_iter()
            .flatten()
            .filter_map(|order| self.rows.get(order).map(|row| row.id.clone()))
            .collect()
    }

    pub fn observe_committed(&mut self, ordinal: usize) -> Result<(), ViewError> {
        self.chronology.observe(ordinal)
    }

    pub fn bind_completion(
        &mut self,
        item: &ItemId,
        attempt: &AttemptKey,
    ) -> Result<(), ViewError> {
        if !matches!(item, ItemId::Local { .. }) || !self.ids.contains_key(item) {
            return Err(ViewError::AttemptMismatch);
        }
        self.chronology.bind(item, attempt)
    }

    pub fn forget_completions(&mut self, attempt: &AttemptKey) {
        drop(self.chronology.take(attempt));
    }

    /// Move only Done rows whose exact producer supplied this accepted cursor.
    /// IDs and bodies stay unchanged; remove/insert refresh every order-keyed index.
    pub fn place_completions_after(
        &mut self,
        attempt: &AttemptKey,
        cursor: &HistoryCursor,
    ) -> Result<(), ViewError> {
        let HistoryPosition::Event { ordinal, .. } = cursor.position() else {
            return Err(ViewError::AttemptMismatch);
        };
        for item in self.chronology.take(attempt) {
            #[cfg(test)]
            self.completion_relocations
                .set(self.completion_relocations.get() + 1);
            let order = self
                .ids
                .get(&item)
                .ok_or(ViewError::AttemptMismatch)?
                .after_event(*ordinal)?;
            let row = self.remove(&item).ok_or(ViewError::AttemptMismatch)?;
            self.insert_at(order, row);
        }
        Ok(())
    }

    pub fn insert(&mut self, row: ViewItem) -> Result<(), ViewError> {
        let order = if let Some(order) = self.ids.get(&row.id) {
            *order
        } else {
            match &row.id {
                ItemId::Committed { cursor, part } => match cursor.position() {
                    HistoryPosition::Event { ordinal, .. } => Order::committed(*ordinal, *part),
                    HistoryPosition::Empty => return Err(ViewError::AttemptMismatch),
                },
                ItemId::Provisional(_) | ItemId::Local { .. } => self.chronology.next_order()?,
            }
        };
        let completion = self.chronology.owner(&row.id).cloned();
        self.remove(&row.id);
        if let Some(attempt) = completion {
            self.chronology.bind(&row.id, &attempt)?;
        }
        self.insert_at(order, row);
        Ok(())
    }

    pub fn replace(&mut self, previous: &ItemId, row: ViewItem) -> Result<(), ViewError> {
        if &row.id != previous && self.ids.contains_key(&row.id) {
            return Err(ViewError::AttemptMismatch);
        }
        if let Some(order) = self.ids.get(previous).copied() {
            let completion = self.chronology.owner(previous).cloned();
            self.remove(previous);
            if let Some(attempt) = completion {
                self.chronology.bind(&row.id, &attempt)?;
            }
            self.insert_at(order, row);
            Ok(())
        } else {
            self.insert(row)
        }
    }

    pub fn remove(&mut self, id: &ItemId) -> Option<ViewItem> {
        let order = self.ids.remove(id)?;
        let row = self.rows.remove(&order)?;
        self.chronology.unbind(id);
        self.remove_committed_tool(order, &row);
        self.remove_live_tool(order, &row);
        if let Some(attempt) = attempt_of(&row)
            && let Some(orders) = self.attempts.get_mut(attempt)
        {
            orders.remove(&order);
            if orders.is_empty() {
                self.attempts.remove(attempt);
            }
            if let Some(orders) = self.executions.get_mut(&attempt.execution) {
                orders.remove(&order);
                if orders.is_empty() {
                    self.executions.remove(&attempt.execution);
                }
            }
        }
        Some(row)
    }

    fn insert_at(&mut self, order: Order, row: ViewItem) {
        self.insert_committed_tool(order, &row);
        self.insert_live_tool(order, &row);
        if let Some(attempt) = attempt_of(&row) {
            self.attempts
                .entry(attempt.clone())
                .or_default()
                .insert(order);
            self.executions
                .entry(attempt.execution)
                .or_default()
                .insert(order);
        }
        self.ids.insert(row.id.clone(), order);
        self.rows.insert(order, row);
    }

    fn insert_committed_tool(&mut self, order: Order, row: &ViewItem) {
        let ViewItemKind::Tool(tool) = &row.kind else {
            return;
        };
        if let (Some(event_id), Some(call)) = (&tool.invocation_event, &tool.call_id)
            && tool.arguments.is_some()
        {
            self.invocations
                .entry(event_id.clone())
                .or_default()
                .entry(call.clone())
                .or_default()
                .insert(order);
            self.preceding_calls
                .entry(call.clone())
                .or_default()
                .insert(order);
        }
        if tool.arguments.is_none() && tool.result_event.is_some() {
            self.pending_results.insert(order);
            if let Some(parent) = &tool.result_parent {
                self.pending_parents
                    .entry(parent.clone())
                    .or_default()
                    .insert(order);
            }
        }
    }

    fn remove_committed_tool(&mut self, order: Order, row: &ViewItem) {
        let ViewItemKind::Tool(tool) = &row.kind else {
            return;
        };
        if let (Some(event_id), Some(call)) = (&tool.invocation_event, &tool.call_id) {
            if let Some(calls) = self.invocations.get_mut(event_id) {
                remove_order(calls, call, order);
                if calls.is_empty() {
                    self.invocations.remove(event_id);
                }
            }
            remove_order(&mut self.preceding_calls, call, order);
        }
        self.pending_results.remove(&order);
        if let Some(parent) = &tool.result_parent {
            remove_order(&mut self.pending_parents, parent, order);
        }
    }

    fn insert_live_tool(&mut self, order: Order, row: &ViewItem) {
        let ViewItemKind::Tool(tool) = &row.kind else {
            return;
        };
        let Some(call) = &tool.call_id else {
            return;
        };
        if let ItemId::Provisional(key) = &row.id {
            self.live_calls
                .entry(key.attempt.clone())
                .or_default()
                .entry(call.clone())
                .or_default()
                .insert(order);
        }
        if let Some(attempt) = &tool.invocation_attempt
            && tool.arguments.is_some()
            && tool.result.is_none()
        {
            self.pending_calls
                .entry(attempt.execution)
                .or_default()
                .entry(call.clone())
                .or_default()
                .insert(order);
        }
    }

    fn remove_live_tool(&mut self, order: Order, row: &ViewItem) {
        let ViewItemKind::Tool(tool) = &row.kind else {
            return;
        };
        let Some(call) = &tool.call_id else {
            return;
        };
        if let ItemId::Provisional(key) = &row.id {
            remove_call_order(&mut self.live_calls, &key.attempt, call, order);
        }
        if let Some(attempt) = &tool.invocation_attempt {
            remove_call_order(&mut self.pending_calls, &attempt.execution, call, order);
        }
    }
}

fn remove_call_order<K: Eq + std::hash::Hash>(
    index: &mut HashMap<K, HashMap<String, BTreeSet<Order>>>,
    key: &K,
    call: &str,
    order: Order,
) {
    if let Some(calls) = index.get_mut(key) {
        remove_order(calls, call, order);
        if calls.is_empty() {
            index.remove(key);
        }
    }
}

fn remove_order<K, Q>(index: &mut HashMap<K, BTreeSet<Order>>, key: &Q, order: Order)
where
    K: Eq + std::hash::Hash + std::borrow::Borrow<Q>,
    Q: Eq + std::hash::Hash + ?Sized,
{
    if let Some(orders) = index.get_mut(key) {
        orders.remove(&order);
        if orders.is_empty() {
            index.remove(key);
        }
    }
}

fn attempt_of(row: &ViewItem) -> Option<&AttemptKey> {
    match &row.id {
        ItemId::Provisional(key) => Some(&key.attempt),
        ItemId::Committed { .. } | ItemId::Local { .. } => match &row.kind {
            ViewItemKind::Tool(tool) => tool.invocation_attempt.as_ref(),
            _ => None,
        },
    }
}
