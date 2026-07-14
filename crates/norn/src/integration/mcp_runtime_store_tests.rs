use std::sync::Arc;

use super::McpRuntimeStore;
use crate::integration::McpRuntime;
use crate::tool::{ToolContext, ToolGeneration, ToolRegistry};

fn generation(revision: u64) -> Arc<ToolGeneration> {
    let mut registry = ToolRegistry::new();
    registry.set_context(Arc::new(ToolContext::empty()));
    Arc::new(ToolGeneration::from_registry(&registry, revision))
}

#[test]
fn snapshot_never_splits_generation_from_runtime_pair() {
    let first_generation = generation(4);
    let first_runtime = Arc::new(McpRuntime::empty());
    let store = McpRuntimeStore::new(Arc::clone(&first_generation), Arc::clone(&first_runtime));

    let first = store.snapshot();
    assert_eq!(first.revision(), 4);
    assert!(Arc::ptr_eq(&first.generation(), &first_generation));
    assert!(Arc::ptr_eq(&first.runtime(), &first_runtime));

    let second_generation = generation(5);
    let second_runtime = Arc::new(McpRuntime::empty());
    store.replace(Arc::clone(&second_generation), Arc::clone(&second_runtime));

    let second = store.snapshot();
    assert_eq!(second.revision(), 5);
    assert!(Arc::ptr_eq(&second.generation(), &second_generation));
    assert!(Arc::ptr_eq(&second.runtime(), &second_runtime));
}
