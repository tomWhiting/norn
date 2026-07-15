//! Lexically scoped local analyzer state with conservative cross-scope joins.

use std::collections::BTreeMap;

use tree_sitter::Node;

#[derive(Clone, Debug)]
struct Possibilities<T> {
    values: Vec<T>,
    absent: bool,
}

impl<T: Clone + Eq> Possibilities<T> {
    fn from_value(value: Option<T>) -> Self {
        match value {
            Some(value) => Self {
                values: vec![value],
                absent: false,
            },
            None => Self {
                values: Vec::new(),
                absent: true,
            },
        }
    }

    fn join(&mut self, value: Option<T>) {
        if let Some(value) = value {
            if !self.values.contains(&value) {
                self.values.push(value);
            }
        } else {
            self.absent = true;
        }
    }

    fn local_lookup(&self) -> LocalLookup<T> {
        match (self.values.as_slice(), self.absent) {
            ([], _) => LocalLookup::Shadowed,
            ([value], false) => LocalLookup::Exact(value.clone()),
            _ => LocalLookup::Ambiguous,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum LocalLookup<T> {
    Unbound,
    Shadowed,
    Exact(T),
    Ambiguous,
}

#[derive(Clone, Debug)]
pub(super) struct ScopedBindings<T> {
    frames: BTreeMap<(usize, usize), BTreeMap<String, Possibilities<T>>>,
}

impl<T: Clone + Eq> ScopedBindings<T> {
    pub(super) fn new() -> Self {
        Self {
            frames: BTreeMap::new(),
        }
    }

    pub(super) fn declare(&mut self, node: Node<'_>, name: String, value: Option<T>) {
        let Some(scope) = nearest_scope(node) else {
            return;
        };
        self.frames
            .entry(scope)
            .or_default()
            .insert(name, Possibilities::from_value(value));
    }

    pub(super) fn assign(&mut self, node: Node<'_>, name: String, value: Option<T>) {
        let scopes = scope_chain(node);
        let Some(nearest) = scopes.first().copied() else {
            return;
        };
        let target = scopes.iter().copied().find(|scope| {
            self.frames
                .get(scope)
                .is_some_and(|frame| frame.contains_key(&name))
        });
        let Some(target) = target else {
            self.frames
                .entry(nearest)
                .or_default()
                .insert(name, Possibilities::from_value(value));
            return;
        };
        let frame = self.frames.entry(target).or_default();
        if target == nearest {
            frame.insert(name, Possibilities::from_value(value));
        } else if let Some(possibilities) = frame.get_mut(&name) {
            possibilities.join(value);
        }
    }

    pub(super) fn local_lookup(&self, node: Node<'_>, name: &str) -> LocalLookup<T> {
        self.possibilities(node, name)
            .map_or(LocalLookup::Unbound, Possibilities::local_lookup)
    }

    pub(super) fn values(&self, node: Node<'_>, name: &str) -> Option<(Vec<T>, bool)> {
        self.possibilities(node, name)
            .map(|values| (values.values.clone(), values.absent))
    }

    pub(super) fn contains_value(&self, node: Node<'_>, name: &str) -> bool {
        self.possibilities(node, name)
            .is_some_and(|values| !values.values.is_empty())
    }

    fn possibilities(&self, node: Node<'_>, name: &str) -> Option<&Possibilities<T>> {
        scope_chain(node)
            .into_iter()
            .find_map(|scope| self.frames.get(&scope).and_then(|frame| frame.get(name)))
    }
}

fn nearest_scope(node: Node<'_>) -> Option<(usize, usize)> {
    scope_chain(node).into_iter().next()
}

fn scope_chain(node: Node<'_>) -> Vec<(usize, usize)> {
    let mut scopes = Vec::new();
    let mut current = Some(node);
    while let Some(candidate) = current {
        if is_scope(candidate) {
            scopes.push((candidate.start_byte(), candidate.end_byte()));
        }
        current = candidate.parent();
    }
    scopes
}

pub(super) fn is_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "source_file" | "declaration_list" | "block" | "match_arm" | "closure_expression"
    )
}
