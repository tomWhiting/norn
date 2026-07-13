use super::*;
use crate::config::{McpDefinitionFingerprint, McpServerSettings};

fn server(source: McpConfigSource, fingerprint: &str) -> ResolvedMcpServer {
    ResolvedMcpServer {
        name: "docs".to_owned(),
        source,
        definition: McpServerSettings {
            command: Some("server".to_owned()),
            ..McpServerSettings::default()
        },
        fingerprint: McpDefinitionFingerprint(fingerprint.to_owned()),
    }
}

#[test]
fn project_approval_is_definition_bound() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let store = McpApprovalStore::at_root(home.path())?;
    let first = server(McpConfigSource::Project, "first");
    let changed = server(McpConfigSource::Project, "changed");

    assert_eq!(
        store.state(project.path(), &first)?,
        McpApprovalState::Pending
    );
    store.approve(project.path(), &first)?;
    assert_eq!(
        store.state(project.path(), &first)?,
        McpApprovalState::Approved
    );
    assert_eq!(
        store.state(project.path(), &changed)?,
        McpApprovalState::Pending
    );
    Ok(())
}

#[test]
fn direct_scope_never_requires_approval() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let store = McpApprovalStore::at_root(home.path())?;

    for source in [
        McpConfigSource::User,
        McpConfigSource::Local,
        McpConfigSource::Cli,
        McpConfigSource::Session,
    ] {
        assert_eq!(
            store.state(project.path(), &server(source, "direct"))?,
            McpApprovalState::NotRequired
        );
    }
    Ok(())
}

#[test]
fn revocation_removes_remembered_approval() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let store = McpApprovalStore::at_root(home.path())?;
    let server = server(McpConfigSource::Project, "approved");

    store.approve(project.path(), &server)?;
    store.revoke(project.path(), server.name())?;

    assert_eq!(
        store.state(project.path(), &server)?,
        McpApprovalState::Pending
    );
    Ok(())
}
