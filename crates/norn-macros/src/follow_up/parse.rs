//! Parses the `tool_follow_ups!` macro input into a structured AST the code
//! generator consumes.
//!
//! Grammar (order-independent top-level keys):
//!
//! ```text
//! tool_follow_ups! {
//!     tool: EditTool,
//!     actions: [
//!         {
//!             name: "undo",
//!             description: "Revert {path} to pre-edit content",
//!             when: |output| output.content["committed"].as_bool().unwrap_or(false),
//!             expires: FileModified(
//!                 output.content["path"].as_str().unwrap_or(""),
//!                 output.content["content_hash"].as_str().unwrap_or(""),
//!             ),
//!             overrides: { "mode": "structural" },
//!         },
//!     ],
//! }
//! ```
//!
//! Each stateful expiry shorthand carries the registration-time state it needs,
//! sourced from the tool output by the author. `FileModified(path, hash)` takes
//! the watched path and the content hash captured by the tool; `AnyFileModified`
//! and `TurnScoped` likewise take the file set and turn identifier. The hash and
//! file-state values themselves are produced by the registering tool (the
//! comparison logic lives in the follow-up cluster); the macro only places them
//! into the generated `ExpiryCondition`.
//!
//! The parser only recognises syntax and preserves spans / raw expressions;
//! semantic expansion (interpolation, expiry lowering, overrides) lives in
//! `codegen.rs`.

use proc_macro2::TokenStream;
use syn::parse::{Parse, ParseStream};
use syn::{Error, Expr, Ident, LitStr, Token, Type, braced, bracketed, parenthesized};

/// The whole macro input: the target tool type path plus the ordered actions.
pub(super) struct FollowUpsInput {
    /// Type path of the tool these follow-ups belong to. Only the final path
    /// segment is used (stringified into `FollowUpAction.tool`); the type is
    /// never referenced by the generated code, so it need not be in scope.
    pub(super) tool: Type,
    /// Action declarations in source order.
    pub(super) actions: Vec<ActionDecl>,
}

/// A single declared follow-up action.
pub(super) struct ActionDecl {
    /// `name:` — the action identifier (`FollowUpAction.action`).
    pub(super) name: LitStr,
    /// `description:` — may contain `{field}` interpolation placeholders.
    pub(super) description: LitStr,
    /// `when:` — a closure `|output| -> bool` evaluated against `&ToolOutput`.
    pub(super) when: Expr,
    /// `expires:` — expiry shorthand lowered to an `ExpiryCondition` variant.
    pub(super) expires: ExpirySpec,
    /// `overrides:` — raw token tree captured from the braced JSON-like body,
    /// re-emitted inside `serde_json::json!({ ... })`. `None` when omitted.
    pub(super) overrides: Option<TokenStream>,
}

/// Expiry shorthand variants, mirroring `ExpiryCondition` variant names.
///
/// Every variant that carries registration-time state takes that state as an
/// expression the author sources from the tool output, so the generated
/// `ExpiryCondition` is populated with real captured values rather than empty
/// placeholders. `Never` carries no state and stays bare.
pub(super) enum ExpirySpec {
    /// `FileModified(path_expr, content_hash_expr)` — the watched path and the
    /// content hash the tool captured at registration.
    FileModified {
        /// Expression yielding the watched file path.
        path: Expr,
        /// Expression yielding the content hash recorded at registration.
        content_hash: Expr,
    },
    /// `AnyFileModified(files_expr)` — `files_expr` yields an iterator of
    /// `(path, content_hash)` pairs captured at registration.
    AnyFileModified(Expr),
    /// `TurnScoped(turn_id_expr)` — `turn_id_expr` yields the scoping turn
    /// identifier captured at registration.
    TurnScoped(Expr),
    /// `Never` — bare shorthand; the action never expires.
    Never,
}

impl Parse for FollowUpsInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut tool: Option<Type> = None;
        let mut actions: Option<Vec<ActionDecl>> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "tool" => {
                    if tool.is_some() {
                        return Err(Error::new(key.span(), "duplicate `tool` field"));
                    }
                    tool = Some(input.parse()?);
                }
                "actions" => {
                    if actions.is_some() {
                        return Err(Error::new(key.span(), "duplicate `actions` field"));
                    }
                    let inner;
                    bracketed!(inner in input);
                    let parsed = inner.parse_terminated(ActionDecl::parse, Token![,])?;
                    actions = Some(parsed.into_iter().collect());
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown top-level field `{other}`; expected `tool` or `actions`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let tool = tool.ok_or_else(|| input.error("missing required `tool` field"))?;
        let actions = actions.ok_or_else(|| input.error("missing required `actions` field"))?;
        Ok(Self { tool, actions })
    }
}

