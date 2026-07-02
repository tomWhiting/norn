#![allow(dead_code)]

use std::collections::HashMap;

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
struct NonStringKey {
    /// Counters keyed by numeric id.
    counters: HashMap<u32, u64>,
}

fn main() {}
