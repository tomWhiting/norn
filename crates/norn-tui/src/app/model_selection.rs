//! Validated model policy transitions and retained notices after accepted publication.

use norn::agent_loop::config::AgentLoopConfig;
use norn::agent_loop::loop_context::LoopContext;
use norn::agent_loop::{ServiceTierCommand, parse_service_tier_command};
use norn::error::ConfigError;
use norn::model_selection::ModelRuntime;
use norn::provider::request::{ReasoningEffort, ServiceTier};
use norn::tool::context::ToolContext;

use crate::TuiError;
use crate::render::fixed_panel::StatusBar;

use super::dispatch::write_error_line;
use super::event_loop::RuntimeRefs;
use super::slash::write_dim_line;
use super::slash_catalog::{EffortCommand, effort_label, parse_effort_command};
use super::state::AppState;

#[derive(Clone, Copy)]
enum SelectionChange<'a> {
    Model(&'a str),
    Effort(Option<ReasoningEffort>),
    Tier(Option<ServiceTier>),
}

struct PublicationTarget<'a> {
    model: &'a mut String,
    config: &'a mut AgentLoopConfig,
    context: &'a mut LoopContext,
    tools: Option<&'a ToolContext>,
    status: &'a mut StatusBar,
}

struct ClearedPolicy {
    effort: Option<ReasoningEffort>,
    tier: Option<ServiceTier>,
}

/// Prepare every fallible change before publishing any driver-owned value.
fn apply_change(
    current: &mut ModelRuntime,
    target: &mut PublicationTarget<'_>,
    change: SelectionChange<'_>,
) -> Result<ClearedPolicy, ConfigError> {
    let prepared = match change {
        SelectionChange::Model(model) => current.prepare(model)?,
        SelectionChange::Effort(effort) => {
            let mut prepared = current.clone();
            prepared.set_effort(effort)?;
            prepared
        }
        SelectionChange::Tier(tier) => {
            let mut prepared = current.clone();
            prepared.set_tier(tier)?;
            prepared
        }
    };
    let cleared = ClearedPolicy {
        effort: current.effort().filter(|_| prepared.effort().is_none()),
        tier: current.tier().filter(|_| prepared.tier().is_none()),
    };
    prepared.apply(target.config, target.context, target.tools);
    prepared.model().clone_into(target.model);
    target.status.model_name.clone_from(target.model);
    target.status.reasoning_effort = prepared.effort().map(|effort| effort.as_str().to_owned());
    target.status.service_tier = prepared.tier().map(|tier| tier.as_str().to_owned());
    *current = prepared;
    Ok(cleared)
}

fn apply_runtime_change(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    change: SelectionChange<'_>,
) -> Result<ClearedPolicy, ConfigError> {
    apply_change(
        &mut runtime.model_selection,
        &mut PublicationTarget {
            model: &mut runtime.model,
            config: &mut runtime.agent_config,
            context: &mut runtime.loop_context,
            tools: runtime.executor.shared_context().as_deref(),
            status: state.fixed_panel.status_bar_mut(),
        },
        change,
    )
}

pub(super) fn handle_model(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    arg: &str,
) -> Result<(), TuiError> {
    let name = arg.trim();
    if name.is_empty() {
        return write_dim_line("usage: /model <name>", state);
    }
    let cleared = match apply_runtime_change(state, runtime, SelectionChange::Model(name)) {
        Ok(cleared) => cleared,
        Err(error) => return write_error_line(state, &format!("/model failed: {error}")),
    };
    state.transcript.model_changed()?;
    let mut line = format!("Switched model to {}", runtime.model);
    if let Some(effort) = cleared.effort {
        line.push_str("; cleared unsupported effort '");
        line.push_str(effort.as_str());
        line.push('\'');
    }
    if let Some(tier) = cleared.tier {
        line.push_str("; cleared unsupported tier '");
        line.push_str(tier.as_str());
        line.push('\'');
    }
    write_dim_line(&line, state)
}

