//! `#[derive(ToolArgs)]` orchestration.
//!
//! Parses the derive input, dispatches to the struct or enum schema builder,
//! and surfaces any parse / validation failure as a spanned compile error
//! rather than panicking.

mod enum_schema;
mod parse;
mod rename;
mod schema;
mod validate;

use parse::Parsed;

/// Expands the `ToolArgs` derive into the generated `json_schema()` impl block.
///
/// Parse and schema-construction errors are converted to `compile_error!`
/// token streams so the compiler reports them against the offending item.
pub(crate) fn derive(input: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let derive_input: syn::DeriveInput = match syn::parse2(input) {
        Ok(value) => value,
        Err(error) => return error.into_compile_error(),
    };
    let parsed = match parse::parse(&derive_input) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_compile_error(),
    };
    if let Err(error) = validate::validate(&parsed) {
        return error.into_compile_error();
    }
    let result = match parsed {
        Parsed::Struct(parsed) => schema::build_struct_impl(&parsed),
        Parsed::Enum(parsed) => enum_schema::build_enum_impl(&parsed),
    };
    match result {
        Ok(tokens) => tokens,
        Err(error) => error.into_compile_error(),
    }
}
