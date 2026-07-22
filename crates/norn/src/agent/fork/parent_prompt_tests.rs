use super::*;
use crate::system_prompt::PromptAuthority;

#[test]
fn typed_parent_plan_is_captured_exactly() {
    let mut expected = PromptPlan::new();
    expected.set(PromptSource::ChildAgentPolicy, "compiled child policy");
    expected.set(PromptSource::ConfiguredVariant, "configured variant");
    let mut context = LoopContext::new("legacy view");
    context.install_stable_prompt_plan(expected.clone());

    let captured = ParentPromptPlan::from_loop_context(&context);
    assert_eq!(captured.plan(), &expected);
}

#[test]
fn legacy_parent_base_becomes_embedder_system_policy_only() -> Result<(), Box<dyn std::error::Error>>
{
    let context = LoopContext::new("explicit embedder base");
    let captured = ParentPromptPlan::from_loop_context(&context);
    let fragment = captured
        .plan()
        .fragments()
        .first()
        .ok_or_else(|| std::io::Error::other("legacy base produced no fragment"))?;

    assert_eq!(captured.plan().fragments().len(), 1);
    assert_eq!(fragment.source(), PromptSource::EmbedderPolicy);
    assert_eq!(fragment.authority(), PromptAuthority::System);
    assert_eq!(fragment.content(), "explicit embedder base");
    Ok(())
}
