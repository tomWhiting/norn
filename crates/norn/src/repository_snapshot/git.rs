//! Closed Git command and inventory boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use norn_policy::phase_lock::GitObjectFormat;
use norn_policy::{GitLeafMode, GitObjectId, RepositoryPath};

use super::error::{GitOperation, SnapshotAdapterError};

const PHASE_LOCK_PATH: &str = "policy/phase-lock.json";

/// Git process boundary fixed to one canonical workspace root.
pub(super) struct GitRunner {
    root: PathBuf,
}

impl GitRunner {
    pub(super) fn discover_root(start: &Path) -> Result<PathBuf, SnapshotAdapterError> {
        let output = run_output_at(
            start,
            GitOperation::DiscoverRoot,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )?;
        parse_root(&output)
    }

    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn verify_object_format(&self) -> Result<(), SnapshotAdapterError> {
        let output = self.output(
            GitOperation::ObjectFormat,
            &["rev-parse", "--show-object-format"],
        )?;
        if singleton_line(&output, GitOperation::ObjectFormat)? != b"sha1" {
            return Err(SnapshotAdapterError::ObjectFormat);
        }
        Ok(())
    }

    pub(super) fn verify_base(
        &self,
        commit: &str,
        tree: &str,
    ) -> Result<(GitObjectId, GitObjectId), SnapshotAdapterError> {
        let commit_revision = format!("{commit}^{{commit}}");
        let observed_commit = self.output(
            GitOperation::VerifyBaseCommit,
            &["rev-parse", "--verify", commit_revision.as_str()],
        )?;
        let observed_commit = parse_sha1(
            singleton_line(&observed_commit, GitOperation::VerifyBaseCommit)?,
            GitOperation::VerifyBaseCommit,
        )?;
        if observed_commit.as_str() != commit {
            return Err(SnapshotAdapterError::BaseIdentity);
        }

        let tree_revision = format!("{commit}^{{tree}}");
        let observed_tree = self.output(
            GitOperation::VerifyBaseTree,
            &["rev-parse", "--verify", tree_revision.as_str()],
        )?;
        let observed_tree = parse_sha1(
            singleton_line(&observed_tree, GitOperation::VerifyBaseTree)?,
            GitOperation::VerifyBaseTree,
        )?;
        if observed_tree.as_str() != tree {
            return Err(SnapshotAdapterError::BaseIdentity);
        }
        Ok((observed_commit, observed_tree))
    }

    pub(super) fn base_tree(
        &self,
        tree: &GitObjectId,
    ) -> Result<Vec<BaseTreeRecord>, SnapshotAdapterError> {
        let output = self.output(
            GitOperation::EnumerateBaseTree,
            &["ls-tree", "-rz", "--full-tree", "-r", tree.as_str()],
        )?;
        parse_base_tree(&output)
    }

    pub(super) fn current_inventory(&self) -> Result<GitInventory, SnapshotAdapterError> {
        let tracked = self.output(
            GitOperation::EnumerateTracked,
            &["ls-files", "-z", "-v", "--stage", "--cached"],
        )?;
        let untracked = self.output(
            GitOperation::EnumerateUntracked,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?;
        let history = self.output(
            GitOperation::MarkerHistory,
            &["rev-list", "--all", "--max-count=1", "--", PHASE_LOCK_PATH],
        )?;
        let marker_in_history = parse_marker_history(&history)?;
        GitInventory::parse(&tracked, &untracked, marker_in_history)
    }

    pub(super) fn command(&self) -> Command {
        git_command_at(&self.root)
    }

    fn output(
        &self,
        operation: GitOperation,
        arguments: &[&str],
    ) -> Result<Vec<u8>, SnapshotAdapterError> {
        run_output_at(&self.root, operation, arguments)
    }
}

impl std::fmt::Debug for GitRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GitRunner(..)")
    }
}

/// One exact leaf row from `git ls-tree -rz`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BaseTreeRecord {
    pub(super) path: RepositoryPath,
    pub(super) mode: GitLeafMode,
    pub(super) object_id: GitObjectId,
}

/// Exact tracked/untracked path authority for one current acquisition pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GitInventory {
    entries: BTreeMap<RepositoryPath, InventoryAuthority>,
    marker_in_history: bool,
}

