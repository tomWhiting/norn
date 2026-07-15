//! Repository path and immutable snapshot tests.

use std::error::Error;

use norn_policy::path::{RepositoryPath, RepositoryPathError};
use norn_policy::snapshot::{
    EntryKind, MutationProposal, OwnedSnapshot, SnapshotEntry, SnapshotError, SnapshotMutation,
};

#[test]
fn repository_path_accepts_only_normalized_relative_paths() -> Result<(), Box<dyn Error>> {
    let path: RepositoryPath = "crates/norn/src/lib.rs".parse()?;
    assert_eq!(path.as_str(), "crates/norn/src/lib.rs");
    assert_eq!(path.file_name(), "lib.rs");
    assert_eq!(
        path.parent().as_ref().map(RepositoryPath::as_str),
        Some("crates/norn/src")
    );

    let root_file: RepositoryPath = "Cargo.toml".parse()?;
    assert!(root_file.parent().is_none());

    let encoded = serde_json::to_string(&path)?;
    let decoded: RepositoryPath = serde_json::from_str(&encoded)?;
    assert_eq!(decoded, path);
    Ok(())
}

#[test]
fn repository_path_rejects_every_non_normalized_shape() {
    let cases = [
        ("", RepositoryPathError::Empty),
        ("/etc/passwd", RepositoryPathError::Absolute),
        ("C:/work/file.rs", RepositoryPathError::WindowsPrefix),
        ("src\\lib.rs", RepositoryPathError::Backslash),
        ("src//lib.rs", RepositoryPathError::EmptyComponent),
        ("src/", RepositoryPathError::EmptyComponent),
        ("./src/lib.rs", RepositoryPathError::DotComponent),
        ("src/../lib.rs", RepositoryPathError::ParentComponent),
        ("src/\nlib.rs", RepositoryPathError::ControlCharacter),
    ];

    for (raw, expected) in cases {
        assert_eq!(RepositoryPath::parse(raw), Err(expected));
    }
}

#[test]
fn snapshot_copies_borrowed_bytes_and_redacts_debug_content() {
    let mut source = b"credential-sentinel".to_vec();
    let entry = SnapshotEntry::copy_from_slice(EntryKind::Regular, &source);
    source.fill(b'x');

    assert_eq!(entry.bytes(), b"credential-sentinel");
    assert_eq!(entry.kind(), EntryKind::Regular);
    assert_eq!(entry.len(), 19);
    assert!(!entry.is_empty());

    let rendered = format!("{entry:?}");
    assert!(!rendered.contains("credential-sentinel"));
    assert!(rendered.contains("byte_len"));
}

#[test]
fn snapshot_order_is_deterministic_and_duplicates_fail() -> Result<(), Box<dyn Error>> {
    let a: RepositoryPath = "a.rs".parse()?;
    let z: RepositoryPath = "z.rs".parse()?;
    let snapshot = OwnedSnapshot::try_from_entries([
        (z, SnapshotEntry::regular(Vec::from(&b"z"[..]))),
        (a.clone(), SnapshotEntry::regular(Vec::from(&b"a"[..]))),
    ])?;

    let ordered: Vec<&str> = snapshot.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(ordered, ["a.rs", "z.rs"]);

    let duplicate = OwnedSnapshot::try_from_entries([
        (a.clone(), SnapshotEntry::regular(Vec::<u8>::new())),
        (a.clone(), SnapshotEntry::regular(Vec::<u8>::new())),
    ]);
    assert_eq!(duplicate, Err(SnapshotError::DuplicateEntry { path: a }));
    Ok(())
}

