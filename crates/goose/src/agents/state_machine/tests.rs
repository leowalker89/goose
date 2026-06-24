use anyhow::Result;
use rmcp::model::Role;

use crate::agents::state_machine::test_helpers::{
    tool_response_text, ScriptedProvider, Step, TestHarness,
};
use crate::agents::AgentEvent;
use crate::conversation::message::Message;
use std::sync::Arc;

#[tokio::test]
async fn llm_requests_tool_then_replies() -> Result<()> {
    let harness = TestHarness::with_steps([
        Step::ToolCall {
            id: "call_1".to_string(),
            name: "test__echo".to_string(),
            args: serde_json::json!({ "x": 1 }),
        },
        Step::Text("all done".to_string()),
    ])
    .await
    .with_default_extension()
    .await;

    let messages = harness.run("use the echo tool", 10).await?;

    // emitted: assistant(tool req) + user(tool resp) + assistant(text)
    assert_eq!(messages.len(), 3, "events: {messages:#?}");
    assert_eq!(messages[0].role, Role::Assistant);
    assert!(messages[0].is_tool_call());
    assert_eq!(messages[1].role, Role::User);
    assert!(messages[1].is_tool_response());
    assert_eq!(messages[2].role, Role::Assistant);

    // tool actually ran: echo returned the args as JSON text
    let resp_text = tool_response_text(&messages[1]);
    assert!(resp_text.contains("\"x\":1"), "tool response: {resp_text}");

    // provider was called twice (tool turn + final text turn)
    assert_eq!(harness.provider.call_count(), 2);

    // persisted conversation matches what was emitted (prompt + 3 above)
    let persisted = harness.persisted_messages().await?;
    assert_eq!(persisted.len(), 4);
    assert_eq!(persisted[0].role, Role::User);

    Ok(())
}

#[tokio::test]
async fn stops_at_max_turns() -> Result<()> {
    // The provider never stops on its own — every turn calls a tool, whose
    // response re-triggers the LLM. Only the max-turns op can halt the loop.
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let provider = Arc::new(ScriptedProvider::from_fn(move |_messages, _tools| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        vec![Message::assistant().with_tool_request(
            format!("call_{n}"),
            Ok(rmcp::model::CallToolRequestParams::new("test__echo")
                .with_arguments(serde_json::Map::new())),
        )]
    }));
    let harness = TestHarness::with_provider(provider)
        .await
        .with_default_extension()
        .await;

    let messages = harness.run("keep going", 3).await?;

    // 3 LLM turns, then the max-turns op halts before a 4th.
    assert_eq!(harness.provider.call_count(), 3);

    let limit = messages.last().expect("at least one message");
    assert_eq!(limit.role, Role::Assistant);
    assert!(
        limit.as_concat_text().contains("maximum number of actions"),
        "last message: {limit:#?}"
    );

    // The 3 tool-calling turns are persisted; the limit message is not.
    let persisted = harness.persisted_messages().await?;
    let tool_call_turns = persisted.iter().filter(|m| m.is_tool_call()).count();
    assert_eq!(tool_call_turns, 3);

    Ok(())
}

#[tokio::test]
async fn compacts_when_over_token_threshold() -> Result<()> {
    // Every provider call (the compaction summary and the post-compaction LLM
    // turn) returns plain text, so the loop ends after one real turn.
    let provider = Arc::new(ScriptedProvider::from_fn(|_messages, _tools| {
        vec![Message::assistant().with_text("ok")]
    }));
    let harness = TestHarness::with_provider(provider).await;

    // 128k context * 0.8 threshold = 102_400; push well past it.
    harness.set_total_tokens(120_000).await;

    let events = harness.run_events("hello", 10).await?;

    // Compaction replaced the conversation exactly once.
    let replaced = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::HistoryReplaced(_)))
        .count();
    assert_eq!(replaced, 1, "events: {events:#?}");

    // The "Performing auto-compaction" notice was emitted.
    use crate::conversation::message::MessageContent;
    let saw_notice = events.iter().any(|e| {
        match e {
        AgentEvent::Message(m) => m.content.iter().any(|c| {
            matches!(c, MessageContent::SystemNotification(s) if s.msg.contains("auto-compaction"))
        }),
        _ => false,
    }
    });
    assert!(saw_notice, "events: {events:#?}");

    // Provider was called for the summary and then the post-compaction turn.
    assert_eq!(harness.provider.call_count(), 2);

    // The token total was cleared so compaction doesn't re-trigger.
    let reloaded = harness.reload().await?;
    assert!(reloaded.usage.total_tokens.is_none());

    Ok(())
}