impl GitInventory {
    pub(super) fn parse(
        tracked: &[u8],
        untracked: &[u8],
        marker_in_history: bool,
    ) -> Result<Self, SnapshotAdapterError> {
        let mut entries = BTreeMap::new();
        for record in nul_records(tracked, GitOperation::EnumerateTracked)? {
            let (header, raw_path) = split_once(record, b'\t', GitOperation::EnumerateTracked)?;
            let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
            let [raw_tag, raw_mode, raw_object, raw_stage] = fields.as_slice() else {
                return Err(SnapshotAdapterError::GitProtocol(
                    GitOperation::EnumerateTracked,
                ));
            };
            match *raw_tag {
                b"H" | b"h" => {}
                b"S" | b"s" => return Err(SnapshotAdapterError::SparseIndex),
                _ => {
                    return Err(SnapshotAdapterError::GitProtocol(
                        GitOperation::EnumerateTracked,
                    ));
                }
            }
            if *raw_stage != b"0" {
                return Err(SnapshotAdapterError::UnmergedIndex);
            }
            let mode = parse_leaf_mode(raw_mode)?;
            let object_id = parse_object_id(raw_object, GitOperation::EnumerateTracked)?;
            let path = parse_repository_path(raw_path, GitOperation::EnumerateTracked)?;
            let authority = InventoryAuthority::Tracked { mode, object_id };
            if entries.insert(path, authority).is_some() {
                return Err(SnapshotAdapterError::DuplicateInventory);
            }
        }
        for raw_path in nul_records(untracked, GitOperation::EnumerateUntracked)? {
            let path = parse_repository_path(raw_path, GitOperation::EnumerateUntracked)?;
            if entries
                .insert(path, InventoryAuthority::Untracked)
                .is_some()
            {
                return Err(SnapshotAdapterError::DuplicateInventory);
            }
        }
        Ok(Self {
            entries,
            marker_in_history,
        })
    }

    pub(super) fn paths(&self) -> impl ExactSizeIterator<Item = &RepositoryPath> {
        self.entries.keys()
    }

    pub(super) fn has_tracked_path(&self, expected: &str) -> bool {
        self.entries.iter().any(|(path, authority)| {
            path.as_str() == expected && matches!(authority, InventoryAuthority::Tracked { .. })
        })
    }

    pub(super) fn marker_observed(&self) -> bool {
        self.marker_in_history || self.has_tracked_path(PHASE_LOCK_PATH)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InventoryAuthority {
    Tracked {
        mode: GitLeafMode,
        object_id: GitObjectId,
    },
    Untracked,
}

pub(super) fn git_command_at(root: &Path) -> Command {
    let mut command = Command::new("git");
    for variable in std::env::vars_os() {
        let key = variable.0;
        if key.to_str().is_some_and(|name| name.starts_with("GIT_")) {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(["-c", "core.fsmonitor=false"]);
    command
}

fn run_output_at(
    root: &Path,
    operation: GitOperation,
    arguments: &[&str],
) -> Result<Vec<u8>, SnapshotAdapterError> {
    let permit = crate::resource::acquire_output_subprocess()?;
    let output = git_command_at(root)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| SnapshotAdapterError::git_spawn(operation, &error));
    drop(permit);
    let output = output?;
    if !output.status.success() {
        return Err(SnapshotAdapterError::GitExit(operation));
    }
    Ok(output.stdout)
}

fn parse_root(output: &[u8]) -> Result<PathBuf, SnapshotAdapterError> {
    let line = singleton_line(output, GitOperation::DiscoverRoot)?;
    let value = std::str::from_utf8(line).map_err(root_encoding_error)?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(SnapshotAdapterError::RepositoryRoot);
    }
    std::fs::canonicalize(path).map_err(|error| SnapshotAdapterError::filesystem(&error))
}

fn parse_marker_history(output: &[u8]) -> Result<bool, SnapshotAdapterError> {
    if output.is_empty() {
        return Ok(false);
    }
    let line = singleton_line(output, GitOperation::MarkerHistory)?;
    parse_object_id(line, GitOperation::MarkerHistory)?;
    Ok(true)
}

fn singleton_line(output: &[u8], operation: GitOperation) -> Result<&[u8], SnapshotAdapterError> {
    let Some(line) = output.strip_suffix(b"\n") else {
        return Err(SnapshotAdapterError::GitProtocol(operation));
    };
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') {
        return Err(SnapshotAdapterError::GitProtocol(operation));
    }
    Ok(line)
}

pub(super) fn parse_base_tree(output: &[u8]) -> Result<Vec<BaseTreeRecord>, SnapshotAdapterError> {
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for record in nul_records(output, GitOperation::EnumerateBaseTree)? {
        let (header, raw_path) = split_once(record, b'\t', GitOperation::EnumerateBaseTree)?;
        let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
        let [raw_mode, raw_kind, raw_object] = fields.as_slice() else {
            return Err(SnapshotAdapterError::GitProtocol(
                GitOperation::EnumerateBaseTree,
            ));
        };
        if *raw_kind != b"blob" {
            return Err(SnapshotAdapterError::GitEntry);
        }
        let path = parse_repository_path(raw_path, GitOperation::EnumerateBaseTree)?;
        let record = BaseTreeRecord {
            path: path.clone(),
            mode: parse_leaf_mode(raw_mode)?,
            object_id: parse_sha1(raw_object, GitOperation::EnumerateBaseTree)?,
        };
        if !seen.insert(path) {
            return Err(SnapshotAdapterError::DuplicateInventory);
        }
        records.push(record);
    }
    Ok(records)
}

fn nul_records(bytes: &[u8], operation: GitOperation) -> Result<Vec<&[u8]>, SnapshotAdapterError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.ends_with(&[0]) {
        return Err(SnapshotAdapterError::GitProtocol(operation));
    }
    let records = bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    if records.iter().any(|record| record.is_empty()) {
        return Err(SnapshotAdapterError::RepositoryPath);
    }
    Ok(records)
}

