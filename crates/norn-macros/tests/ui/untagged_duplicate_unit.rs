#![allow(dead_code)]

use norn_macros::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
#[serde(untagged)]
enum DuplicateUnits {
    /// Data-bearing form.
    Data {
        /// Payload value.
        value: i64,
    },
    /// First null form.
    Nothing,
    /// Second null form.
    AlsoNothing,
}

fn main() {}