#[tokio::test]
async fn provider_error_is_persisted_and_yields() -> Result<()> {
    use crate::conversation::message::MessageErrorKind;
    use goose_providers::errors::ProviderError;

    let provider = Arc::new(ScriptedProvider::from_steps([Step::Error(
        ProviderError::ServerError("boom".to_string()),
    )]));
    let harness = TestHarness::with_provider(provider).await;

    let events = harness.run_events("hello", 10).await?;

    // The error surfaced as a message event (replacing the old notification).
    let saw_error_event = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Message(m) if m.error_kind() == Some(MessageErrorKind::Other)
        )
    });
    assert!(saw_error_event, "events: {events:#?}");

    // It is durable conversation state, tagged, user-visible, agent-invisible.
    let persisted = harness.persisted_messages().await?;
    let last = persisted.last().expect("a persisted message");
    assert_eq!(last.error_kind(), Some(MessageErrorKind::Other));
    assert!(last.is_user_visible());
    assert!(!last.is_agent_visible());

    // The provider was called exactly once: ExitOnError yielded, no retry.
    assert_eq!(harness.provider.call_count(), 1);

    Ok(())
}

#[tokio::test]
async fn context_length_error_triggers_compaction_recovery() -> Result<()> {
    use goose_providers::errors::ProviderError;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // First LLM call blows the context; after compaction replaces the
    // conversation, the retried call succeeds with plain text.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_fn = calls.clone();
    let provider = Arc::new(ScriptedProvider::from_fn_result(
        move |_messages, _tools| {
            match calls_for_fn.fetch_add(1, Ordering::SeqCst) {
                // call 0: the failing turn
                0 => Err(ProviderError::ContextLengthExceeded("too long".to_string())),
                // call 1: the compaction summary
                // call 2: the retried turn
                _ => Ok(vec![Message::assistant().with_text("recovered")]),
            }
        },
    ));
    let harness = TestHarness::with_provider(provider).await;

    let events = harness.run_events("hello", 10).await?;

    // Compaction replaced the conversation as part of recovery.
    let replaced = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::HistoryReplaced(_)))
        .count();
    assert_eq!(replaced, 1, "events: {events:#?}");

    // The turn ultimately succeeded; no error message lingers on the tail.
    let persisted = harness.persisted_messages().await?;
    let last = persisted.last().expect("a persisted message");
    assert!(last.error_kind().is_none(), "tail still an error: {last:?}");

    // Failing turn + compaction summary + retried turn = three provider calls.
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    Ok(())
}

#[tokio::test]
async fn slash_command_yields_without_calling_provider() -> Result<()> {
    let harness = TestHarness::with_steps([]).await;

    let messages = harness.run("/goal", 10).await?;

    assert_eq!(harness.provider.call_count(), 0);
    assert_eq!(messages.len(), 2, "messages: {messages:#?}");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[1].role, Role::Assistant);
    assert!(
        messages[1].as_concat_text().contains("No goal set"),
        "response: {:#?}",
        messages[1]
    );

    let persisted = harness.persisted_messages().await?;
    assert_eq!(persisted.len(), 2);
    assert!(persisted.iter().all(|m| m.is_user_visible()));
    assert!(persisted.iter().all(|m| !m.is_agent_visible()));

    Ok(())
}

#[tokio::test]
async fn goal_slash_command_starts_turn_with_hidden_kickoff() -> Result<()> {
    let harness = TestHarness::with_steps([Step::Text("done".to_string())]).await;

    let messages = harness.run("/goal make all tests pass", 10).await?;

    assert_eq!(harness.provider.call_count(), 1);
    assert_eq!(messages.len(), 3, "messages: {messages:#?}");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[2].role, Role::Assistant);
    assert_eq!(messages[2].as_concat_text(), "done");

    let persisted = harness.persisted_messages().await?;
    assert_eq!(persisted.len(), 4);
    assert_eq!(persisted[0].role, Role::User);
    assert!(persisted[0].is_user_visible());
    assert!(!persisted[0].is_agent_visible());
    assert_eq!(persisted[1].role, Role::Assistant);
    assert!(persisted[1].is_user_visible());
    assert!(!persisted[1].is_agent_visible());
    assert_eq!(persisted[2].role, Role::User);
    assert!(!persisted[2].is_user_visible());
    assert!(persisted[2].is_agent_visible());
    assert!(persisted[2]
        .as_concat_text()
        .contains("Start working toward this goal now"));
    assert_eq!(persisted[3].role, Role::Assistant);

    Ok(())
}
