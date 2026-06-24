use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use rmcp::model::Role;

use crate::agents::execute_commands::{command_starts_turn, parse_slash_command, COMPACT_TRIGGERS};
use crate::agents::state_machine::operation::{Emitter, Operation, TurnOutcome};
use crate::agents::{Agent, AgentEvent};
use crate::conversation::message::Message;
use crate::session::Session;

pub struct SlashCommandOperation<'a> {
    agent: &'a Agent,
    user_message: Message,
    consumed: AtomicBool,
}

impl<'a> SlashCommandOperation<'a> {
    pub fn new(agent: &'a Agent, user_message: Message) -> Self {
        Self {
            agent,
            user_message,
            consumed: AtomicBool::new(false),
        }
    }

    fn message_text(&self) -> String {
        self.user_message.as_concat_text()
    }
}

#[async_trait]
impl Operation for SlashCommandOperation<'_> {
    fn name(&self) -> &'static str {
        "slash_command"
    }

    fn applies(&self, _session: &Session) -> bool {
        !self.consumed.load(Ordering::SeqCst) && parse_slash_command(&self.message_text()).is_some()
    }

    async fn run(&self, session: &Session, emit: Emitter) -> Result<TurnOutcome> {
        self.consumed.store(true, Ordering::SeqCst);

        let message_text = self.message_text();
        let command_result = self.agent.execute_command(&message_text, &session.id).await;

        match command_result {
            Err(e) => {
                let error_message = Message::assistant()
                    .with_text(e.to_string())
                    .with_visibility(true, false);
                emit.emit(AgentEvent::Message(error_message)).await;
                Ok(TurnOutcome::YieldToClient)
            }
            Ok(Some(response))
                if response.role == Role::Assistant && command_starts_turn(&message_text) =>
            {
                let user_message = self.user_message.clone().with_visibility(true, false);
                let response = response.with_visibility(true, false);

                emit.emit(AgentEvent::Message(user_message.clone())).await;
                emit.emit(AgentEvent::Message(response.clone())).await;

                let goal_text = parse_slash_command(&message_text)
                    .map(|parsed| parsed.params_str.to_string())
                    .unwrap_or_default();
                let kickoff = Message::user()
                    .with_text(format!(
                        "Start working toward this goal now:\n\n**Goal:** {goal_text}"
                    ))
                    .with_visibility(false, true);

                Ok(TurnOutcome::AppendMessages(vec![
                    user_message,
                    response,
                    kickoff,
                ]))
            }
            Ok(Some(response)) if response.role == Role::Assistant => {
                let user_message = self.user_message.clone().with_visibility(true, false);
                let response = response.with_visibility(true, false);

                emit.emit(AgentEvent::Message(user_message.clone())).await;
                emit.emit(AgentEvent::Message(response.clone())).await;

                let modifies_history = COMPACT_TRIGGERS.contains(&message_text.trim())
                    || message_text.trim() == "/clear";
                Ok(TurnOutcome::AppendMessagesAndYield {
                    messages: vec![user_message, response],
                    history_replaced: modifies_history,
                })
            }
            Ok(Some(resolved_message)) => {
                let user_message = self.user_message.clone().with_visibility(true, false);
                let resolved_message = resolved_message.with_visibility(false, true);
                Ok(TurnOutcome::AppendMessages(vec![
                    user_message,
                    resolved_message,
                ]))
            }
            Ok(None) => Ok(TurnOutcome::AppendMessages(vec![self.user_message.clone()])),
        }
    }
}
