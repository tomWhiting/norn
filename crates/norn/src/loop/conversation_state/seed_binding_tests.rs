use super::test_support::{config, message};
use super::{ConversationRequestState, ResponseThreadAnchor};
use crate::error::ProviderError;
use crate::r#loop::config::ConversationStateMode;
use crate::provider::request::MessageRole;
use crate::provider::tools::ProviderCapabilities;
use crate::system_prompt::{PromptPlan, PromptSeedFingerprint, PromptSource};

fn seed(fragments: &[(PromptSource, &str)]) -> PromptSeedFingerprint {
    let mut plan = PromptPlan::new();
    for (source, content) in fragments {
        plan.set(*source, *content);
    }
    PromptSeedFingerprint::from_plan(&plan)
}

#[test]
fn matching_v2_anchor_sends_system_and_post_anchor_delta_only() -> Result<(), ProviderError> {
    let prompt_seed = seed(&[
        (PromptSource::OperatorProfile, "operator"),
        (PromptSource::WorkspaceProfile, "repository"),
    ]);
    let state = ConversationRequestState::with_prompt_seed(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        3,
        prompt_seed,
        Some(ResponseThreadAnchor::for_test_with_prompt_seed(
            "resp_old".to_owned(),
            4,
            prompt_seed,
        )),
    )?;
    let messages = vec![
        message(MessageRole::System, "product"),
        message(MessageRole::Developer, "operator"),
        message(MessageRole::User, "repository"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new task"),
    ];

    let request = state.request_messages(&messages);

    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert_eq!(request.len(), 2);
    assert_eq!(request[0].role, MessageRole::System);
    assert_eq!(request[0].content.as_deref(), Some("product"));
    assert_eq!(request[1].content.as_deref(), Some("new task"));
    Ok(())
}

#[test]
fn v1_anchor_bootstraps_full_non_system_seed_once() -> Result<(), ProviderError> {
    let prompt_seed = seed(&[(PromptSource::OperatorProfile, "operator")]);
    let mut state = ConversationRequestState::with_prompt_seed(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        2,
        prompt_seed,
        Some(ResponseThreadAnchor::for_test("resp_v1".to_owned(), 3)),
    )?;
    let mut messages = vec![
        message(MessageRole::System, "product"),
        message(MessageRole::Developer, "operator"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new task"),
    ];

    let bootstrap = state.request_messages(&messages);
    assert_eq!(bootstrap.len(), 3);
    assert_eq!(bootstrap[1].role, MessageRole::Developer);

    messages.push(message(MessageRole::Assistant, "new answer"));
    state.observe_response(Some("resp_v2"), messages.len());
    messages.push(message(MessageRole::User, "continue"));
    let upgraded = state.request_messages(&messages);
    assert_eq!(upgraded.len(), 2);
    assert_eq!(upgraded[0].role, MessageRole::System);
    assert_eq!(upgraded[1].content.as_deref(), Some("continue"));
    Ok(())
}

#[test]
fn mismatching_v2_seed_cuts_anchor_and_forces_full_replay() -> Result<(), ProviderError> {
    let old_seed = seed(&[(PromptSource::OperatorProfile, "old")]);
    let new_seed = seed(&[(PromptSource::OperatorProfile, "new")]);
    let state = ConversationRequestState::with_prompt_seed(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        2,
        new_seed,
        Some(ResponseThreadAnchor::for_test_with_prompt_seed(
            "resp_old".to_owned(),
            3,
            old_seed,
        )),
    )?;
    let messages = vec![
        message(MessageRole::System, "product"),
        message(MessageRole::Developer, "new"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new task"),
    ];

    assert!(state.previous_response_id().is_none());
    let request = state.request_messages(&messages);
    assert_eq!(request.len(), messages.len());
    assert_eq!(
        request
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        ["product", "new", "old answer", "new task"],
    );
    Ok(())
}

#[test]
fn hot_prefix_sync_preserves_system_only_change_but_cuts_non_system_change()
-> Result<(), ProviderError> {
    let original_seed = seed(&[(PromptSource::OperatorProfile, "operator-v1")]);
    let mut state = ConversationRequestState::with_prompt_seed(
        &config(ConversationStateMode::ProviderThreaded),
        ProviderCapabilities::openai_responses(),
        2,
        original_seed,
        Some(ResponseThreadAnchor::for_test_with_prompt_seed(
            "resp_old".to_owned(),
            3,
            original_seed,
        )),
    )?;
    let mut messages = vec![
        message(MessageRole::System, "product-v1"),
        message(MessageRole::Developer, "operator-v1"),
        message(MessageRole::Assistant, "old answer"),
        message(MessageRole::User, "new task"),
    ];

    state.sync_stable_prefix(
        &mut messages,
        vec![
            message(MessageRole::System, "product-v2"),
            message(MessageRole::Developer, "operator-v1"),
        ],
        original_seed,
    );
    assert_eq!(state.previous_response_id().as_deref(), Some("resp_old"));
    assert_eq!(state.request_messages(&messages).len(), 2);

    let changed_seed = seed(&[
        (PromptSource::OperatorProfile, "operator-v2"),
        (PromptSource::WorkspaceProfile, "repository"),
    ]);
    state.sync_stable_prefix(
        &mut messages,
        vec![
            message(MessageRole::System, "product-v2"),
            message(MessageRole::Developer, "operator-v2"),
            message(MessageRole::User, "repository"),
        ],
        changed_seed,
    );
    assert!(state.previous_response_id().is_none());
    assert_eq!(state.prefix_len(), 3);
    let request = state.request_messages(&messages);
    assert_eq!(request.len(), messages.len());
    assert_eq!(
        request
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        [
            "product-v2",
            "operator-v2",
            "repository",
            "old answer",
            "new task",
        ],
    );
    Ok(())
}
