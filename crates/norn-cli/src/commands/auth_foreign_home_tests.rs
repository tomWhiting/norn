use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use norn::provider::auth::{
    AuthSource, LoginConfig, build_from_auth_source, list_auth_accounts, logout, use_auth_account,
};
use norn::provider::openai_oauth::{
    DeleteAuthOutcome, NamedLoginPreparation, OAuthHttpOptions, RemoteRevokeOutcome,
    prepare_named_login, resolve_norn_auth_root,
};

use super::*;

const TEST_JWT: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJzdWIiOiJ1c2VyIiwiZXhwIjo0MTAyNDQ0ODAwLCJodHRwczovL2FwaS5vcGVu",
    "YWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudCJ9fQ."
);
const NAMED_TEST_JWT: &str = concat!(
    "eyJhbGciOiJub25lIn0.",
    "eyJzdWIiOiJ1c2VyIiwiZXhwIjo0MTAyNDQ0ODAwLCJodHRwczovL2FwaS5vcGVu",
    "YWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoibmFtZWQtYWNjb3VudCJ9fQ."
);

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Eq, PartialEq)]
struct EntrySnapshot {
    kind: EntryKind,
    bytes: Option<Vec<u8>>,
    symlink_target: Option<PathBuf>,
    len: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    links: u64,
    #[cfg(unix)]
    owner: u32,
    #[cfg(unix)]
    group: u32,
}

#[derive(Eq, PartialEq)]
struct ForeignHomeSnapshot {
    entries: BTreeMap<PathBuf, EntrySnapshot>,
}

#[tokio::test]
#[serial_test::serial]
async fn default_auth_surfaces_leave_foreign_home_unchanged_at_each_checkpoint() -> TestResult {
    let norn_home = tempfile::tempdir()?;
    let codex_home = tempfile::tempdir()?;
    let norn_auth_root = norn_home.path().join("auth");
    seed_norn_auth(&norn_auth_root)?;
    populate_foreign_home(codex_home.path())?;
    let foreign_before = snapshot_foreign_home(codex_home.path())?;

    temp_env::async_with_vars(
        [
            ("NORN_HOME", Some(norn_home.path().as_os_str())),
            ("CODEX_HOME", Some(codex_home.path().as_os_str())),
        ],
        async {
            let expected_root = NornAuthRoot::try_from(norn_home.path().join("auth"))?;
            assert_eq!(resolve_norn_auth_root(None)?, expected_root);

            assert_eq!(run_status(None), ExitCode::Success);
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;

            assert!(crate::commands::doctor::check_auth());
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;

            let prepared =
                prepare_named_login(&expected_root, "isolated", OAuthHttpOptions::default())?;
            let NamedLoginPreparation::Pending(reservation) = prepared else {
                return Err(
                    std::io::Error::other("fresh named login unexpectedly recovered").into(),
                );
            };
            seed_norn_auth_as(
                reservation.auth_root().as_path(),
                NAMED_TEST_JWT,
                "named-account",
            )?;
            reservation.commit()?;
            use_auth_account("isolated")?;
            assert!(list_auth_accounts()?.iter().any(|account| {
                account.alias == "isolated" && account.active && !account.legacy_default
            }));
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;

            let provider = build_from_auth_source(&AuthSource::oauth_default()).await?;
            let request = provider
                .apply_auth(reqwest::Client::new().get("http://example.invalid"))
                .await?
                .build()?;
            assert!(request.headers().contains_key("Authorization"));
            drop(provider);
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;

            let named_report =
                norn::provider::auth::logout_named(LoginConfig::default(), "isolated").await?;
            assert!(matches!(named_report.local, Ok(DeleteAuthOutcome::Removed)));
            assert!(matches!(
                named_report.remote,
                RemoteRevokeOutcome::NotApplicable
            ));
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;

            let report = logout(LoginConfig::default()).await?;
            assert!(matches!(report.local, Ok(DeleteAuthOutcome::Removed)));
            assert!(matches!(report.remote, RemoteRevokeOutcome::NotApplicable));
            assert!(!norn_auth_root.join("auth.json").exists());
            verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;
            Ok::<(), Box<dyn std::error::Error>>(())
        },
    )
    .await?;

    verify_foreign_home_unchanged(codex_home.path(), &foreign_before)?;
    Ok(())
}

