//! Browser-launch policy for Unix desktop environments.

use super::{BrowserLaunchError, LaunchSpec};

/// Desktop launchers accept their target through process arguments. OAuth
/// authorization URLs carry one-time state and therefore stay on the explicit
/// terminal presentation boundary instead of entering an observable argv.
pub(super) fn launch_spec(_target: &url::Url) -> Result<LaunchSpec, BrowserLaunchError> {
    Err(BrowserLaunchError::Structural(
        "automatic browser launch is unavailable without a no-argv desktop integration",
    ))
}
