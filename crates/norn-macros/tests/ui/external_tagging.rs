use norn_macros::ToolArgs;

#[derive(ToolArgs)]
enum ExternalTagging {
    /// Data-bearing variant without a representation attribute.
    Create {
        /// Item title.
        title: String,
    },
}

fn main() {}
