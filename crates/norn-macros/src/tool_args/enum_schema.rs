//! JSON Schema generation for enums.
//!
//! Handles the four serde enum representations relevant to tool schemas:
//!
//! * **String enum** — every variant is `Unit` and there is no `tag` /
//!   `untagged` attribute. Emits `{"type": "string", "enum": [...]}`.
//! * **Internally tagged** — `#[serde(tag = "t")]`. Emits `oneOf` of objects
//!   each carrying `t` as a const discriminator alongside the variant's named
//!   fields.
//! * **Adjacently tagged** — `#[serde(tag = "t", content = "c")]`. Emits
//!   `oneOf` of objects each with the tag as a const and (for data-bearing
//!   variants) the variant's data nested under `c`.
//! * **Untagged** — `#[serde(untagged)]`. Emits `oneOf` of the per-variant
//!   data schemas with no discriminator.
//!
//! Tuple variants (`Foo(String)`) are rejected at parse time. External tagging
//! (data variants without any of the three serde attrs) produces a spanned
//! compile error suggesting the user pick a representation, per the
//! NTM-002 brief.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Error;

use super::parse::{EnumRepresentation, ParsedEnum, ParsedVariant, VariantFields};
use super::rename::RenameRule;
use super::schema::build_field_props;

/// Emits the full `impl` block exposing `json_schema()` for the parsed enum,
/// dispatching to the right schema shape for its representation.
pub(super) fn build_enum_impl(parsed: &ParsedEnum) -> syn::Result<TokenStream> {
    let body = match &parsed.representation {
        EnumRepresentation::Default => build_string_enum(parsed)?,
        EnumRepresentation::InternallyTagged { tag } => build_internally_tagged(parsed, tag)?,
        EnumRepresentation::Adjacent { tag, content } => build_adjacent(parsed, tag, content)?,
        EnumRepresentation::Untagged => build_untagged(parsed)?,
    };
    let ident = &parsed.ident;
    Ok(quote! {
        impl #ident {
            pub fn json_schema() -> ::serde_json::Value {
                #body
            }
        }
    })
}

