//! Turns the parsed [`FollowUpsInput`] AST into a closure expression that
//! accepts `&::norn::tool::ToolOutput` and returns
//! `Vec<::norn::tool::follow_up::FollowUpAction>`.
//!
//! All runtime types are referenced by absolute path so the generated code
//! works in any crate that depends on `norn` and `serde_json`, regardless of
//! local `use` statements.
//!
//! Reconciliation with the brief: the live `FollowUpAction` has seven fields
//! (the brief's "4 fields" wording predates the NTF-001 type), and
//! `ExpiryCondition::FileModified` carries `content_hash` rather than the
//! brief's obsolete `modified_at`/`chrono::Utc::now()` field. Because the macro
//! only ever sees the tool output and must not compute hashes itself (the
//! hashing/comparison contract belongs to the follow-up cluster's expiry
//! checker), each stateful expiry shorthand takes the registration-time state
//! as an author-supplied expression sourced from the output: `content_hash` for
//! `FileModified`, the `(path, hash)` pairs for `AnyFileModified`, and `turn_id`
//! for `TurnScoped`. The generated code therefore populates these fields with
//! real captured values, never empty placeholders. `confidence` defaults to
//! `High` and `before_content` to `Unavailable`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Error, LitStr, Type};

use super::parse::{ActionDecl, ExpirySpec, FollowUpsInput};

/// Generates the follow-up registration closure from the parsed input.
pub(super) fn generate(input: &FollowUpsInput) -> syn::Result<TokenStream> {
    let tool_name = tool_name(&input.tool)?;

    let action_blocks = input
        .actions
        .iter()
        .map(|action| action_block(action, &tool_name))
        .collect::<Vec<_>>();

    // Name the parameter `output` so user-supplied `when`/`expires` exprs and
    // generated interpolation can reference it. When there are no actions the
    // parameter is unused, so bind it to a discard name and skip the predicate
    // helper to avoid unused-variable / dead-code warnings.
    let (param, helper) = if input.actions.is_empty() {
        (quote! { _output }, TokenStream::new())
    } else {
        // Routing each `when` closure through a typed helper forces its
        // parameter type to `&ToolOutput` (closures bound to a `let` cannot
        // infer it) without an immediate closure call (`redundant_closure_call`).
        let helper = quote! {
            fn eval_condition<F>(output: &::norn::tool::ToolOutput, predicate: F) -> bool
            where
                F: ::std::ops::Fn(&::norn::tool::ToolOutput) -> bool,
            {
                predicate(output)
            }
        };
        (quote! { output }, helper)
    };

    Ok(quote! {
        |#param: &::norn::tool::ToolOutput|
            -> ::std::vec::Vec<::norn::tool::follow_up::FollowUpAction> {
            #helper
            let mut follow_up_actions: ::std::vec::Vec<
                ::norn::tool::follow_up::FollowUpAction,
            > = ::std::vec::Vec::new();
            #(#action_blocks)*
            follow_up_actions
        }
    })
}

/// Derives the `FollowUpAction.tool` string from the final segment of the
/// declared tool type path.
fn tool_name(ty: &Type) -> syn::Result<String> {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
    {
        return Ok(segment.ident.to_string());
    }
    Err(Error::new_spanned(
        ty,
        "`tool` must be a type path (e.g. `EditTool`)",
    ))
}

/// Emits the conditional push block for a single action.
fn action_block(action: &ActionDecl, tool_name: &str) -> TokenStream {
    let name = &action.name;
    let when = &action.when;
    let description = interpolate_description(&action.description);
    let expires = lower_expiry(&action.expires);
    let args = lower_overrides(action.overrides.as_ref());
    let tool_lit = LitStr::new(tool_name, action.name.span());

    quote! {
        if eval_condition(output, #when) {
            follow_up_actions.push(::norn::tool::follow_up::FollowUpAction {
                action: ::std::string::String::from(#name),
                description: #description,
                tool: ::std::string::String::from(#tool_lit),
                args: #args,
                expires: #expires,
                confidence: ::norn::tool::follow_up::Confidence::High,
                before_content: ::norn::tool::follow_up::BeforeContentSource::Unavailable,
            });
        }
    }
}

/// Rewrites `{field}` placeholders in a description into a `format!` call that
/// pulls each field from `output.content`, falling back to `"<missing>"`.
///
/// Descriptions without placeholders are emitted as the original string
/// literal unchanged.
fn interpolate_description(lit: &LitStr) -> TokenStream {
    let raw = lit.value();
    let mut fmt = String::new();
    let mut args: Vec<TokenStream> = Vec::new();
    let mut has_placeholder = false;
    let mut chars = raw.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    fmt.push_str("{{");
                    continue;
                }
                let mut field = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    field.push(next);
                }
                if closed {
                    has_placeholder = true;
                    fmt.push_str("{}");
                    let field_lit = LitStr::new(field.trim(), lit.span());
                    args.push(quote! {
                        output.content[#field_lit].as_str().unwrap_or("<missing>")
                    });
                } else {
                    // Unterminated `{` — keep it as an escaped literal brace so
                    // the resulting format string stays valid.
                    fmt.push_str("{{");
                    fmt.push_str(&escape_braces(&field));
                }
            }
            '}' => fmt.push_str("}}"),
            other => fmt.push(other),
        }
    }

    if has_placeholder {
        quote! { ::std::format!(#fmt, #(#args),*) }
    } else {
        quote! { ::std::string::String::from(#lit) }
    }
}

/// Doubles `{` and `}` so a literal fragment is safe inside a format string.
fn escape_braces(text: &str) -> String {
    text.replace('{', "{{").replace('}', "}}")
}

/// Lowers an expiry shorthand to its absolute-path `ExpiryCondition` variant,
/// populating each variant's registration-time state from the author-supplied
/// expressions rather than empty placeholders.
fn lower_expiry(spec: &ExpirySpec) -> TokenStream {
    match spec {
        ExpirySpec::FileModified { path, content_hash } => quote! {
            ::norn::tool::follow_up::ExpiryCondition::FileModified {
                path: ::std::path::PathBuf::from(#path),
                content_hash: ::std::string::String::from(#content_hash),
            }
        },
        ExpirySpec::AnyFileModified(files) => quote! {
            ::norn::tool::follow_up::ExpiryCondition::AnyFileModified {
                files: ::std::iter::IntoIterator::into_iter(#files)
                    .map(|(path, content_hash)| (
                        ::std::path::PathBuf::from(path),
                        ::std::string::String::from(content_hash),
                    ))
                    .collect::<::std::collections::HashMap<
                        ::std::path::PathBuf,
                        ::std::string::String,
                    >>(),
            }
        },
        ExpirySpec::TurnScoped(turn_id) => quote! {
            ::norn::tool::follow_up::ExpiryCondition::TurnScoped {
                turn_id: ::std::string::String::from(#turn_id),
            }
        },
        ExpirySpec::Never => quote! {
            ::norn::tool::follow_up::ExpiryCondition::Never
        },
    }
}

/// Wraps captured override tokens in `serde_json::json!`; an absent or empty
/// override produces `json!({})`.
fn lower_overrides(overrides: Option<&TokenStream>) -> TokenStream {
    if let Some(tokens) = overrides {
        quote! { ::serde_json::json!({ #tokens }) }
    } else {
        quote! { ::serde_json::json!({}) }
    }
}
