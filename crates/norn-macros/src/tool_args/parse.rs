//! Parses a `syn::DeriveInput` into the structured representation the schema
//! builder consumes: structs become a [`ParsedStruct`] with per-field metadata,
//! enums become a [`ParsedEnum`] with per-variant metadata. Both retain the
//! source-declaration order and the doc-comment text the builders need to keep
//! generated schemas faithful to the Rust definition.
//!
//! Attribute interpretation lives in the sibling `serde_attrs` / `tool_attrs`
//! modules; this module owns the shape checks (named fields only, no tuple
//! variants, no unions, no generics) and the doc-comment extraction.

use proc_macro2::{Span, TokenStream};
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Error, Expr, ExprLit, Fields, Lit, Meta,
    Token, Type,
};

use super::rename::RenameRule;
use super::serde_attrs::{
    parse_enum_serde, parse_field_serde, parse_struct_serde, parse_variant_serde,
};
use super::tool_attrs::parse_field_tool_args;

/// Per-field metadata extracted from a struct or struct-shaped enum variant.
pub(super) struct ParsedField {
    /// The field identifier (the default schema property name).
    pub(super) ident: syn::Ident,
    /// The declared Rust type, mapped to a JSON Schema type by the builder.
    pub(super) ty: Type,
    /// Doc-comment lines joined with a single space; empty if none.
    pub(super) description: String,
    /// Whether the field carries `#[serde(default)]` or `#[serde(default = "fn")]`.
    pub(super) has_default: bool,
    /// `#[serde(rename = "...")]` wire name override, if any.
    pub(super) serde_rename: Option<String>,
    /// `#[serde(skip)]` — omitted from both serialize and deserialize.
    pub(super) serde_skip: bool,
    /// `#[serde(skip_deserializing)]` — not accepted as model input.
    pub(super) serde_skip_deserializing: bool,
    /// `#[serde(flatten)]` — merge the nested struct's schema into the parent.
    pub(super) flatten: bool,
    /// Span of the `flatten` keyword, used to anchor flatten-misuse errors.
    pub(super) flatten_span: Option<Span>,
    /// `#[tool_args(skip)]` — excluded from the schema only (deserialize intact).
    pub(super) tool_args_skip: bool,
    /// `#[tool_args(required)]` — force-include in `required` even with a default.
    pub(super) tool_args_required: bool,
    /// `#[tool_args(description = "...")]` description override, if any.
    pub(super) tool_args_description: Option<String>,
    /// `#[tool_args(schema = {...})]` full schema replacement tokens, if any.
    /// Stored as the raw `{...}` group so it can be re-emitted inside
    /// `::serde_json::json!(...)`.
    pub(super) tool_args_schema: Option<TokenStream>,
    /// `#[tool_args(additional_properties)]` — set `additionalProperties: true`
    /// on this field's object schema.
    pub(super) tool_args_additional_properties: bool,
}

/// The struct identifier together with its parsed fields, in declaration order.
pub(super) struct ParsedStruct {
    /// The annotated struct's identifier.
    pub(super) ident: syn::Ident,
    /// Container-level `#[serde(rename_all = "...")]` rule, parsed and validated
    /// at parse time so an unknown value becomes a `compile_error!`.
    pub(super) rename_all: Option<RenameRule>,
    /// Container-level `#[serde(default)]` / `#[serde(default = "fn")]` — serde
    /// fills every omitted non-flattened field from the container default, so
    /// none of those fields belongs in `required`.
    pub(super) container_default: bool,
    /// Parsed fields in source declaration order.
    pub(super) fields: Vec<ParsedField>,
}

/// The enum identifier together with its representation and variants.
pub(super) struct ParsedEnum {
    /// The annotated enum's identifier.
    pub(super) ident: syn::Ident,
    /// Doc-comment lines from the enum item itself, joined with a single space.
    pub(super) description: String,
    /// Optional `#[serde(rename_all = "...")]` rule applied to variant names.
    pub(super) rename_all: Option<RenameRule>,
    /// Optional `#[serde(rename_all_fields = "...")]` rule applied to the
    /// field names of every struct variant (a variant's own `rename_all`
    /// takes precedence).
    pub(super) rename_all_fields: Option<RenameRule>,
    /// Serde tagging representation, derived from `tag`/`content`/`untagged`.
    pub(super) representation: EnumRepresentation,
    /// Parsed variants in source declaration order.
    pub(super) variants: Vec<ParsedVariant>,
}

/// How the enum is serialised on the wire (mirrors the four serde modes).
pub(super) enum EnumRepresentation {
    /// No `#[serde(tag/content/untagged)]` attribute — string-enum if all
    /// variants are unit, otherwise an error (external tagging is rejected).
    Default,
    /// `#[serde(tag = "t")]` — internally tagged.
    InternallyTagged { tag: String },
    /// `#[serde(tag = "t", content = "c")]` — adjacently tagged.
    Adjacent { tag: String, content: String },
    /// `#[serde(untagged)]` — no discriminator.
    Untagged,
}

/// Per-variant metadata extracted from an enum.
pub(super) struct ParsedVariant {
    /// The variant identifier (used to compute the wire name).
    pub(super) ident: syn::Ident,
    /// Doc-comment lines joined with a single space; empty if none.
    pub(super) description: String,
    /// Per-variant `#[serde(rename = "...")]` override, if any.
    pub(super) rename: Option<String>,
    /// Per-variant `#[serde(rename_all = "...")]` rule for this variant's
    /// *field* names, overriding the container's `rename_all_fields`.
    pub(super) rename_all: Option<RenameRule>,
    /// The variant's payload shape (unit or struct-style named fields).
    pub(super) fields: VariantFields,
}

