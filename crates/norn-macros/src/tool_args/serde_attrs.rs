//! `#[serde(...)]` attribute parsing shared by the struct and enum paths.
//!
//! Only the keys that change the generated schema are interpreted; every other
//! serde key is skipped untouched so attributes owned by serde's own derive
//! pass through and `Deserialize` behaviour is left intact. The rename family
//! (`rename`, `rename_all`, `rename_all_fields`) accepts both the `= "..."`
//! form and serde's split `(serialize = "...", deserialize = "...")` form.
//! Generated schemas describe model *input* — the deserialize direction — so
//! the deserialize side is the one that lands in the schema and a
//! serialize-only spec contributes nothing.

use proc_macro2::Span;
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{Attribute, Error, Expr, ExprLit, Lit, Token};

use super::rename::RenameRule;

/// Field-level serde attrs that affect schema generation.
#[derive(Default)]
pub(super) struct FieldSerde {
    /// `#[serde(default)]` or `#[serde(default = "fn")]` on the field.
    pub(super) has_default: bool,
    /// Deserialize-side `#[serde(rename = "...")]` wire name, if any.
    pub(super) rename: Option<String>,
    /// `#[serde(skip)]`.
    pub(super) skip: bool,
    /// `#[serde(skip_deserializing)]`.
    pub(super) skip_deserializing: bool,
    /// `#[serde(flatten)]`.
    pub(super) flatten: bool,
    /// Span of the `flatten` keyword, used to anchor flatten-misuse errors.
    pub(super) flatten_span: Option<Span>,
}

