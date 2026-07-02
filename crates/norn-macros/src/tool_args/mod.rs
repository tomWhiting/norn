//! `#[derive(ToolArgs)]` — JSON Schema generation for tool argument types.

mod derive;
mod enum_schema;
mod parse;
mod rename;
mod schema;
mod serde_attrs;
mod tool_attrs;
mod validate;

pub(crate) use derive::derive;