/// Builds the body for a string-enum: `{"type": "string", "enum": [...]}`,
/// rejecting any variant that carries data (that would be external tagging,
/// which we do not generate).
fn build_string_enum(parsed: &ParsedEnum) -> syn::Result<TokenStream> {
    for variant in &parsed.variants {
        if !matches!(variant.fields, VariantFields::Unit) {
            return Err(Error::new_spanned(
                &variant.ident,
                "ToolArgs: data-bearing enum variants require a serde representation \
                 attribute — add #[serde(tag = \"...\")], #[serde(tag = \"t\", content = \"c\")], \
                 or #[serde(untagged)] on the enum",
            ));
        }
    }
    let wire_names: Vec<String> = parsed
        .variants
        .iter()
        .map(|v| wire_name(v, parsed.rename_all))
        .collect();
    let description = string_enum_description(parsed, &wire_names);
    if description.is_empty() {
        Ok(quote! {
            ::serde_json::json!({
                "type": "string",
                "enum": [ #( #wire_names ),* ]
            })
        })
    } else {
        Ok(quote! {
            ::serde_json::json!({
                "type": "string",
                "enum": [ #( #wire_names ),* ],
                "description": #description
            })
        })
    }
}

/// Composes the description for a string enum from the container doc comment
/// and per-variant entries. Variants without docs are skipped. Returns the
/// empty string when there is nothing to write.
fn string_enum_description(parsed: &ParsedEnum, wire_names: &[String]) -> String {
    let mut parts = Vec::new();
    if !parsed.description.is_empty() {
        parts.push(parsed.description.clone());
    }
    for (variant, name) in parsed.variants.iter().zip(wire_names) {
        if !variant.description.is_empty() {
            parts.push(format!("{name}: {}", variant.description));
        }
    }
    parts.join(" ")
}

/// Builds the body for an internally-tagged enum. Each variant becomes an
/// object schema with the tag field as a `const` plus any named fields the
/// variant carries.
fn build_internally_tagged(parsed: &ParsedEnum, tag: &str) -> syn::Result<TokenStream> {
    let mut variant_schemas = Vec::with_capacity(parsed.variants.len());
    for variant in &parsed.variants {
        let wire = wire_name(variant, parsed.rename_all);
        let (prop_names, prop_schemas, mut required) = match &variant.fields {
            VariantFields::Unit => (Vec::new(), Vec::new(), Vec::new()),
            VariantFields::Named(fields) => build_field_props(fields)?,
        };
        // Tag is always present and always required, in addition to whatever
        // the variant's named fields require.
        required.insert(0, tag.to_string());
        let tag_lit = tag.to_string();
        variant_schemas.push(quote! {
            ::serde_json::json!({
                "type": "object",
                "properties": {
                    #tag_lit : { "const": #wire },
                    #( #prop_names : #prop_schemas ),*
                },
                "required": [ #( #required ),* ],
                "additionalProperties": false
            })
        });
    }
    Ok(wrap_one_of(parsed, &variant_schemas))
}

/// Builds the body for an adjacently-tagged enum. Unit variants get just the
/// tag field; data-bearing variants get both the tag and `content` field, with
/// the content carrying the variant's named-field object schema.
fn build_adjacent(parsed: &ParsedEnum, tag: &str, content: &str) -> syn::Result<TokenStream> {
    let mut variant_schemas = Vec::with_capacity(parsed.variants.len());
    for variant in &parsed.variants {
        let wire = wire_name(variant, parsed.rename_all);
        let tag_lit = tag.to_string();
        let content_lit = content.to_string();
        let schema = match &variant.fields {
            VariantFields::Unit => quote! {
                ::serde_json::json!({
                    "type": "object",
                    "properties": { #tag_lit : { "const": #wire } },
                    "required": [ #tag_lit ],
                    "additionalProperties": false
                })
            },
            VariantFields::Named(fields) => {
                let inner = variant_named_object(fields)?;
                quote! {
                    ::serde_json::json!({
                        "type": "object",
                        "properties": {
                            #tag_lit : { "const": #wire },
                            #content_lit : #inner
                        },
                        "required": [ #tag_lit, #content_lit ],
                        "additionalProperties": false
                    })
                }
            }
        };
        variant_schemas.push(schema);
    }
    Ok(wrap_one_of(parsed, &variant_schemas))
}

/// Builds the body for an untagged enum. Each variant emits its data schema
/// directly into the `oneOf` array with no discriminator.
fn build_untagged(parsed: &ParsedEnum) -> syn::Result<TokenStream> {
    let mut variant_schemas = Vec::with_capacity(parsed.variants.len());
    for variant in &parsed.variants {
        let wire = wire_name(variant, parsed.rename_all);
        let schema = match &variant.fields {
            VariantFields::Unit => quote! {
                ::serde_json::json!({ "type": "string", "const": #wire })
            },
            VariantFields::Named(fields) => variant_named_object(fields)?,
        };
        variant_schemas.push(schema);
    }
    Ok(wrap_one_of(parsed, &variant_schemas))
}

/// Builds an object schema for a named-field variant payload: `type: object`
/// plus the field properties, `required` list and `additionalProperties:
/// false`.
fn variant_named_object(fields: &[super::parse::ParsedField]) -> syn::Result<TokenStream> {
    let (prop_names, prop_schemas, required) = build_field_props(fields)?;
    Ok(quote! {
        ::serde_json::json!({
            "type": "object",
            "properties": {
                #( #prop_names : #prop_schemas ),*
            },
            "required": [ #( #required ),* ],
            "additionalProperties": false
        })
    })
}

/// Wraps a list of variant schemas in `oneOf`, attaching the enum's container
/// doc-comment description when one is present.
fn wrap_one_of(parsed: &ParsedEnum, variant_schemas: &[TokenStream]) -> TokenStream {
    let description = &parsed.description;
    if description.is_empty() {
        quote! {
            ::serde_json::json!({
                "oneOf": [ #( #variant_schemas ),* ]
            })
        }
    } else {
        quote! {
            ::serde_json::json!({
                "oneOf": [ #( #variant_schemas ),* ],
                "description": #description
            })
        }
    }
}

/// Resolves a variant's wire name. Precedence: per-variant `#[serde(rename =
/// "x")]` → container `#[serde(rename_all = "...")]` applied to the Rust
/// ident → the raw Rust ident.
fn wire_name(variant: &ParsedVariant, rename_all: Option<RenameRule>) -> String {
    if let Some(name) = &variant.rename {
        return name.clone();
    }
    let raw = variant.ident.to_string();
    match rename_all {
        Some(rule) => rule.apply(&raw),
        None => raw,
    }
}
