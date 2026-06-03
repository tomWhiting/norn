#![allow(dead_code)]

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
struct MissingDoc {
    /// Documented field.
    documented: String,
    undocumented: String,
}

fn main() {}