fn seed_norn_auth(root: &Path) -> TestResult {
    seed_norn_auth_as(root, TEST_JWT, "account")
}

fn seed_norn_auth_as(root: &Path, jwt: &str, account_id: &str) -> TestResult {
    std::fs::create_dir_all(root)?;
    std::fs::write(
        root.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": jwt,
                "access_token": jwt,
                "refresh_token": "",
                "account_id": account_id
            }
        }))?,
    )?;
    Ok(())
}

fn populate_foreign_home(root: &Path) -> Result<(), std::io::Error> {
    let profiles = root.join("profiles");
    std::fs::create_dir(&profiles)?;
    std::fs::write(root.join("auth.json"), b"foreign-auth-sentinel")?;
    std::fs::write(root.join("profile.json"), b"foreign-profile-sentinel")?;
    std::fs::write(
        profiles.join("secondary.json"),
        b"foreign-secondary-sentinel",
    )?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("profiles/secondary.json", root.join("current-profile"))?;
        set_fixture_modes(root, &profiles)?;
    }
    Ok(())
}

fn snapshot_foreign_home(root: &Path) -> Result<ForeignHomeSnapshot, std::io::Error> {
    let mut entries = BTreeMap::new();
    snapshot_entry(root, Path::new("."), &mut entries)?;
    Ok(ForeignHomeSnapshot { entries })
}

fn verify_foreign_home_unchanged(
    root: &Path,
    before: &ForeignHomeSnapshot,
) -> Result<(), std::io::Error> {
    let after = snapshot_foreign_home(root)?;
    if &after != before {
        return Err(std::io::Error::other(
            "foreign CODEX_HOME inventory, bytes, or metadata changed",
        ));
    }
    if after.entries.keys().any(|path| is_norn_artifact(path)) {
        return Err(std::io::Error::other(
            "foreign CODEX_HOME contains a Norn credential transaction artifact",
        ));
    }
    Ok(())
}

fn snapshot_entry(
    root: &Path,
    relative: &Path,
    entries: &mut BTreeMap<PathBuf, EntrySnapshot>,
) -> Result<(), std::io::Error> {
    let absolute = if relative == Path::new(".") {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let metadata = std::fs::symlink_metadata(&absolute)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    };
    let bytes = file_type
        .is_file()
        .then(|| std::fs::read(&absolute))
        .transpose()?;
    let symlink_target = file_type
        .is_symlink()
        .then(|| std::fs::read_link(&absolute))
        .transpose()?;
    entries.insert(
        relative.to_path_buf(),
        entry_snapshot(kind, bytes, symlink_target, &metadata),
    );

    if file_type.is_dir() {
        let mut children = std::fs::read_dir(&absolute)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        for child in children {
            snapshot_entry(root, &relative.join(child), entries)?;
        }
    }
    Ok(())
}

fn entry_snapshot(
    kind: EntryKind,
    bytes: Option<Vec<u8>>,
    symlink_target: Option<PathBuf>,
    metadata: &std::fs::Metadata,
) -> EntrySnapshot {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;

    EntrySnapshot {
        kind,
        bytes,
        symlink_target,
        len: metadata.len(),
        created: metadata.created().ok(),
        modified: metadata.modified().ok(),
        readonly: metadata.permissions().readonly(),
        #[cfg(unix)]
        mode: metadata.mode(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(unix)]
        links: metadata.nlink(),
        #[cfg(unix)]
        owner: metadata.uid(),
        #[cfg(unix)]
        group: metadata.gid(),
    }
}

fn is_norn_artifact(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name == ".norn-auth.lock"
            || (name.starts_with("auth.json.") && name.ends_with(".tmp"))
            || (name.starts_with("auth.json.") && name.ends_with(".logout-quarantine"))
    })
}

#[cfg(unix)]
fn set_fixture_modes(root: &Path, profiles: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))?;
    std::fs::set_permissions(profiles, std::fs::Permissions::from_mode(0o710))?;
    std::fs::set_permissions(
        root.join("auth.json"),
        std::fs::Permissions::from_mode(0o640),
    )?;
    std::fs::set_permissions(
        root.join("profile.json"),
        std::fs::Permissions::from_mode(0o600),
    )?;
    std::fs::set_permissions(
        profiles.join("secondary.json"),
        std::fs::Permissions::from_mode(0o644),
    )
}