#[test]
fn canonical_snapshot_identity_binds_every_analysis_input() -> Result<(), Box<dyn Error>> {
    let baseline = identity_snapshot(&[
        ("a.rs", EntryKind::Regular, b"a"),
        ("z.rs", EntryKind::Symlink, b"target"),
    ])?;
    let reversed = identity_snapshot(&[
        ("z.rs", EntryKind::Symlink, b"target"),
        ("a.rs", EntryKind::Regular, b"a"),
    ])?;
    assert_eq!(baseline.canonical_identity(), reversed.canonical_identity());
    assert_eq!(
        baseline.canonical_identity().to_string(),
        "d062b92866feb5bfaf16f851d0cef6efdc6ddd6474eb7793e474374aa5f3a58b"
    );

    let variants = [
        OwnedSnapshot::empty(),
        identity_snapshot(&[("a.rs", EntryKind::Regular, b"a")])?,
        identity_snapshot(&[
            ("a.rs", EntryKind::Regular, b"a"),
            ("z.rs", EntryKind::Symlink, b"target"),
            ("extra.rs", EntryKind::Regular, b"extra"),
        ])?,
        identity_snapshot(&[
            ("a.rs", EntryKind::Regular, b"a"),
            ("z.rs", EntryKind::Regular, b"target"),
        ])?,
        identity_snapshot(&[
            ("a.rs", EntryKind::Regular, b"a"),
            ("z.rs", EntryKind::Other, b"target"),
        ])?,
        identity_snapshot(&[
            ("a.rs", EntryKind::Regular, b"changed"),
            ("z.rs", EntryKind::Symlink, b"target"),
        ])?,
        identity_snapshot(&[
            ("b.rs", EntryKind::Regular, b"a"),
            ("z.rs", EntryKind::Symlink, b"target"),
        ])?,
    ];
    for variant in variants {
        assert_ne!(baseline.canonical_identity(), variant.canonical_identity());
    }
    Ok(())
}

#[test]
fn overlay_applies_create_modify_delete_without_mutating_base() -> Result<(), Box<dyn Error>> {
    let modify_path: RepositoryPath = "src/lib.rs".parse()?;
    let delete_path: RepositoryPath = "src/old.rs".parse()?;
    let create_path: RepositoryPath = "src/new.rs".parse()?;
    let base = OwnedSnapshot::try_from_entries([
        (
            modify_path.clone(),
            SnapshotEntry::regular(Vec::from(&b"old-lib"[..])),
        ),
        (
            delete_path.clone(),
            SnapshotEntry::regular(Vec::from(&b"old-file"[..])),
        ),
    ])?;
    let proposal = MutationProposal::try_from_mutations([
        SnapshotMutation::create(
            create_path.clone(),
            SnapshotEntry::regular(Vec::from(&b"new-file"[..])),
        ),
        SnapshotMutation::delete(delete_path.clone()),
        SnapshotMutation::modify(
            modify_path.clone(),
            SnapshotEntry::regular(Vec::from(&b"new-lib"[..])),
        ),
    ])?;

    let overlaid = base.overlay(&proposal)?;
    assert_eq!(entry_bytes(&overlaid, &modify_path), Some(&b"new-lib"[..]));
    assert_eq!(entry_bytes(&overlaid, &create_path), Some(&b"new-file"[..]));
    assert!(!overlaid.contains_path(&delete_path));

    assert_eq!(entry_bytes(&base, &modify_path), Some(&b"old-lib"[..]));
    assert!(base.contains_path(&delete_path));
    assert!(!base.contains_path(&create_path));
    Ok(())
}

#[test]
fn proposal_rejects_duplicate_paths_before_overlay() -> Result<(), Box<dyn Error>> {
    let path: RepositoryPath = "src/lib.rs".parse()?;
    let proposal = MutationProposal::try_from_mutations([
        SnapshotMutation::modify(
            path.clone(),
            SnapshotEntry::regular(Vec::from(&b"first"[..])),
        ),
        SnapshotMutation::delete(path.clone()),
    ]);

    assert_eq!(proposal, Err(SnapshotError::DuplicateMutation { path }));
    Ok(())
}

