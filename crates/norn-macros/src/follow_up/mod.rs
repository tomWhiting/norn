//! Follow-up action registration macro.
//!
//! Parses declarative `tool_follow_ups!` action definitions and generates a
//! closure that evaluates each action's `when` condition against a
//! `&ToolOutput` and constructs the matching
//! `Vec<::norn::tool::follow_up::FollowUpAction>`.

mod codegen;
mod parse;

use proc_macro2::TokenStream;

/// Expands a `tool_follow_ups!` invocation into the registration closure.
///
/// Parse and code-generation failures are surfaced as `compile_error!` token
/// streams so the compiler reports them against the offending input.
pub fn expand(input: TokenStream) -> TokenStream {
    let parsed: parse::FollowUpsInput = match syn::parse2(input) {
        Ok(value) => value,
        Err(error) => return error.into_compile_error(),
    };
    match codegen::generate(&parsed) {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}
