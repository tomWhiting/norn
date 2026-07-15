//! Rust source analysis.

pub mod cargo;
pub mod modules;

mod cfg;
pub(crate) mod identifier;
mod items;
mod loc;
mod shape;
mod syntax;

pub use cfg::{CfgError, CfgTruth, evaluate_cfg};
pub use items::{RustItemProjection, RustItemProjectionError, rust_item_projections};
pub use loc::{LocError, ProductionMetrics, production_metrics};
pub use shape::{ModuleShapeKind, ModuleShapeViolation, module_shape};
pub use syntax::{RustSource, RustSourceError, SourceRange};
