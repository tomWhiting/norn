//! Generation-bound registered subtree discovery under one recovered index lock.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use super::{SessionIndexEntry, SessionPersistError};

pub(crate) fn registered_subtree(
    data_dir: &Path,
    registered: &SessionIndexEntry,
    deadline: Option<Duration>,
) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let lock = super::lock_recovered_index(data_dir, deadline)?;
    let entries = super::codec::read_index_in(lock.root())?;
    let root = super::registered_position(&entries, registered)?;
    subtree(&entries, root)
}

fn subtree(
    entries: &[SessionIndexEntry],
    root: usize,
) -> Result<Vec<SessionIndexEntry>, SessionPersistError> {
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    for (position, entry) in entries.iter().enumerate() {
        if let Some(parent) = entry.parent_id.as_deref() {
            children.entry(parent).or_default().push(position);
        }
    }
    let mut pending = vec![root];
    let mut visited = HashSet::new();
    let mut result = Vec::new();
    while let Some(position) = pending.pop() {
        let entry = &entries[position];
        if !visited.insert(position) {
            return Err(SessionPersistError::DescendantCycle {
                root: entries[root].id.clone(),
                id: entry.id.clone(),
            });
        }
        result.push(entry.clone());
        if let Some(descendants) = children.get(entry.id.as_str()) {
            pending.extend(descendants.iter().rev().copied());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preorder_preserves_siblings_and_refuses_reachable_cycles()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let manager = crate::session::SessionManager::new(temp.path());
        let opened = manager.create(
            crate::session::CreateSessionOptions {
                model: "fixture".into(),
                working_dir: "/fixture".into(),
                name: None,
            },
            crate::session::store::DurabilityPolicy::Flush,
        )?;
        let mut entries = Vec::new();
        for (id, parent) in [
            ("root", None),
            ("first", Some("root")),
            ("second", Some("root")),
            ("grandchild", Some("first")),
            ("foreign", None),
        ] {
            let mut entry = opened.entry.clone();
            entry.id = id.into();
            entry.parent_id = parent.map(str::to_owned);
            entries.push(entry);
        }
        let ids = |rows: Vec<SessionIndexEntry>| rows.into_iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(
            ids(subtree(&entries, 0)?),
            ["root", "first", "grandchild", "second"]
        );
        assert_eq!(ids(subtree(&entries, 1)?), ["first", "grandchild"]);
        entries[0].parent_id = Some("grandchild".into());
        assert!(
            matches!(subtree(&entries, 0), Err(SessionPersistError::DescendantCycle { root, id }) if root == "root" && id == "root")
        );
        // A corrupt unrelated component cannot confer access or prevent this leaf's listing.
        assert_eq!(ids(subtree(&entries, 2)?), ["second"]);
        Ok(())
    }
}
