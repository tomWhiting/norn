//! `WebSearch` and `WebFetch` tools.

pub mod fetch;
pub mod search;
mod ssrf;

pub use self::fetch::WebFetchTool;
pub use self::search::{WEB_SEARCH_TOOL_NAME, WebSearchTool};
