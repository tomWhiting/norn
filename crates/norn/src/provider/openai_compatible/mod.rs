//! OpenAI-compatible Chat Completions provider.

mod execute;
mod provider;
mod request;
mod role_policy;
mod sse;

pub use provider::OpenAiCompatibleProvider;

#[cfg(test)]
#[path = "role_policy_integration_tests.rs"]
mod role_policy_integration_tests;
