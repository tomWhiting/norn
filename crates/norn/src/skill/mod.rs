//! Skill discovery, metadata parsing, and template expansion for the
//! Norn agent runtime.
//!
//! - [`types`] — [`SkillMetadata`], [`StringOrList`], and the
//!   [`SkillEffort`] / [`SkillContext`] / [`SkillShell`] enums.
//! - [`loader`] — file discovery and frontmatter parsing
//!   ([`discover_skills`], [`load_skill_from_path`]).
//! - [`catalog`] — in-memory [`SkillCatalog`] populated by scanning the
//!   configured search directories with first-match-wins shadowing.
//! - [`template`] — three-stage skill body expansion
//!   ([`expand`], [`TemplateInputs`]).

pub mod catalog;
pub mod loader;
mod origin;
pub mod template;
pub mod types;

pub use crate::error::SkillError;
pub use catalog::{SkillCatalog, SkillPromptSections};
pub use loader::{
    DiscoveryResult, LoadedSkill, discover_skills, infer_default_name, load_skill_from_path,
    parse_skill_content,
};
pub use origin::SkillOrigin;
pub use template::{SKILL_SHELL_TIMEOUT, TemplateInputs, expand};
pub use types::{SkillContext, SkillEffort, SkillMetadata, SkillShell, StringOrList};
