#![allow(dead_code)]

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
struct UnsupportedType {
    /// Destination path.
    output: std::path::PathBuf,
}

fn main() {}