pub(super) fn handle_reasoning_effort(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    arg: &str,
) -> Result<(), TuiError> {
    let value = arg.trim();
    if value.is_empty() {
        return write_dim_line(
            runtime
                .model_selection
                .effort()
                .map_or("default", effort_label),
            state,
        );
    }
    let effort = match parse_effort_command(value) {
        Some(EffortCommand::Set(effort)) => Some(effort),
        Some(EffortCommand::Clear) => None,
        None => {
            return write_error_line(
                state,
                &format!(
                    "/effort: invalid reasoning effort '{value}'; expected low, medium, high, xhigh, max, ultra, or default"
                ),
            );
        }
    };
    if let Err(error) = apply_runtime_change(state, runtime, SelectionChange::Effort(effort)) {
        return write_error_line(state, &format!("/effort failed: {error}"));
    }
    state.transcript.model_changed()?;
    match effort {
        Some(effort) => write_dim_line(
            &format!("Reasoning effort: {}", effort_label(effort)),
            state,
        ),
        None => write_dim_line("Reasoning effort cleared.", state),
    }
}

pub(super) fn handle_service_tier(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
    arg: &str,
) -> Result<(), TuiError> {
    let value = arg.trim();
    if value.is_empty() {
        return write_dim_line(
            runtime
                .model_selection
                .tier()
                .map_or("none", |tier| tier.as_str()),
            state,
        );
    }
    match parse_service_tier_command(value) {
        Some(ServiceTierCommand::Fast) => set_fast_service_tier(state, runtime),
        Some(ServiceTierCommand::Clear) => {
            if let Err(error) = apply_runtime_change(state, runtime, SelectionChange::Tier(None)) {
                return write_error_line(state, &format!("/service-tier failed: {error}"));
            }
            state.transcript.model_changed()?;
            write_dim_line("Service tier cleared.", state)
        }
        None => write_error_line(
            state,
            &format!("/service-tier: invalid service tier '{value}'; expected fast or none"),
        ),
    }
}

