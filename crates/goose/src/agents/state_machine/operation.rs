use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agents::AgentEvent;
use crate::conversation::message::Message;
use crate::conversation::Conversation;
use crate::session::Session;

/// One step in the agent loop. The first op whose `applies` returns true
/// gets to `run` — it streams events via the emitter and returns an outcome.
#[async_trait]
pub trait Operation: Send + Sync {
    fn name(&self) -> &'static str;

    fn applies(&self, session: &Session) -> bool;

    async fn run(&self, session: &Session, emit: Emitter) -> Result<TurnOutcome>;
}

/// What an operation returns when it finishes.
///
/// Ops produce events via `Emitter` *during* execution; the outcome describes
/// the state change to commit *after*. The machine applies the outcome but
/// does not derive client events from it — ops are responsible for emitting
/// what they want the client to see.
///
pub enum TurnOutcome {
    /// Append messages to the conversation
    AppendMessages(Vec<Message>),

    /// Append messages, optionally notify that history changed, then hand
    /// control back to the caller.
    AppendMessagesAndYield {
        messages: Vec<Message>,
        history_replaced: bool,
    },

    /// Replace the entire conversation (compaction, `/clear`, …)
    ReplaceConversation(Conversation),

    // TODO: `UpdateSession(SessionUpdate)` — variants added as ops need them
    // (provider name, model config, goose_mode, …).
    /// Hand control back to the caller and stop the loop
    YieldToClient,
}

/// An op's handle to the machine: emit events the client should see, and
/// observe cancellation. Long-running ops `select!` on [`Emitter::cancelled`];
/// short ops can ignore it entirely.
pub struct Emitter {
    tx: mpsc::Sender<AgentEvent>,
    cancel: CancellationToken,
}

impl Emitter {
    pub fn new(tx: mpsc::Sender<AgentEvent>, cancel: CancellationToken) -> Self {
        Self { tx, cancel }
    }

    /// Drops silently if the receiver is gone (caller cancelled the stream).
    pub async fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event).await;
    }

    /// The machine's cancellation token. Ops use it as they need — poll it,
    /// `select!` on it, or hand it to work that observes cancellation itself
    /// (e.g. tool dispatch).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Resolves when cancellation is requested. Convenience for `select!` arms.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }
}
