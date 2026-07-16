//! Provider implementation for the `OpenAI` Responses API.

mod backend;
mod execute;
mod opaque_discriminator;
mod provider;
pub mod rate_limiter;
pub mod request;
pub mod response_contract;
pub mod retry_after;
pub(crate) mod schema_downlevel;
pub mod sse;
mod sse_completed_item;
mod sse_types;
pub mod tools;

pub use provider::OpenAiProvider;