pub(super) fn set_fast_service_tier(
    state: &mut AppState,
    runtime: &mut RuntimeRefs,
) -> Result<(), TuiError> {
    if let Err(error) = apply_runtime_change(
        state,
        runtime,
        SelectionChange::Tier(Some(ServiceTier::Fast)),
    ) {
        return write_error_line(state, &format!("/service-tier failed: {error}"));
    }
    state.transcript.model_changed()?;
    write_dim_line("Service tier: fast", state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn::model_selection::CatalogBackend;
    use norn::tool::output_budget::ToolOutputBudget;
    use norn::tools::agent::AgentModel;
    use std::collections::BTreeMap;

    struct Fixture {
        selection: ModelRuntime,
        model: String,
        config: AgentLoopConfig,
        context: LoopContext,
        tools: ToolContext,
        status: StatusBar,
    }

    impl Fixture {
        fn new(explicit: Option<u64>) -> Result<Self, ConfigError> {
            let selection = ModelRuntime::new(
                Some(CatalogBackend::CODEX),
                "sol",
                explicit,
                Some(ReasoningEffort::Max),
                Some(ServiceTier::Fast),
                BTreeMap::new(),
            )?;
            let mut fixture = Self {
                selection,
                model: String::new(),
                config: AgentLoopConfig::default(),
                context: LoopContext::new("preserved instruction"),
                tools: ToolContext::empty(),
                status: StatusBar {
                    session_name: "preserved session".to_owned(),
                    ..StatusBar::default()
                },
            };
            fixture.change(SelectionChange::Model("sol"))?;
            Ok(fixture)
        }

        fn change(&mut self, change: SelectionChange<'_>) -> Result<ClearedPolicy, ConfigError> {
            apply_change(
                &mut self.selection,
                &mut PublicationTarget {
                    model: &mut self.model,
                    config: &mut self.config,
                    context: &mut self.context,
                    tools: Some(&self.tools),
                    status: &mut self.status,
                },
                change,
            )
        }

        fn assert_published(&self) {
            assert_eq!(self.model, self.selection.model());
            assert_eq!(self.status.model_name, self.model);
            assert_eq!(
                self.config.context_window_limit,
                Some(self.selection.window())
            );
            assert_eq!(self.context.reasoning_effort, self.selection.effort());
            assert_eq!(self.context.service_tier, self.selection.tier());
            assert_eq!(
                self.status.reasoning_effort.as_deref(),
                self.selection.effort().map(ReasoningEffort::as_str)
            );
            assert_eq!(
                self.status.service_tier.as_deref(),
                self.selection.tier().map(ServiceTier::as_str)
            );
            assert_eq!(
                self.tools
                    .get_extension::<AgentModel>()
                    .map(|stamp| (stamp.model.clone(), stamp.reasoning_effort)),
                Some((self.model.clone(), self.selection.effort()))
            );
            assert_eq!(
                self.tools.get_extension::<ToolOutputBudget>().as_deref(),
                Some(&ToolOutputBudget::for_context_window(Some(
                    self.selection.window()
                )))
            );
            assert_eq!(self.status.session_name, "preserved session");
            assert_eq!(
                self.context.base_system_instruction(),
                "preserved instruction"
            );
        }
    }

    #[test]
    fn model_transition_publishes_derived_budget_and_status() -> Result<(), ConfigError> {
        let mut fixture = Fixture::new(None)?;
        let cleared = fixture.change(SelectionChange::Model("gpt-5.5"))?;
        assert_eq!(cleared.effort, Some(ReasoningEffort::Max));
        assert_eq!(cleared.tier, None);
        fixture.assert_published();
        fixture.change(SelectionChange::Model("codex-spark"))?;
        assert_eq!(fixture.selection.window(), 128_000);
        assert_eq!(fixture.selection.tier(), None);
        fixture.assert_published();
        fixture.change(SelectionChange::Model("astra"))?;
        assert_eq!(fixture.selection.window(), 372_000);
        fixture.assert_published();
        Ok(())
    }

    #[test]
    fn rejected_preparation_preserves_every_published_value() -> Result<(), ConfigError> {
        let mut fixture = Fixture::new(Some(272_000))?;
        assert!(
            fixture
                .change(SelectionChange::Model("codex-spark"))
                .is_err()
        );
        assert_eq!(fixture.model, "gpt-5.6-sol");
        assert_eq!(fixture.selection.effort(), Some(ReasoningEffort::Max));
        assert_eq!(fixture.selection.tier(), Some(ServiceTier::Fast));
        assert_eq!(fixture.selection.explicit_window(), Some(272_000));
        fixture.assert_published();
        Ok(())
    }

    #[test]
    fn compaction_refusal_preserves_published_state_and_effective_reserve()
    -> Result<(), ConfigError> {
        let mut fixture = Fixture::new(None)?;
        fixture.config.auto_compact_reserve_tokens = Some(150_000);
        fixture
            .selection
            .bind_compaction_reserve(fixture.config.auto_compact_reserve_tokens);
        assert!(
            fixture
                .change(SelectionChange::Model("codex-spark"))
                .is_err()
        );
        assert_eq!(fixture.model, "gpt-5.6-sol");
        assert_eq!(fixture.selection.effort(), Some(ReasoningEffort::Max));
        assert_eq!(fixture.selection.tier(), Some(ServiceTier::Fast));
        assert_eq!(fixture.config.auto_compact_reserve_tokens, Some(150_000));
        fixture.assert_published();
        fixture.change(SelectionChange::Model("astra"))?;
        assert_eq!(fixture.config.auto_compact_reserve_tokens, Some(150_000));
        fixture.assert_published();

        let mut compatible = Fixture::new(None)?;
        compatible.config.auto_compact_reserve_tokens = Some(30_000);
        compatible
            .selection
            .bind_compaction_reserve(compatible.config.auto_compact_reserve_tokens);
        compatible.change(SelectionChange::Model("codex-spark"))?;
        assert_eq!(compatible.selection.window(), 128_000);
        assert_eq!(compatible.config.auto_compact_reserve_tokens, Some(30_000));
        compatible.assert_published();
        Ok(())
    }

    #[test]
    fn effort_and_tier_changes_republish_child_stamp_and_display() -> Result<(), ConfigError> {
        let mut fixture = Fixture::new(None)?;
        fixture.change(SelectionChange::Effort(Some(ReasoningEffort::Ultra)))?;
        fixture.assert_published();
        fixture.change(SelectionChange::Tier(None))?;
        fixture.assert_published();
        fixture.change(SelectionChange::Tier(Some(ServiceTier::Fast)))?;
        fixture.assert_published();
        fixture.change(SelectionChange::Effort(None))?;
        fixture.assert_published();
        fixture.change(SelectionChange::Model("luna"))?;
        fixture.change(SelectionChange::Effort(Some(ReasoningEffort::High)))?;
        assert!(
            fixture
                .change(SelectionChange::Effort(Some(ReasoningEffort::Ultra)))
                .is_err()
        );
        assert_eq!(fixture.selection.effort(), Some(ReasoningEffort::High));
        fixture.assert_published();
        fixture.change(SelectionChange::Model("codex-spark"))?;
        assert!(
            fixture
                .change(SelectionChange::Tier(Some(ServiceTier::Fast)))
                .is_err()
        );
        assert_eq!(fixture.selection.tier(), None);
        fixture.assert_published();
        Ok(())
    }
}
