//! Parses a `syn::DeriveInput` into the structured representation the schema
//! builder consumes: structs become a [`ParsedStruct`] with per-field metadata,
//! enums become a [`ParsedEnum`] with per-variant metadata. Both retain the
//! source-declaration order and the doc-comment text the builders need to keep
//! generated schemas faithful to the Rust definition.
//!
//! The set of fields is intentionally additive: NTM-003 extends the per-field
//! and per-variant records with rename / flatten / override data without
//! breaking the NTM-001 / NTM-002 surface.

use proc_macro2::{Span, TokenStream};
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Error, Expr, ExprLit, Fields, Lit, Meta,
    Token, Type,
};

use super::rename::RenameRule;

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
/// or an enum whose variants are unit or struct-style. Empty named-field
/// structs are accepted and yield an empty field list.
pub(super) fn parse(input: &DeriveInput) -> syn::Result<Parsed> {
    match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => {
            let rename_all = parse_struct_rename_all(&input.attrs)?;
            let fields = parse_named_fields(&named.named)?;
            Ok(Parsed::Struct(ParsedStruct {
                ident: input.ident.clone(),
                rename_all,
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
        representation,
        variants,
    })
}

/// Extracts the fields the variant carries plus the per-variant serde
/// attributes the schema builder needs (currently `rename`).
fn parse_variant(variant: &syn::Variant) -> syn::Result<ParsedVariant> {
    let description = extract_doc(&variant.attrs);
    let rename = parse_variant_serde(&variant.attrs)?;
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
        rename,
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

/// Field-level serde attrs that affect schema generation.
#[derive(Default)]
struct FieldSerde {
    has_default: bool,
    rename: Option<String>,
    skip: bool,
    skip_deserializing: bool,
    flatten: bool,
    flatten_span: Option<Span>,
}

/// Inspects `#[serde(...)]` attributes for the keys that affect schema
/// generation: `default` (with or without `= "fn"`), `rename`, `skip`,
/// `skip_deserializing`, and `flatten`. Unknown keys (including
/// `skip_serializing`) are ignored so that serde attributes owned by other
/// concerns pass through untouched and `Deserialize` behaviour is left intact.
fn parse_field_serde(attrs: &[Attribute]) -> syn::Result<FieldSerde> {
    let mut out = FieldSerde::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                out.has_default = true;
                if meta.input.peek(Token![=]) {
                    let _: Expr = meta.value()?.parse()?;
                }
            } else if meta.path.is_ident("rename") {
                out.rename = Some(parse_string_value(&meta)?);
            } else if meta.path.is_ident("skip") {
                out.skip = true;
            } else if meta.path.is_ident("skip_deserializing") {
                out.skip_deserializing = true;
            } else if meta.path.is_ident("flatten") {
                out.flatten = true;
                out.flatten_span = Some(meta.path.span());
            } else if meta.input.peek(Token![=]) {
                let _: Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _content;
                syn::parenthesized!(_content in meta.input);
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Field-level `#[tool_args(...)]` schema-only overrides.
#[derive(Default)]
struct FieldToolArgs {
    skip: bool,
    required: bool,
    description: Option<String>,
    schema: Option<TokenStream>,
    additional_properties: bool,
}

/// Parses `#[tool_args(schema = {...}, description = "...", skip, required,
/// additional_properties)]` on a field. Unknown keys are rejected with a
/// spanned error so typos surface at compile time rather than being silently
/// dropped.
fn parse_field_tool_args(attrs: &[Attribute]) -> syn::Result<FieldToolArgs> {
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

/// Container-level serde attrs that drive enum schema shape.
#[derive(Default)]
struct EnumSerdeAttrs {
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
    rename_all: Option<RenameRule>,
}

/// Parses `#[serde(tag = "...", content = "...", untagged, rename_all = "...")]`
/// at the enum container level. Other serde keys are skipped without erroring.
fn parse_enum_serde(attrs: &[Attribute]) -> syn::Result<EnumSerdeAttrs> {
    let mut out = EnumSerdeAttrs::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                out.tag = Some(parse_string_value(&meta)?);
            } else if meta.path.is_ident("content") {
                out.content = Some(parse_string_value(&meta)?);
            } else if meta.path.is_ident("untagged") {
                out.untagged = true;
            } else if meta.path.is_ident("rename_all") {
                let raw = parse_string_value(&meta)?;
                match RenameRule::from_str(&raw) {
                    Some(rule) => out.rename_all = Some(rule),
                    None => {
                        return Err(syn::Error::new(
                            meta.path.span(),
                            format!(
                                "ToolArgs: unsupported rename_all rule `{raw}` — expected one of \
                                 lowercase, UPPERCASE, camelCase, snake_case, PascalCase, \
                                 SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
                            ),
                        ));
                    }
                }
            } else if meta.input.peek(Token![=]) {
                let _: Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _content;
                syn::parenthesized!(_content in meta.input);
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Parses the container-level `#[serde(rename_all = "...")]` on a struct,
/// validating the value against the eight supported rules. Unlike the enum
/// path, an unrecognised rule is a hard error so the derive emits a
/// `compile_error!` instead of silently using the raw field names.
fn parse_struct_rename_all(attrs: &[Attribute]) -> syn::Result<Option<RenameRule>> {
    let mut rule = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value = meta.value()?;
                let expr: Expr = value.parse()?;
                let raw = expect_string_literal(&expr)?;
                match RenameRule::from_str(&raw) {
                    Some(parsed) => rule = Some(parsed),
                    None => {
                        return Err(Error::new_spanned(
                            &expr,
                            format!(
                                "ToolArgs: unsupported rename_all rule `{raw}` — expected one of \
                                 lowercase, UPPERCASE, camelCase, snake_case, PascalCase, \
                                 SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
                            ),
                        ));
                    }
                }
            } else if meta.input.peek(Token![=]) {
                let _: Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _content;
                syn::parenthesized!(_content in meta.input);
            }
            Ok(())
        })?;
    }
    Ok(rule)
}

/// Variant-level serde attrs the schema builder needs (currently only `rename`).
fn parse_variant_serde(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut rename = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                rename = Some(parse_string_value(&meta)?);
            } else if meta.input.peek(Token![=]) {
                let _: Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _content;
                syn::parenthesized!(_content in meta.input);
            }
            Ok(())
        })?;
    }
    Ok(rename)
}

/// Reads a `key = "value"` pair inside a `#[serde(...)]` / `#[tool_args(...)]`
/// invocation, returning the string. Errors point at the right-hand-side
/// expression when it is not a string literal.
fn parse_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let expr: Expr = meta.value()?.parse()?;
    expect_string_literal(&expr)
}

/// Extracts the value of a string-literal expression, erroring otherwise.
fn expect_string_literal(expr: &Expr) -> syn::Result<String> {
    if let Expr::Lit(ExprLit {
        lit: Lit::Str(s), ..
    }) = expr
    {
        Ok(s.value())
    } else {
        Err(Error::new_spanned(
            expr,
            "ToolArgs: expected a string literal",
        ))
    }
}
