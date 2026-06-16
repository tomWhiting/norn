//! OpenAI-compatible Chat Completions provider.

mod execute;
mod provider;
mod request;
mod sse;

pub use provider::OpenAiCompatibleProvider;
