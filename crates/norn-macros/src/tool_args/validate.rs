//! Compile-time policy checks run after parsing and before schema generation.
//!
//! The single rule enforced here is documentation coverage: every
//! schema-visible field — whether on a struct or inside a named enum variant —
//! must carry a description, sourced either from a `///` doc comment or a
//! `#[tool_args(description = "...")]` override. A missing description produces
//! a `compile_error!` anchored to the field, so the model never receives a tool
//! schema with an undocumented parameter (S3 / S15).

use syn::Error;

use super::parse::{Parsed, ParsedField, VariantFields};

/// Validates the parsed item, returning the first documentation violation as a
/// spanned error.
pub(super) fn validate(parsed: &Parsed) -> syn::Result<()> {
    match parsed {
        Parsed::Struct(parsed) => validate_fields(&parsed.fields),
        Parsed::Enum(parsed) => {
            for variant in &parsed.variants {
                if let VariantFields::Named(fields) = &variant.fields {
                    validate_fields(fields)?;
                }
            }
            Ok(())
        }
    }
}

/// Checks every schema-visible field in `fields` for a description. Only the
/// three skip forms the brief enumerates exempt a field from the rule:
/// `#[tool_args(skip)]`, `#[serde(skip)]`, and `#[serde(skip_deserializing)]`.
/// Every other field — `#[serde(flatten)]` included — must carry a description.
fn validate_fields(fields: &[ParsedField]) -> syn::Result<()> {
    for field in fields {
        if !requires_documentation(field) {
            continue;
        }
        if !has_description(field) {
            let name = field.ident.to_string();
            return Err(Error::new_spanned(
                &field.ident,
                format!(
                    "ToolArgs: field `{name}` has no doc comment. \
                     Add a /// comment to provide the schema description."
                ),
            ));
        }
    }
    Ok(())
}

/// A field needs documentation unless it is excluded from the schema by one of
/// the three skip forms the brief enumerates. `#[serde(flatten)]` is *not* an
/// exemption — the brief requires a doc comment on every non-skipped field.
fn requires_documentation(field: &ParsedField) -> bool {
    !(field.tool_args_skip || field.serde_skip || field.serde_skip_deserializing)
}

/// Whether the field has any description source — a doc comment or a
/// `#[tool_args(description = "...")]` override.
fn has_description(field: &ParsedField) -> bool {
    !field.description.is_empty()
        || field
            .tool_args_description
            .as_deref()
            .is_some_and(|d| !d.is_empty())
}
