//! Field-level `#[tool_args(...)]` attribute parsing.
//!
//! These attributes are schema-only overrides: they never change how serde
//! deserializes the field, only how it is described to the model. Unknown
//! keys are rejected with a spanned error so typos surface at compile time
//! rather than being silently dropped.

use proc_macro2::TokenStream;

use super::serde_attrs::parse_string_value;
use syn::{Attribute, Error};

/// Field-level `#[tool_args(...)]` schema-only overrides.
#[derive(Default)]
pub(super) struct FieldToolArgs {
    /// `#[tool_args(skip)]` — excluded from the schema only.
    pub(super) skip: bool,
    /// `#[tool_args(required)]` — force-include in `required`.
    pub(super) required: bool,
    /// `#[tool_args(description = "...")]` override, if any.
    pub(super) description: Option<String>,
    /// `#[tool_args(schema = {...})]` replacement tokens, if any.
    pub(super) schema: Option<TokenStream>,
    /// `#[tool_args(additional_properties)]`.
    pub(super) additional_properties: bool,
}

/// Parses `#[tool_args(schema = {...}, description = "...", skip, required,
/// additional_properties)]` on a field.
pub(super) fn parse_field_tool_args(attrs: &[Attribute]) -> syn::Result<FieldToolArgs> {
    let mut out = FieldToolArgs::default();
    for attr in attrs {
        if !attr.path().is_ident("tool_args") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                out.skip = true;
            } else if meta.path.is_ident("required") {
                out.required = true;
            } else if meta.path.is_ident("additional_properties") {
                out.additional_properties = true;
            } else if meta.path.is_ident("description") {
                out.description = Some(parse_string_value(&meta)?);
            } else if meta.path.is_ident("schema") {
                let value = meta.value()?;
                // The schema value is a single JSON-object token tree (`{...}`);
                // parse exactly one tree so sibling keys after a comma are left
                // for `parse_nested_meta` to handle.
                let tree: proc_macro2::TokenTree = value.parse()?;
                let is_brace_object = matches!(
                    &tree,
                    proc_macro2::TokenTree::Group(group)
                        if group.delimiter() == proc_macro2::Delimiter::Brace
                );
                if !is_brace_object {
                    return Err(Error::new_spanned(
                        &tree,
                        "ToolArgs: #[tool_args(schema = ...)] requires a JSON object literal, \
                         e.g. schema = {\"type\": \"string\"}",
                    ));
                }
                out.schema = Some(TokenStream::from(tree));
            } else {
                return Err(meta.error(
                    "ToolArgs: unknown #[tool_args(...)] key — expected one of \
                     `schema`, `description`, `skip`, `required`, `additional_properties`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(out)
}
