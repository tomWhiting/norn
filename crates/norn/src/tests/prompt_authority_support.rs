use std::sync::Arc;

use crate::agent::AgentBuilder;
use crate::profile::{Profile, ProfileOrigin};
use crate::provider::mock::MockProvider;
use crate::provider::{Message, Provider};

pub(crate) const OPERATOR_OVERRIDE: &str = "D8-OPERATOR-OVERRIDE";
pub(crate) const WORKSPACE_PROFILE: &str = "D8-WORKSPACE-PROFILE";

pub(crate) fn root_prompt_messages() -> Result<Vec<Message>, Box<dyn std::error::Error>> {
    let workspace = tempfile::tempdir()?;
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
    let agent = AgentBuilder::new(provider)
        .profile_with_origin(
            Profile {
                model: "test-model".to_owned(),
                system_instructions: vec![WORKSPACE_PROFILE.to_owned()],
                ..Profile::default()
            },
            ProfileOrigin::WorkingDirectory,
        )
        .working_dir(workspace.path())
        .context_window_limit(64_000)
        .append_system_prompt(OPERATOR_OVERRIDE)
        .build()?;
    let parts = agent.into_parts();
    let plan = parts
        .loop_context
        .stable_prompt_plan()
        .ok_or_else(|| std::io::Error::other("root builder omitted its stable prompt plan"))?;
    Ok(plan.materialize_messages())
}
