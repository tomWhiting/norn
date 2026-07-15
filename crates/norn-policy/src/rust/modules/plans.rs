//! Attribute-plan projection into production and test traversal modes.

use tree_sitter::Node;

use super::analyze::{Analyzer, ReachModes};
use super::attributes::{AnalysisMode, AttributeFailure, AttributePlan, plan};
use super::model::{ModuleDiagnosticCode, ModuleTargetIdentity, SourceSpan};
use super::scan::span;
use crate::RepositoryPath;

pub(super) struct ModePlans {
    pub(super) production: Option<AttributePlan>,
    pub(super) test: Option<AttributePlan>,
    pub(super) fixture: Option<AttributePlan>,
}

impl Analyzer<'_, '_> {
    pub(super) fn inner_modes(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        parent: ReachModes,
        attributes: &[Node<'_>],
        bytes: &[u8],
    ) -> ReachModes {
        let plans = self.attribute_plans(source, target, parent, attributes, bytes);
        ReachModes {
            production: self.default_mode_without_node(source, target, plans.production.as_ref()),
            test: self.default_mode_without_node(source, target, plans.test.as_ref()),
            fixture: self.default_mode_without_node(source, target, plans.fixture.as_ref()),
        }
    }

    pub(super) fn attribute_plans(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        parent: ReachModes,
        attributes: &[Node<'_>],
        bytes: &[u8],
    ) -> ModePlans {
        let production = parent
            .production
            .then(|| plan(attributes, bytes, AnalysisMode::Production));
        let test = parent
            .test
            .then(|| plan(attributes, bytes, AnalysisMode::Test));
        let fixture = parent
            .fixture
            .then(|| plan(attributes, bytes, AnalysisMode::Production));
        ModePlans {
            production: self.finish_plan(source, target, production),
            test: self.finish_plan(source, target, test),
            fixture: self.finish_plan(source, target, fixture),
        }
    }

    pub(super) fn default_modes(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        node: Node<'_>,
        plans: &ModePlans,
    ) -> ReachModes {
        ReachModes {
            production: self.default_mode(source, target, node, plans.production.as_ref()),
            test: self.default_mode(source, target, node, plans.test.as_ref()),
            fixture: self.default_mode(source, target, node, plans.fixture.as_ref()),
        }
    }

    fn finish_plan(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        result: Option<Result<AttributePlan, AttributeFailure>>,
    ) -> Option<AttributePlan> {
        match result {
            Some(Ok(value)) if value.is_reachable() => Some(value),
            None | Some(Ok(_)) => None,
            Some(Err(error)) => {
                self.problem(
                    error.code,
                    source,
                    Some(SourceSpan::from_offsets(
                        error.offset,
                        error.offset.saturating_add(1),
                    )),
                    None,
                    Some(target.clone()),
                    None,
                );
                None
            }
        }
    }

    fn default_mode(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        node: Node<'_>,
        plan: Option<&AttributePlan>,
    ) -> bool {
        let Some(plan) = plan else {
            return false;
        };
        if plan.is_default_path() {
            true
        } else {
            self.problem(
                ModuleDiagnosticCode::AttributeUnsupported,
                source,
                Some(span(node)),
                None,
                Some(target.clone()),
                None,
            );
            false
        }
    }

    fn default_mode_without_node(
        &mut self,
        source: &RepositoryPath,
        target: &ModuleTargetIdentity,
        plan: Option<&AttributePlan>,
    ) -> bool {
        let Some(plan) = plan else {
            return false;
        };
        if plan.is_default_path() {
            true
        } else {
            self.problem(
                ModuleDiagnosticCode::AttributeUnsupported,
                source,
                None,
                None,
                Some(target.clone()),
                None,
            );
            false
        }
    }
}
