use std::sync::Arc;

use anyhow::Result;
use async_stream::try_stream;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agents::agent::DEFAULT_MAX_TURNS;
use crate::agents::execute_commands::parse_slash_command;
use crate::agents::state_machine::operation::{Emitter, Operation, TurnOutcome};
use crate::agents::state_machine::ops_compaction::CompactionOperation;
use crate::agents::state_machine::ops_exit_on_error::ExitOnErrorOperation;
use crate::agents::state_machine::ops_llm::LlmOperation;
use crate::agents::state_machine::ops_maxturns::MaxTurnsOperation;
use crate::agents::state_machine::ops_slash_command::SlashCommandOperation;
use crate::agents::state_machine::ops_toolcalling::ToolExecutionOperation;
use crate::agents::types::SessionConfig;
use crate::agents::{Agent, AgentEvent};
use crate::config::Config;
use crate::conversation::message::Message;
use goose_providers::conversation::token_usage::Usage;

/// State-machine replacement for `Agent::reply`.
pub async fn reply(
    agent: &Agent,
    user_message: Message,
    session_config: SessionConfig,
    cancel_token: Option<CancellationToken>,
) -> Result<BoxStream<'_, Result<AgentEvent>>> {
    let session_manager = agent.config.session_manager.clone();

    // A never-cancelled token stands in for the un-cancellable caller so the
    // loop body never has to branch on the `Option`.
    let cancel = cancel_token.unwrap_or_default();

    let session_id = session_config.id.clone();
    if parse_slash_command(&user_message.as_concat_text()).is_none() {
        session_manager
            .add_message(&session_id, &user_message)
            .await?;
    }

    // Session naming is out-of-band: a detached task that overlaps the reply
    // loop, generates a title once early in a session, persists it, and pushes
    // it to the UI. It never reads or mutates the conversation, so it is not an
    // operation — see WIP.md "Two kinds of work".
    if !agent.config.disable_session_naming {
        let provider = agent.provider().await?;
        let manager = session_manager.clone();
        let tx = agent.config.session_name_update_tx.clone();
        let id = session_id.clone();
        tokio::spawn(async move {
            match manager.maybe_update_name(&id, provider).await {
                Ok(Some(update)) => {
                    if let Some(tx) = tx {
                        if tx.send(update).is_err() {
                            tracing::warn!("Failed to publish generated session name");
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Failed to generate session description: {}", e),
            }
        });
    }

    let working_dir = session_manager
        .get_session(&session_id, false)
        .await?
        .working_dir;
    let (tools, _toolshim_tools, system_prompt, model_config) = agent
        .prepare_tools_and_prompt(&session_id, &working_dir)
        .await?;

    let provider = agent.provider().await?;

    let max_turns = session_config.max_turns.unwrap_or_else(|| {
        Config::global()
            .get_param::<u32>("GOOSE_MAX_TURNS")
            .unwrap_or(DEFAULT_MAX_TURNS)
    });

    let operations: Vec<Arc<dyn Operation + '_>> = vec![
        Arc::new(SlashCommandOperation::new(agent, user_message)),
        Arc::new(MaxTurnsOperation::new(max_turns)),
        Arc::new(CompactionOperation::new(
            provider.clone(),
            model_config.clone(),
        )),
        Arc::new(LlmOperation::new(
            provider,
            model_config,
            system_prompt,
            tools,
        )),
        Arc::new(ToolExecutionOperation::new(agent.extension_manager.clone())),
        Arc::new(ExitOnErrorOperation),
    ];

    Ok(Box::pin(try_stream! {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            let session = session_manager
                .get_session(&session_id, true)
                .await?;

            let Some(op) = operations.iter().find(|op| op.applies(&session)).cloned() else {
                break;
            };
            tracing::debug!(target: "goose::state_machine", op = op.name(), "running operation");

            let (tx, mut rx) = mpsc::channel::<AgentEvent>(32);
            let emitter = Emitter::new(tx, cancel.clone());

            let outcome: TurnOutcome = {
                let op_fut = op.run(&session, emitter);
                tokio::pin!(op_fut);
                let result = loop {
                    tokio::select! {
                        biased;
                        Some(event) = rx.recv() => yield event,
                        result = &mut op_fut => break result,
                    }
                };
                result?
            };

            // Op returned; its Emitter dropped; channel closed. Drain leftovers.
            while let Some(event) = rx.recv().await {
                yield event;
            }

            match outcome {
                TurnOutcome::AppendMessages(messages) => {
                    for msg in &messages {
                        session_manager.add_message(&session.id, msg).await?;
                    }
                }
                TurnOutcome::AppendMessagesAndYield {
                    messages,
                    history_replaced,
                } => {
                    for msg in &messages {
                        session_manager.add_message(&session.id, msg).await?;
                    }
                    if history_replaced {
                        let updated_session = session_manager
                            .get_session(&session.id, true)
                            .await?;
                        if let Some(conversation) = updated_session.conversation {
                            yield AgentEvent::HistoryReplaced(conversation);
                        }
                    }
                    break;
                }
                TurnOutcome::ReplaceConversation(conversation) => {
                    session_manager
                        .replace_conversation(&session.id, &conversation)
                        .await?;
                    // The recorded usage described the old conversation; clear it
                    // so the next iteration recomputes against the new one rather
                    // than re-triggering compaction on a stale token count.
                    session_manager
                        .update(&session.id)
                        .usage(Usage::default())
                        .apply()
                        .await?;
                    yield AgentEvent::HistoryReplaced(conversation);
                }
                TurnOutcome::YieldToClient => break,
            }
        }
    }))
}
