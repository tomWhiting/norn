#![allow(dead_code)]

use norn_macros::ToolArgs;

#[derive(ToolArgs)]
struct Wrapper<T> {
    /// The wrapped payload.
    value: T,
}

fn main() {}