#[test]
fn overlay_enforces_operation_preconditions() -> Result<(), Box<dyn Error>> {
    let present: RepositoryPath = "present.rs".parse()?;
    let missing: RepositoryPath = "missing.rs".parse()?;
    let base = OwnedSnapshot::try_from_entries([(
        present.clone(),
        SnapshotEntry::regular(Vec::<u8>::new()),
    )])?;

    let create_conflict = MutationProposal::try_from_mutations([SnapshotMutation::create(
        present.clone(),
        SnapshotEntry::regular(Vec::<u8>::new()),
    )])?;
    assert_eq!(
        base.overlay(&create_conflict),
        Err(SnapshotError::CreateTargetExists { path: present })
    );

    let missing_modify = MutationProposal::try_from_mutations([SnapshotMutation::modify(
        missing.clone(),
        SnapshotEntry::regular(Vec::<u8>::new()),
    )])?;
    assert_eq!(
        base.overlay(&missing_modify),
        Err(SnapshotError::MutationTargetMissing {
            path: missing.clone()
        })
    );

    let missing_delete =
        MutationProposal::try_from_mutations([SnapshotMutation::delete(missing.clone())])?;
    assert_eq!(
        base.overlay(&missing_delete),
        Err(SnapshotError::MutationTargetMissing { path: missing })
    );
    Ok(())
}

#[test]
fn entry_kinds_remain_explicit_without_following_links() {
    let link = SnapshotEntry::symlink(Vec::from(&b"../outside"[..]));
    let other = SnapshotEntry::other(Vec::<u8>::new());
    assert_eq!(link.kind(), EntryKind::Symlink);
    assert_eq!(link.bytes(), b"../outside");
    assert_eq!(other.kind(), EntryKind::Other);
}

#[test]
fn snapshot_rejects_descendants_beneath_non_directory_entries() -> Result<(), Box<dyn Error>> {
    for (ancestor_kind, ancestor_entry) in [
        (
            EntryKind::Symlink,
            SnapshotEntry::symlink(b"outside".to_vec()),
        ),
        (EntryKind::Other, SnapshotEntry::other(Vec::<u8>::new())),
        (EntryKind::Regular, SnapshotEntry::regular(Vec::<u8>::new())),
    ] {
        let ancestor: RepositoryPath = "authority".parse()?;
        let descendant: RepositoryPath = "authority/source.rs".parse()?;
        let result = OwnedSnapshot::try_from_entries([
            (descendant.clone(), SnapshotEntry::regular(Vec::<u8>::new())),
            (ancestor.clone(), ancestor_entry),
        ]);

        assert_eq!(
            result,
            Err(SnapshotError::DescendantBeneathEntry {
                ancestor,
                ancestor_kind,
                descendant,
            })
        );
    }
    Ok(())
}

#[test]
fn overlay_rejects_creation_of_an_ancestor_above_existing_content() -> Result<(), Box<dyn Error>> {
    let ancestor: RepositoryPath = "authority".parse()?;
    let descendant: RepositoryPath = "authority/source.rs".parse()?;
    let base = OwnedSnapshot::try_from_entries([(
        descendant.clone(),
        SnapshotEntry::regular(Vec::<u8>::new()),
    )])?;
    let proposal = MutationProposal::try_from_mutations([SnapshotMutation::create(
        ancestor.clone(),
        SnapshotEntry::symlink(b"outside".to_vec()),
    )])?;

    assert_eq!(
        base.overlay(&proposal),
        Err(SnapshotError::DescendantBeneathEntry {
            ancestor,
            ancestor_kind: EntryKind::Symlink,
            descendant,
        })
    );
    Ok(())
}

fn entry_bytes<'a>(snapshot: &'a OwnedSnapshot, path: &RepositoryPath) -> Option<&'a [u8]> {
    snapshot.get(path).map(SnapshotEntry::bytes)
}

fn identity_snapshot(
    entries: &[(&str, EntryKind, &[u8])],
) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, kind, bytes)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::copy_from_slice(*kind, bytes),
            ))
        })
        .collect::<Result<Vec<_>, RepositoryPathError>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}
