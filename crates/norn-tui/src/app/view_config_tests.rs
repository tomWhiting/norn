//! Declared view defaults and explicit demand overrides.

use super::*;

#[test]
fn defaults_are_demands_and_tools_start_compact() -> Result<(), crate::TuiError> {
    let config = ViewConfig::default();
    assert_eq!(config.history_demand()?.get(), DEFAULT_HISTORY_EVENTS);
    assert_eq!(config.body_demand()?.get(), DEFAULT_BODY_BYTES);
    assert!(!config.expanded_tools);
    assert_eq!(
        config.clipboard,
        crate::terminal::clipboard::ClipboardCapability::Unspecified
    );
    Ok(())
}

#[test]
fn explicit_positive_demand_changes_do_not_change_tool_preferences() -> Result<(), crate::TuiError>
{
    let mut config = ViewConfig::default();
    config.set_history_demand(NonZeroUsize::MIN);
    config.set_body_demand(NonZeroUsize::MIN);
    assert_eq!(config.history_demand()?, NonZeroUsize::MIN);
    assert_eq!(config.body_demand()?, NonZeroUsize::MIN);
    assert!(!config.expanded_tools);
    assert_eq!(
        config.clipboard,
        crate::terminal::clipboard::ClipboardCapability::Unspecified
    );
    Ok(())
}
