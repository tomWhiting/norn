#![allow(dead_code)]

use norn_macros::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
struct BadFlatten {
    /// A scalar that cannot be flattened.
    #[serde(flatten)]
    inner: String,
}

fn main() {}
