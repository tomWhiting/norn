//! Closed admission and drift checks for build-script `OUT_DIR` includes.

use std::collections::BTreeSet;

use tree_sitter::Node;

use crate::{Digest, EntryKind, OwnedSnapshot, digest_bytes};

use super::super::cargo::CargoDiscovery;
use super::literal::decode_rust_string;
use super::model::{
    GENERATED_INCLUDE_REGISTRY_VERSION, GeneratedIncludeRegistration, GeneratedIncludeRegistry,
    ModuleDiagnostic, ModuleDiagnosticCode, ModuleTargetIdentity, SourceSpan,
};

/// Hash the canonical form of one admitted generated invocation.
#[must_use]
pub fn generated_invocation_digest(output_basename: &str) -> Option<Digest> {
    valid_basename(output_basename).then(|| {
        digest_bytes(
            format!("include!(concat!(env!(\"OUT_DIR\"),\"/{output_basename}\"))").as_bytes(),
        )
    })
}

pub(super) struct GeneratedAuthority<'a> {
    registry: &'a GeneratedIncludeRegistry,
    used: BTreeSet<usize>,
}

impl<'a> GeneratedAuthority<'a> {
    pub(super) fn new(
        snapshot: &OwnedSnapshot,
        cargo: &CargoDiscovery,
        registry: &'a GeneratedIncludeRegistry,
        diagnostics: &mut Vec<ModuleDiagnostic>,
    ) -> Self {
        validate_registry(snapshot, cargo, registry, diagnostics);
        Self {
            registry,
            used: BTreeSet::new(),
        }
    }

    pub(super) fn encounter(
        &mut self,
        source: &crate::RepositoryPath,
        target: &ModuleTargetIdentity,
        node: Node<'_>,
        enclosing_item: SourceSpan,
        bytes: &[u8],
        diagnostics: &mut Vec<ModuleDiagnostic>,
    ) {
        let callsite = SourceSpan::from_offsets(node.start_byte(), node.end_byte());
        let Some(output_basename) = generated_output(node, bytes) else {
            diagnostics.push(problem(
                ModuleDiagnosticCode::IncludeUnsupported,
                source,
                Some(callsite),
                Some(target.clone()),
                None,
            ));
            return;
        };
        let candidates: Vec<usize> = self
            .registry
            .entries
            .iter()
            .enumerate()
            .filter_map(|(ordinal, entry)| {
                (entry.source == *source
                    && entry.callsite == callsite
                    && entry.enclosing_item == enclosing_item
                    && entry.target == *target)
                    .then_some(ordinal)
            })
            .collect();
        let [ordinal] = candidates.as_slice() else {
            diagnostics.push(problem(
                ModuleDiagnosticCode::GeneratedIncludeUnregistered,
                source,
                Some(callsite),
                Some(target.clone()),
                None,
            ));
            return;
        };
        self.used.insert(*ordinal);
        let entry = &self.registry.entries[*ordinal];
        let digest = generated_invocation_digest(&output_basename);
        if entry.output_basename != output_basename || digest != Some(entry.invocation_digest) {
            diagnostics.push(problem(
                ModuleDiagnosticCode::GeneratedIncludeRegistryDrift,
                source,
                Some(callsite),
                Some(target.clone()),
                Some(*ordinal),
            ));
        }
    }

    pub(super) fn finish(&self, diagnostics: &mut Vec<ModuleDiagnostic>) {
        for (ordinal, entry) in self.registry.entries.iter().enumerate() {
            if !self.used.contains(&ordinal) {
                diagnostics.push(problem(
                    ModuleDiagnosticCode::GeneratedIncludeRegistryUnused,
                    &entry.source,
                    Some(entry.callsite),
                    Some(entry.target.clone()),
                    Some(ordinal),
                ));
            }
        }
    }
}

