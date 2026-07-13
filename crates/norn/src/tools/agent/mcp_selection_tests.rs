use std::sync::Arc;

use super::*;
use crate::integration::mcp_runtime::tests::runtime_with_servers;
use crate::provider::surface::collect_registered_function_definitions;

#[test]
fn child_can_select_connected_server_hidden_from_root() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Arc::new(runtime_with_servers(&["alpha", "beta"]));
    let mut registry = ToolRegistry::new();
    runtime.register_tools(&mut registry)?;
    runtime.restrict_registry_to_servers(&mut registry, &["alpha".to_owned()])?;
    let beta = runtime.tool_names_for_servers(&["beta".to_owned()])?;
    let beta_name = beta.first().ok_or("beta fixture exposed no tool")?;
    assert!(registry.get(beta_name).is_none());
    assert!(registry.get_registered(beta_name).is_some());

    let ctx = ToolContext::empty();
    ctx.insert_extension(Arc::clone(&runtime));
    let selection = apply_mcp_server_selection(&ctx, &registry, None, Some(&["beta".to_owned()]))?
        .ok_or("explicit child MCP selection did not materialize an allow-list")?;

    assert!(selection.contains(beta_name));
    let definitions = collect_registered_function_definitions(&registry, &selection);
    assert!(
        definitions
            .iter()
            .any(|definition| definition.name == *beta_name)
    );
    Ok(())
}
