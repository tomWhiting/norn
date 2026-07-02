#![allow(dead_code)]

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
struct UnknownKey {
    /// A documented field.
    #[tool_args(rename = "nope")]
    field: String,
}

fn main() {}
