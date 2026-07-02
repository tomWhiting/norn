//! `tool_follow_ups!` orchestration.
//!
//! Parses the declarative action definitions and dispatches to code
//! generation, surfacing any failure as a spanned compile error rather than
//! panicking.

use proc_macro2::TokenStream;

use super::{codegen, parse};

/// Expands a `tool_follow_ups!` invocation into the registration closure.
///
/// Parse and code-generation failures are surfaced as `compile_error!` token
/// streams so the compiler reports them against the offending input.
pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let parsed: parse::FollowUpsInput = match syn::parse2(input) {
        Ok(value) => value,
        Err(error) => return error.into_compile_error(),
    };
    match codegen::generate(&parsed) {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}
