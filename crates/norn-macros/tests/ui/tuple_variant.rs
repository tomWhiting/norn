#![allow(dead_code)]

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
enum TupleVariant {
    /// A pair payload.
    Pair(String, u32),
}

fn main() {}
