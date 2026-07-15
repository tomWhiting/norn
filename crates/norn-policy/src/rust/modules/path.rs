//! Host-independent path resolution beneath a package authority.

use crate::RepositoryPath;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) struct Directory(Vec<String>);

impl Directory {
    pub(super) fn root() -> Self {
        Self(Vec::new())
    }

    pub(super) fn from_path(path: &RepositoryPath) -> Self {
        Self(path.as_str().split('/').map(str::to_owned).collect())
    }

    pub(super) fn parent_of(path: &RepositoryPath) -> Self {
        path.parent()
            .map_or_else(Self::root, |parent| Self::from_path(&parent))
    }

    pub(super) fn child(&self, name: &str) -> Option<Self> {
        if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
            return None;
        }
        let mut parts = self.0.clone();
        parts.push(name.to_owned());
        Some(Self(parts))
    }

    pub(super) fn file(&self, name: &str) -> Option<RepositoryPath> {
        self.child(name)?.to_repository_path()
    }

    fn to_repository_path(&self) -> Option<RepositoryPath> {
        let Ok(path) = RepositoryPath::parse(self.0.join("/")) else {
            return None;
        };
        Some(path)
    }

    fn starts_with(&self, authority: &Self) -> bool {
        self.0.starts_with(&authority.0)
    }
}

pub(super) fn package_authority(root: Option<&RepositoryPath>) -> Directory {
    root.map_or_else(Directory::root, Directory::from_path)
}

pub(super) fn resolve_literal(
    source: &RepositoryPath,
    raw: &str,
    authority: &Directory,
) -> Option<RepositoryPath> {
    let base = Directory::parent_of(source);
    resolve_from(&base, raw, authority)
}

pub(super) fn resolve_from(
    base: &Directory,
    raw: &str,
    authority: &Directory,
) -> Option<RepositoryPath> {
    resolve_directory_from(base, raw, authority)?.to_repository_path()
}

pub(super) fn resolve_directory_from(
    base: &Directory,
    raw: &str,
    authority: &Directory,
) -> Option<Directory> {
    if !base.starts_with(authority) || invalid_raw(raw) {
        return None;
    }
    let mut parts = base.0.clone();
    let floor = authority.0.len();
    for component in raw.split('/') {
        match component {
            "." => {}
            ".." if parts.len() > floor => {
                parts.pop();
            }
            "" | ".." => return None,
            value => parts.push(value.to_owned()),
        }
    }
    Some(Directory(parts))
}

pub(super) fn is_beneath(path: &RepositoryPath, authority: &Directory) -> bool {
    let mut components = path.as_str().split('/');
    authority
        .0
        .iter()
        .all(|expected| components.next() == Some(expected.as_str()))
}

fn invalid_raw(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    raw.is_empty()
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}