fn validate_registry(
    snapshot: &OwnedSnapshot,
    cargo: &CargoDiscovery,
    registry: &GeneratedIncludeRegistry,
    diagnostics: &mut Vec<ModuleDiagnostic>,
) {
    if registry.schema_version != GENERATED_INCLUDE_REGISTRY_VERSION {
        if let Some(entry) = registry.entries.first() {
            diagnostics.push(registry_problem(entry, 0));
        } else if let Ok(path) = crate::RepositoryPath::parse("Cargo.toml") {
            diagnostics.push(problem(
                ModuleDiagnosticCode::GeneratedIncludeRegistryDrift,
                &path,
                None,
                None,
                None,
            ));
        }
    }
    let targets: BTreeSet<_> = cargo
        .packages()
        .iter()
        .flat_map(|package| package.targets().iter())
        .map(ModuleTargetIdentity::from_target)
        .collect();
    let mut callsites = BTreeSet::new();
    for (ordinal, entry) in registry.entries.iter().enumerate() {
        if !valid_entry_shape(entry)
            || !targets.contains(&entry.target)
            || !pinned(snapshot, &entry.generator)
            || entry.inputs.iter().any(|input| !pinned(snapshot, input))
            || generated_invocation_digest(&entry.output_basename) != Some(entry.invocation_digest)
        {
            diagnostics.push(registry_problem(entry, ordinal));
        }
        if entry
            .inputs
            .windows(2)
            .any(|pair| pair[0].path >= pair[1].path)
        {
            diagnostics.push(registry_problem(entry, ordinal));
        }
        if !callsites.insert((
            entry.source.clone(),
            entry.callsite,
            entry.enclosing_item,
            entry.target.clone(),
        )) {
            diagnostics.push(registry_problem(entry, ordinal));
        }
    }
    for (ordinal, pair) in registry.entries.windows(2).enumerate() {
        if pair[0] >= pair[1] {
            diagnostics.push(registry_problem(&pair[1], ordinal + 1));
        }
    }
}

fn valid_entry_shape(entry: &GeneratedIncludeRegistration) -> bool {
    entry.callsite.is_valid()
        && entry.enclosing_item.is_valid()
        && entry.enclosing_item.start <= entry.callsite.start
        && entry.callsite.end <= entry.enclosing_item.end
        && valid_basename(&entry.output_basename)
}

fn pinned(snapshot: &OwnedSnapshot, input: &super::model::HashedSourceInput) -> bool {
    snapshot.get(&input.path).is_some_and(|entry| {
        entry.kind() == EntryKind::Regular && digest_bytes(entry.bytes()) == input.digest
    })
}

fn generated_output(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let token_tree = named_child(node, "token_tree")?;
    let Ok(text) = std::str::from_utf8(&bytes[token_tree.byte_range()]) else {
        return None;
    };
    let compact = compact_whitespace(text)?;
    let body = compact.strip_prefix("(concat!(env!(\"OUT_DIR\"),")?;
    let literal = body.strip_suffix("))")?;
    let output = decode_rust_string(literal)?;
    let basename = output.strip_prefix('/')?;
    valid_basename(basename).then(|| basename.to_owned())
}

fn named_child<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn compact_whitespace(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(input.len());
    let mut cursor = 0;
    let mut in_string = false;
    let mut escaped = false;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
            output.push(b'"');
        } else if byte.is_ascii_whitespace() {
        } else if byte.is_ascii() {
            output.push(byte);
        } else {
            return None;
        }
        cursor += 1;
    }
    if in_string {
        None
    } else {
        let Ok(output) = String::from_utf8(output) else {
            return None;
        };
        Some(output)
    }
}

fn valid_basename(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_control)
}

fn registry_problem(entry: &GeneratedIncludeRegistration, ordinal: usize) -> ModuleDiagnostic {
    problem(
        ModuleDiagnosticCode::GeneratedIncludeRegistryDrift,
        &entry.source,
        Some(entry.callsite),
        Some(entry.target.clone()),
        Some(ordinal),
    )
}

fn problem(
    code: ModuleDiagnosticCode,
    path: &crate::RepositoryPath,
    span: Option<SourceSpan>,
    target: Option<ModuleTargetIdentity>,
    ordinal: Option<usize>,
) -> ModuleDiagnostic {
    ModuleDiagnostic {
        code,
        path: path.clone(),
        span,
        related_path: None,
        target,
        ordinal,
    }
}
