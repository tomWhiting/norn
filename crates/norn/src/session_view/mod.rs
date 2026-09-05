//! Terminal-independent retained session identities, display bodies and event reduction.

pub mod body;
mod chronology;
mod committed;
pub mod contract;
#[cfg(test)]
mod contract_tests;
pub mod error;
mod index;
mod live;
mod local;
mod projection;
#[cfg(test)]
mod projection_tests;
mod publication;
#[cfg(test)]
mod publication_tests;
mod response;
mod tools;

pub use body::{BodyOrigin, BodyRange, BodyRef, BodyRepresentation, DisplayField, DisplayText};
pub(crate) use committed::project_committed;
pub use contract::*;
pub use error::ViewError;
pub use projection::{LiveReduction, ProvisionalBodyChunk, SessionProjection};
pub use tools::{ToolState, ToolView};