/// Payload shape of a single enum variant.
pub(super) enum VariantFields {
    /// `Variant` — no associated data.
    Unit,
    /// `Variant { ... }` — struct-style named fields.
    Named(Vec<ParsedField>),
}

/// Top-level dispatch between struct and enum parses.
pub(super) enum Parsed {
    Struct(ParsedStruct),
    Enum(ParsedEnum),
}

/// Parses the derive input into a [`Parsed`] value.
///
/// Returns a spanned error for any input that is not a struct with named fields
/// or an enum whose variants are unit or struct-style, and for any generic
/// item — the generated `json_schema()` is an inherent method on a concrete
/// type, so type, lifetime, and const parameters are all rejected. Empty
/// named-field structs are accepted and yield an empty field list.
pub(super) fn parse(input: &DeriveInput) -> syn::Result<Parsed> {
    if !input.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &input.generics,
            "ToolArgs does not support generic types — json_schema() describes exactly one \
             concrete schema; derive ToolArgs on a concrete type instead",
        ));
    }
    match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => {
            let serde = parse_struct_serde(&input.attrs)?;
            let fields = parse_named_fields(&named.named)?;
            Ok(Parsed::Struct(ParsedStruct {
                ident: input.ident.clone(),
                rename_all: serde.rename_all,
                container_default: serde.container_default,
                fields,
            }))
        }
        Data::Struct(_) => Err(Error::new_spanned(
            input,
            "ToolArgs only supports structs with named fields",
        )),
        Data::Enum(data) => {
            let parsed = parse_enum(input, data)?;
            Ok(Parsed::Enum(parsed))
        }
        Data::Union(_) => Err(Error::new_spanned(
            input,
            "ToolArgs does not support unions",
        )),
    }
}

/// Walks a `Punctuated<Field>` into `ParsedField`s, preserving source order.
fn parse_named_fields(
    named: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
) -> syn::Result<Vec<ParsedField>> {
    let mut fields = Vec::with_capacity(named.len());
    for field in named {
        let Some(ident) = field.ident.clone() else {
            return Err(Error::new_spanned(
                field,
                "internal: FieldsNamed contained a field without an ident",
            ));
        };
        let description = extract_doc(&field.attrs);
        let serde = parse_field_serde(&field.attrs)?;
        let tool_args = parse_field_tool_args(&field.attrs)?;
        fields.push(ParsedField {
            ident,
            ty: field.ty.clone(),
            description,
            has_default: serde.has_default,
            serde_rename: serde.rename,
            serde_skip: serde.skip,
            serde_skip_deserializing: serde.skip_deserializing,
            flatten: serde.flatten,
            flatten_span: serde.flatten_span,
            tool_args_skip: tool_args.skip,
            tool_args_required: tool_args.required,
            tool_args_description: tool_args.description,
            tool_args_schema: tool_args.schema,
            tool_args_additional_properties: tool_args.additional_properties,
        });
    }
    Ok(fields)
}

/// Reads the enum-level serde representation attrs and recurses into each
/// variant. Mixed `untagged` + `tag` would be a serde error at the user's site,
/// so we just take whichever combination the user wrote and let serde validate
/// it.
fn parse_enum(input: &DeriveInput, data: &DataEnum) -> syn::Result<ParsedEnum> {
    let description = extract_doc(&input.attrs);
    let attrs = parse_enum_serde(&input.attrs)?;

    let representation = match (attrs.tag, attrs.content, attrs.untagged) {
        (_, _, true) => EnumRepresentation::Untagged,
        (Some(tag), Some(content), false) => EnumRepresentation::Adjacent { tag, content },
        (Some(tag), None, false) => EnumRepresentation::InternallyTagged { tag },
        (None, Some(_), false) => {
            return Err(Error::new_spanned(
                input,
                "ToolArgs: #[serde(content = ...)] requires #[serde(tag = ...)]",
            ));
        }
        (None, None, false) => EnumRepresentation::Default,
    };

    let mut variants = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        variants.push(parse_variant(variant)?);
    }

    Ok(ParsedEnum {
        ident: input.ident.clone(),
        description,
        rename_all: attrs.rename_all,
        rename_all_fields: attrs.rename_all_fields,
        representation,
        variants,
    })
}

/// Extracts the fields the variant carries plus the per-variant serde
/// attributes the schema builder needs (`rename` and `rename_all`).
fn parse_variant(variant: &syn::Variant) -> syn::Result<ParsedVariant> {
    let description = extract_doc(&variant.attrs);
    let serde = parse_variant_serde(&variant.attrs)?;
    let fields = match &variant.fields {
        Fields::Unit => VariantFields::Unit,
        Fields::Named(named) => VariantFields::Named(parse_named_fields(&named.named)?),
        Fields::Unnamed(_) => {
            return Err(Error::new_spanned(
                variant,
                "ToolArgs: tuple variants are not supported — use a named-field variant",
            ));
        }
    };
    Ok(ParsedVariant {
        ident: variant.ident.clone(),
        description,
        rename: serde.rename,
        rename_all: serde.rename_all,
        fields,
    })
}

/// Collects `#[doc = "..."]` attribute values (one per `///` line), trims the
/// single leading space rustc inserts, and joins them with a single space.
fn extract_doc(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            lines.push(s.value().trim_start().to_string());
        }
    }
    lines.join(" ")
}
