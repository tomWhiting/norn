#![allow(dead_code)]

use norn_macros::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
#[serde(rename_all = "camel-Case")]
struct BadRule {
    /// A documented field.
    field_name: String,
}

fn main() {}
