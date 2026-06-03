//! Procedural macros for the `norn` tool framework.
//!
//! Provides `#[derive(ToolArgs)]`, which generates a `json_schema()` method
//! returning the JSON Schema for a tool's argument struct, and the
//! `tool_follow_ups!` function-like macro for declarative follow-up
//! registration. The generated code references runtime types from `norn`
//! and `serde_json` by absolute path; this crate links against neither.

mod follow_up;
mod tool_args;

/// Derives an inherent `json_schema() -> ::serde_json::Value` method that
/// builds the JSON Schema describing the annotated argument struct.
#[proc_macro_derive(ToolArgs, attributes(tool_args))]
pub fn tool_args(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    tool_args::derive(input.into()).into()
}

/// Generates follow-up action registration code from declarative action
/// definitions. Expands to a closure that accepts `&::norn::tool::ToolOutput`
/// and returns `Vec<::norn::tool::follow_up::FollowUpAction>`.
#[proc_macro]
pub fn tool_follow_ups(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    follow_up::expand(input.into()).into()
}
