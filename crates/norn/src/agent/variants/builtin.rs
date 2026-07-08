//! Built-in agent-variant definitions: `explorer`, `reviewer`, `implementer`.
//!
//! Built-ins are DATA, not code paths (brief `agent-variants` R2): user and
//! project settings override any of them by name, per-field, at catalog
//! build time (see [`super::VariantCatalog`]). Prompt texts live in
//! `prompts/*.md` and are embedded at compile time.
//!
//! The `reviewer` variant deliberately ships with `model_required` and no
//! model: same-tier review of the code that wrote it is the fleet
//! playbook's "broken reviewer", and norn's model catalog is
//! provider-dependent, so any hardcoded review tier would be an invented
//! value. The review tier is an owner-level config value
//! (`variants.reviewer.model`) — ruled 2026-07-04.
//!
//! Recorded toolset boundary (the read-only+diagnostics ruling): the
//! built-in reviewer's subset carries no `bash` and no `task`, so it
//! cannot run gates (fmt/clippy/tests) itself — it reviews code and
//! evidence, and the playbook's "make the reviewer run the gates
//! independently" discipline is satisfied by giving gate-running reviews
//! a wider allowlist via a settings override (`variants.reviewer.tools`),
//! not by widening the built-in.

/// A single compiled-in variant definition.
///
/// Mirrors the resolvable surface of
/// [`crate::config::types::VariantSettings`] with `'static` data. `model`
/// is deliberately absent: no built-in pins a model — `explorer` and
/// `implementer` inherit the parent's, and `reviewer` requires an
/// owner-configured one.
pub(super) struct BuiltinVariant {
    /// Variant name (the settings key that overrides it).
    pub name: &'static str,
    /// One-line purpose description.
    pub description: &'static str,
    /// Base system-prompt block for children of this variant.
    pub prompt: &'static str,
    /// Tool-name allowlist; `None` = inherit the parent's full registry
    /// surface. Always further intersected with the child's granted
    /// delegation policy at assembly (policy WINS — brief R6).
    pub tools: Option<&'static [&'static str]>,
    /// Whether spawning this variant without a model anywhere is a typed
    /// error (the reviewer ruling).
    pub model_required: bool,
}

/// The three shipped built-ins, in stable order.
pub(super) const BUILTIN_VARIANTS: &[BuiltinVariant] = &[
    BuiltinVariant {
        name: "explorer",
        description: "Read-only exploration: wide search, evidence-cited reporting, no mutations.",
        prompt: include_str!("prompts/explorer.md"),
        tools: Some(&[
            "read",
            "search",
            "lsp",
            "tool_search",
            "action_log",
            "agents",
            "web_fetch",
            "web_search",
        ]),
        model_required: false,
    },
    BuiltinVariant {
        name: "reviewer",
        description: "Adversarial review: defect-hunting against brief and intent, read-only plus diagnostics.",
        prompt: include_str!("prompts/reviewer.md"),
        tools: Some(&["read", "search", "lsp", "tool_search", "action_log"]),
        model_required: true,
    },
    BuiltinVariant {
        name: "implementer",
        description: "Complete, verified implementation work with the full tool set.",
        prompt: include_str!("prompts/implementer.md"),
        tools: None,
        model_required: false,
    },
];
