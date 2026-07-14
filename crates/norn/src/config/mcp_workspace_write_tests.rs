use std::io;

use super::*;

#[test]
fn creates_and_atomically_replaces_shared_settings() -> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;
    let document = WorkspaceSettingsDocument::open(&canonical, WorkspaceSettingsFile::Shared)?;

    assert!(document.read()?.is_none());
    document.replace(b"{\"first\":true}\n")?;
    assert_eq!(document.read()?.as_deref(), Some("{\"first\":true}\n"));
    document.replace(b"{\"second\":true}\n")?;
    assert_eq!(document.read()?.as_deref(), Some("{\"second\":true}\n"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_settings_target() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;
    std::fs::create_dir(canonical.join(".norn"))?;
    let outside = project.path().join("outside.json");
    std::fs::write(&outside, "outside")?;
    symlink(&outside, canonical.join(".norn/settings.json"))?;

    let document = WorkspaceSettingsDocument::open(&canonical, WorkspaceSettingsFile::Shared)?;
    assert!(document.read().is_err());
    assert!(document.replace(b"{}\n").is_err());
    assert_eq!(std::fs::read_to_string(outside)?, "outside");
    Ok(())
}

#[cfg(unix)]
#[test]
fn replacement_preserves_existing_project_file_mode() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let project = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;
    std::fs::create_dir(canonical.join(".norn"))?;
    let path = canonical.join(".norn/settings.json");
    std::fs::write(&path, "{}\n")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))?;
    let document = WorkspaceSettingsDocument::open(&canonical, WorkspaceSettingsFile::Shared)?;

    document.replace(b"{\"changed\":true}\n")?;

    assert_eq!(std::fs::metadata(path)?.mode() & 0o777, 0o640);
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_norn_directory() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let project = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let canonical = project.path().canonicalize()?;
    symlink(outside.path(), canonical.join(".norn"))?;

    let Err(error) = WorkspaceSettingsDocument::open(&canonical, WorkspaceSettingsFile::Shared)
    else {
        return Err("symlinked .norn directory was accepted".into());
    };
    assert_ne!(error.kind(), io::ErrorKind::NotFound);
    assert!(!outside.path().join("settings.json").exists());
    Ok(())
}

#[test]
fn pinned_directory_survives_workspace_path_replacement() -> Result<(), Box<dyn std::error::Error>>
{
    let container = tempfile::tempdir()?;
    let project = container.path().join("project");
    std::fs::create_dir(&project)?;
    let canonical = project.canonicalize()?;
    let document = WorkspaceSettingsDocument::open(&canonical, WorkspaceSettingsFile::Shared)?;
    let parked = container.path().join("parked");
    std::fs::rename(&project, &parked)?;
    std::fs::create_dir(&project)?;

    document.replace(b"{\"pinned\":true}\n")?;

    assert_eq!(
        std::fs::read_to_string(parked.join(".norn/settings.json"))?,
        "{\"pinned\":true}\n",
    );
    assert!(!project.join(".norn/settings.json").exists());
    Ok(())
}