impl Parse for ActionDecl {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let content;
        braced!(content in input);

        let mut name: Option<LitStr> = None;
        let mut description: Option<LitStr> = None;
        let mut when: Option<Expr> = None;
        let mut expires: Option<ExpirySpec> = None;
        let mut overrides: Option<TokenStream> = None;

        while !content.is_empty() {
            let key: Ident = content.parse()?;
            content.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "name" => {
                    if name.is_some() {
                        return Err(Error::new(key.span(), "duplicate `name` field"));
                    }
                    name = Some(content.parse()?);
                }
                "description" => {
                    if description.is_some() {
                        return Err(Error::new(key.span(), "duplicate `description` field"));
                    }
                    description = Some(content.parse()?);
                }
                "when" => {
                    if when.is_some() {
                        return Err(Error::new(key.span(), "duplicate `when` field"));
                    }
                    when = Some(content.parse()?);
                }
                "expires" => {
                    if expires.is_some() {
                        return Err(Error::new(key.span(), "duplicate `expires` field"));
                    }
                    expires = Some(parse_expiry(&content)?);
                }
                "overrides" => {
                    if overrides.is_some() {
                        return Err(Error::new(key.span(), "duplicate `overrides` field"));
                    }
                    let braces;
                    braced!(braces in content);
                    overrides = Some(braces.parse()?);
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!(
                            "unknown action field `{other}`; expected `name`, `description`, \
                             `when`, `expires`, or `overrides`"
                        ),
                    ));
                }
            }
            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }

        let name = name.ok_or_else(|| content.error("action missing required `name` field"))?;
        let description = description
            .ok_or_else(|| content.error("action missing required `description` field"))?;
        let when = when.ok_or_else(|| content.error("action missing required `when` field"))?;
        let expires =
            expires.ok_or_else(|| content.error("action missing required `expires` field"))?;

        Ok(Self {
            name,
            description,
            when,
            expires,
            overrides,
        })
    }
}

/// Parses the `expires:` value as one of the four expiry shorthands.
fn parse_expiry(input: ParseStream<'_>) -> syn::Result<ExpirySpec> {
    let ident: Ident = input.parse()?;
    match ident.to_string().as_str() {
        "FileModified" => {
            let inner;
            parenthesized!(inner in input);
            let path: Expr = inner.parse()?;
            inner.parse::<Token![,]>().map_err(|_err| {
                Error::new(
                    ident.span(),
                    "`FileModified` takes two arguments: \
                     `FileModified(path, content_hash)`",
                )
            })?;
            let content_hash: Expr = inner.parse()?;
            consume_optional_trailing_comma(&inner)?;
            reject_trailing_tokens(&inner, &ident, "FileModified", "(path, content_hash)")?;
            Ok(ExpirySpec::FileModified { path, content_hash })
        }
        "AnyFileModified" => {
            let inner;
            parenthesized!(inner in input);
            let files: Expr = inner.parse()?;
            consume_optional_trailing_comma(&inner)?;
            reject_trailing_tokens(&inner, &ident, "AnyFileModified", "(files)")?;
            Ok(ExpirySpec::AnyFileModified(files))
        }
        "TurnScoped" => {
            let inner;
            parenthesized!(inner in input);
            let turn_id: Expr = inner.parse()?;
            consume_optional_trailing_comma(&inner)?;
            reject_trailing_tokens(&inner, &ident, "TurnScoped", "(turn_id)")?;
            Ok(ExpirySpec::TurnScoped(turn_id))
        }
        "Never" => Ok(ExpirySpec::Never),
        other => Err(Error::new(
            ident.span(),
            format!(
                "unknown expiry shorthand `{other}`; expected \
                 `FileModified(path, content_hash)`, `AnyFileModified(files)`, \
                 `TurnScoped(turn_id)`, or `Never`"
            ),
        )),
    }
}

/// Consumes a single optional trailing comma inside an expiry shorthand's
/// argument list so `FileModified(p, h,)` parses like `FileModified(p, h)`.
fn consume_optional_trailing_comma(inner: ParseStream<'_>) -> syn::Result<()> {
    if inner.peek(Token![,]) {
        inner.parse::<Token![,]>()?;
    }
    Ok(())
}

/// Errors when an expiry shorthand carries more tokens than its grammar allows,
/// pointing the diagnostic at the shorthand name with the expected argument
/// form.
fn reject_trailing_tokens(
    inner: ParseStream<'_>,
    ident: &Ident,
    shorthand: &str,
    expected_args: &str,
) -> syn::Result<()> {
    if inner.is_empty() {
        return Ok(());
    }
    Err(Error::new(
        ident.span(),
        format!("`{shorthand}` takes exactly the arguments `{shorthand}{expected_args}`"),
    ))
}