fn split_once(
    bytes: &[u8],
    delimiter: u8,
    operation: GitOperation,
) -> Result<(&[u8], &[u8]), SnapshotAdapterError> {
    let Some(index) = bytes.iter().position(|byte| *byte == delimiter) else {
        return Err(SnapshotAdapterError::GitProtocol(operation));
    };
    let (left, right) = bytes.split_at(index);
    let Some(right) = right.get(1..) else {
        return Err(SnapshotAdapterError::RepositoryPath);
    };
    if left.is_empty() || right.is_empty() {
        return Err(SnapshotAdapterError::RepositoryPath);
    }
    Ok((left, right))
}

fn parse_repository_path(
    raw: &[u8],
    operation: GitOperation,
) -> Result<RepositoryPath, SnapshotAdapterError> {
    let value = std::str::from_utf8(raw)
        .map_err(|error| SnapshotAdapterError::git_encoding(operation, error))?;
    RepositoryPath::parse(value).map_err(repository_path_error)
}

fn parse_sha1(raw: &[u8], operation: GitOperation) -> Result<GitObjectId, SnapshotAdapterError> {
    let object_id = parse_object_id(raw, operation)?;
    if object_id.object_format() != GitObjectFormat::Sha1 {
        return Err(SnapshotAdapterError::ObjectFormat);
    }
    Ok(object_id)
}

fn parse_object_id(
    raw: &[u8],
    operation: GitOperation,
) -> Result<GitObjectId, SnapshotAdapterError> {
    let value =
        std::str::from_utf8(raw).map_err(|error| object_encoding_error(error, operation))?;
    let object_id = GitObjectId::parse(value).map_err(|error| object_id_error(error, operation))?;
    Ok(object_id)
}

fn root_encoding_error(error: std::str::Utf8Error) -> SnapshotAdapterError {
    SnapshotAdapterError::git_encoding(GitOperation::DiscoverRoot, error)
}

fn repository_path_error(error: norn_policy::RepositoryPathError) -> SnapshotAdapterError {
    match error {
        norn_policy::RepositoryPathError::Empty
        | norn_policy::RepositoryPathError::Absolute
        | norn_policy::RepositoryPathError::WindowsPrefix
        | norn_policy::RepositoryPathError::Backslash
        | norn_policy::RepositoryPathError::EmptyComponent
        | norn_policy::RepositoryPathError::DotComponent
        | norn_policy::RepositoryPathError::ParentComponent
        | norn_policy::RepositoryPathError::ControlCharacter => {
            SnapshotAdapterError::RepositoryPath
        }
    }
}

fn object_encoding_error(
    error: std::str::Utf8Error,
    operation: GitOperation,
) -> SnapshotAdapterError {
    SnapshotAdapterError::git_encoding(operation, error)
}

fn object_id_error(
    error: norn_policy::phase_lock::GitObjectIdError,
    operation: GitOperation,
) -> SnapshotAdapterError {
    match error {
        norn_policy::phase_lock::GitObjectIdError::Length { .. }
        | norn_policy::phase_lock::GitObjectIdError::InvalidHex => {
            SnapshotAdapterError::GitProtocol(operation)
        }
    }
}

fn parse_leaf_mode(raw: &[u8]) -> Result<GitLeafMode, SnapshotAdapterError> {
    match raw {
        b"100644" => Ok(GitLeafMode::Regular),
        b"100755" => Ok(GitLeafMode::Executable),
        b"120000" => Ok(GitLeafMode::Symlink),
        _ => Err(SnapshotAdapterError::GitEntry),
    }
}