/// Inspects `#[serde(...)]` attributes for the keys that affect schema
/// generation: `default` (with or without `= "fn"`), `rename`, `skip`,
/// `skip_deserializing`, and `flatten`. Unknown keys (including
/// `skip_serializing`) are ignored.
pub(super) fn parse_field_serde(attrs: &[Attribute]) -> syn::Result<FieldSerde> {
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
                if let Some((name, _)) = parse_de_side_value(&meta)? {
                    out.rename = Some(name);
                }
            } else if meta.path.is_ident("skip") {
                out.skip = true;
            } else if meta.path.is_ident("skip_deserializing") {
                out.skip_deserializing = true;
            } else if meta.path.is_ident("flatten") {
                out.flatten = true;
                out.flatten_span = Some(meta.path.span());
            } else {
                skip_unknown_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Container-level serde attrs on a struct that affect schema generation.
#[derive(Default)]
pub(super) struct StructSerde {
    /// Deserialize-side `#[serde(rename_all = "...")]` rule for field names.
    pub(super) rename_all: Option<RenameRule>,
    /// Container `#[serde(default)]` / `#[serde(default = "fn")]` — serde
    /// fills every omitted non-flattened field from the container default, so
    /// no such field belongs in `required`.
    pub(super) container_default: bool,
}

/// Parses `#[serde(rename_all = "...", default)]` at the struct container
/// level. An unrecognised `rename_all` rule is a hard error so the derive
/// emits a `compile_error!` instead of silently using the raw field names.
pub(super) fn parse_struct_serde(attrs: &[Attribute]) -> syn::Result<StructSerde> {
    let mut out = StructSerde::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if let Some(rule) = parse_rename_rule(&meta)? {
                    out.rename_all = Some(rule);
                }
            } else if meta.path.is_ident("default") {
                out.container_default = true;
                if meta.input.peek(Token![=]) {
                    let _: Expr = meta.value()?.parse()?;
                }
            } else {
                skip_unknown_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Container-level serde attrs that drive enum schema shape.
#[derive(Default)]
pub(super) struct EnumSerde {
    /// `#[serde(tag = "...")]`.
    pub(super) tag: Option<String>,
    /// `#[serde(content = "...")]`.
    pub(super) content: Option<String>,
    /// `#[serde(untagged)]`.
    pub(super) untagged: bool,
    /// `#[serde(rename_all = "...")]` — applies to *variant* names.
    pub(super) rename_all: Option<RenameRule>,
    /// `#[serde(rename_all_fields = "...")]` — applies to the field names of
    /// every struct variant (overridden by a variant's own `rename_all`).
    pub(super) rename_all_fields: Option<RenameRule>,
}

/// Parses `#[serde(tag = "...", content = "...", untagged, rename_all = "...",
/// rename_all_fields = "...")]` at the enum container level. Other serde keys
/// are skipped without erroring.
pub(super) fn parse_enum_serde(attrs: &[Attribute]) -> syn::Result<EnumSerde> {
    let mut out = EnumSerde::default();
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
                if let Some(rule) = parse_rename_rule(&meta)? {
                    out.rename_all = Some(rule);
                }
            } else if meta.path.is_ident("rename_all_fields") {
                if let Some(rule) = parse_rename_rule(&meta)? {
                    out.rename_all_fields = Some(rule);
                }
            } else {
                skip_unknown_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Variant-level serde attrs the schema builder needs.
#[derive(Default)]
pub(super) struct VariantSerde {
    /// Deserialize-side `#[serde(rename = "...")]` for the variant name.
    pub(super) rename: Option<String>,
    /// `#[serde(rename_all = "...")]` — applies to this variant's *field*
    /// names, overriding the container's `rename_all_fields`.
    pub(super) rename_all: Option<RenameRule>,
}

/// Parses `#[serde(rename = "...", rename_all = "...")]` on an enum variant.
/// Other serde keys are skipped without erroring.
pub(super) fn parse_variant_serde(attrs: &[Attribute]) -> syn::Result<VariantSerde> {
    let mut out = VariantSerde::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                if let Some((name, _)) = parse_de_side_value(&meta)? {
                    out.rename = Some(name);
                }
            } else if meta.path.is_ident("rename_all") {
                if let Some(rule) = parse_rename_rule(&meta)? {
                    out.rename_all = Some(rule);
                }
            } else {
                skip_unknown_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(out)
}

/// Parses a `rename_all` / `rename_all_fields` value into a validated
/// [`RenameRule`], accepting both attribute forms. Returns `None` when the
/// spec only covers the serialize direction; an unrecognised rule name is a
/// hard error anchored at the offending string literal.
fn parse_rename_rule(meta: &ParseNestedMeta<'_>) -> syn::Result<Option<RenameRule>> {
    let Some((raw, span)) = parse_de_side_value(meta)? else {
        return Ok(None);
    };
    match RenameRule::from_str(&raw) {
        Some(rule) => Ok(Some(rule)),
        None => Err(Error::new(
            span,
            format!(
                "ToolArgs: unsupported rename_all rule `{raw}` — expected one of \
                 lowercase, UPPERCASE, camelCase, snake_case, PascalCase, \
                 SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
            ),
        )),
    }
}

/// Reads the value of a rename-family serde key, which serde accepts as either
/// `key = "value"` or `key(serialize = "...", deserialize = "...")`. Returns
/// the deserialize-side string and its span; `None` when only a serialize-side
/// value is given (the deserialize name then stays the raw ident).
fn parse_de_side_value(meta: &ParseNestedMeta<'_>) -> syn::Result<Option<(String, Span)>> {
    if meta.input.peek(Token![=]) {
        let expr: Expr = meta.value()?.parse()?;
        let lit = expect_string_literal(&expr)?;
        return Ok(Some((lit.value(), lit.span())));
    }
    if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let mut de_side = None;
        while !content.is_empty() {
            let key: syn::Ident = content.parse()?;
            content.parse::<Token![=]>()?;
            let lit: syn::LitStr = content.parse()?;
            if key == "deserialize" {
                de_side = Some((lit.value(), lit.span()));
            } else if key != "serialize" {
                return Err(Error::new(
                    key.span(),
                    "ToolArgs: expected `serialize` or `deserialize` inside a split \
                     rename attribute",
                ));
            }
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        return Ok(de_side);
    }
    Err(meta
        .error("ToolArgs: expected `= \"...\"` or `(serialize = \"...\", deserialize = \"...\")`"))
}

/// Consumes and discards the value of an unrecognised serde key so nested-meta
/// parsing can continue: `key = <expr>` and `key(...)` are both drained; a
/// bare key needs no consumption.
fn skip_unknown_value(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        let _: Expr = meta.value()?.parse()?;
    } else if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let _: proc_macro2::TokenStream = content.parse()?;
    }
    Ok(())
}

/// Reads a `key = "value"` pair inside a `#[serde(...)]` / `#[tool_args(...)]`
/// invocation, returning the string. Errors point at the right-hand-side
/// expression when it is not a string literal.
pub(super) fn parse_string_value(meta: &ParseNestedMeta<'_>) -> syn::Result<String> {
    let expr: Expr = meta.value()?.parse()?;
    Ok(expect_string_literal(&expr)?.value())
}

/// Extracts the string literal behind an expression, erroring otherwise. The
/// literal (rather than its value) is returned so callers can anchor
/// diagnostics at the exact token.
fn expect_string_literal(expr: &Expr) -> syn::Result<&syn::LitStr> {
    if let Expr::Lit(ExprLit {
        lit: Lit::Str(s), ..
    }) = expr
    {
        Ok(s)
    } else {
        Err(Error::new_spanned(
            expr,
            "ToolArgs: expected a string literal",
        ))
    }
}
