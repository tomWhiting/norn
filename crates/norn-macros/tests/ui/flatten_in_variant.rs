#![allow(dead_code)]

use norn_macros::ToolArgs;
use serde::Deserialize;

#[derive(Deserialize, ToolArgs)]
struct Pagination {
    /// Page offset.
    offset: u64,
}

#[derive(Deserialize, ToolArgs)]
#[serde(tag = "type")]
enum VariantFlatten {
    /// List command.
    List {
        /// Merged pagination.
        #[serde(flatten)]
        page: Pagination,
    },
}

fn main() {}
