# State Machine — Work in Progress

This module is the WIP unrolled agent loop, replacing the monolithic
`Agent::reply_internal`. It is gated behind the `GOOSE_STATE_MACHINE`
environment variable.

The thesis: **the conversation is the state.** Operations observe the
current `Session` and return declarative outcomes; the machine applies them.
Persistence, event emission, and orchestration live in the machine driver,
not in operations.

---

## Layout

```
state_machine/
├── mod.rs          # public surface: `reply` + `enabled` flag check
├── machine.rs      # the driver: assemble ops, run loop, apply outcomes
├── operation.rs    # Operation trait, Emitter, TurnOutcome
├── ops_llm.rs      # chat LLM operation (streaming); provider errors -> error messages
├── ops_toolcalling.rs  # execute tool requests, synthesize responses
├── ops_slash_command.rs # first-turn slash command handling
├── ops_maxturns.rs # halt after N assistant turns
├── ops_compaction.rs   # proactive + reactive (ContextLengthExceeded) auto-compact
├── ops_exit_on_error.rs # terminal: yield when the tail is an unrecovered error
├── test_helpers.rs # ScriptedProvider, TestExtensionClient, TestHarness
├── tests.rs        # scenario tests
└── WIP.md          # this file
```

---

## Current status

- [x] `GOOSE_STATE_MACHINE=1` flag dispatch from `Agent::reply`
- [x] `Operation` trait with `applies(&Session)` + `run(&Session, Emitter) -> TurnOutcome`
- [x] Streaming `LlmOperation` with tools (model can emit `ToolRequest`s)
- [x] `ToolExecutionOperation` — bare execute-and-respond (no approval/frontend/chat-mode)
- [x] `SlashCommandOperation` — first-turn command handling, including `/goal` kickoff
- [x] `MaxTurnsOperation` — halts the loop after `max_turns` assistant turns this request
- [x] `CompactionOperation` — proactive auto-compact before an LLM call (returns `ReplaceConversation`)
- [x] Machine driver applies `AppendMessages`, `ReplaceConversation`, `YieldToClient`
- [x] Machine clears stale `total_tokens` on `ReplaceConversation` so compaction can't re-trigger
- [x] Cancellation plumbed through the machine + `Emitter`
- [ ] More operations (see backlog below)
- [x] Errors as conversation state — provider errors become tagged, user-visible / agent-invisible messages (replacing the old fire-and-forget notification)
- [x] `ExitOnErrorOperation` — terminal catch-all; yields when the tail is an unrecovered error
- [x] Reactive compaction on `ProviderError::ContextLengthExceeded` (capped retries, counted from the conversation)
- [ ] `UpdateSession(SessionUpdate)` outcome variant

---

## Shape

### `Operation`

```rust
#[async_trait]
pub trait Operation: Send + Sync {
    fn name(&self) -> &'static str;
    fn applies(&self, session: &Session) -> bool;
    async fn run(&self, session: &Session, emit: Emitter) -> Result<TurnOutcome>;
}
```

Ops take `&Session` (read-only — the conversation IS the state) and an
`Emitter`. The `Emitter` is the op's handle to the machine: it carries a
sender for `AgentEvent`s the client should see in real time, and a
`CancellationToken`. Ops stream 0+ events through the emitter and return one
`TurnOutcome`.

Long-running, streaming ops `select!` on `emit.cancelled()`. On cancel they
commit whatever they fully produced (via the normal `AppendMessages`) and
`break`; the machine's *between-ops* cancel check then ends the loop. Short
ops ignore cancellation entirely — they run to completion and the loop stops
on the next iteration. A cancelled `reply()` ends the stream cleanly (no
`Err`). There is no dedicated `Canceled` outcome: for now the caller doesn't
distinguish a cancelled stop from a normal one, so the between-ops check is
all we need. Add a distinct outcome later *if/when* we want to signal cancel
to the caller.

**Whatever an op commits must be a valid, self-consistent conversation tail.**
This is the op's responsibility, not the machine's and not a downstream repair
pass:

- The LLM op drops the in-flight chunk and commits whole chunks already
  emitted. A chat turn has no tool requests to pair, so this is trivially
  valid.
- The **tool-execution op**, on cancel, must synthesize a cancellation
  `ToolResponse` for every in-flight `ToolRequest` before committing, so the
  committed tail contains no orphaned request. The model then sees that those
  calls were interrupted, rather than the conversation being silently
  repaired (dropped) on the next read.

The old loop instead buffered everything in a local `messages_to_add` and
flushed it only when it believed the turn was consistent — but on the cancel
path it flushed the partial buffer anyway, so the invariant wasn't actually
enforced there; it leaned on `fix_conversation` dropping orphans at read time.
We make the persisted conversation the real state at all times and make each
op responsible for leaving it valid.

**The SessionManager is the single source of truth; the machine keeps no
in-memory copy.** Each loop iteration re-reads the session via
`get_session(id, true)` before selecting an op, and outcomes only *write* to
the SessionManager (`add_message` / `replace_conversation`) — they never patch
a local `Session`. This avoids reintroducing the `messages_to_add` failure
mode in a new shape: a hand-maintained mirror that can drift from disk. It
already would have drifted, because `add_message` assigns a message id when
one is missing, so a pushed-but-not-reloaded message differs from its
persisted form. The reload costs one small indexed DB read per turn —
negligible next to an LLM call — and guarantees `applies`/`run` see exactly
the persisted state (ids assigned, stored order).

Construction-time dependencies (providers, system prompts, extension
managers, per-call knobs like `max_turns`) are passed to the op's
constructor — never on `Session`. The state machine itself does not know
they exist.

### `TurnOutcome`

```rust
pub enum TurnOutcome {
    AppendMessages(Vec<Message>),
    ReplaceConversation(Conversation),
    // TODO: UpdateSession(SessionUpdate)
    YieldToClient,
}
```

The machine commits the outcome to `session` and persists it via
`SessionManager`. It does **not** auto-emit events for `AppendMessages` —
ops already streamed what they wanted visible. The `Conversation` type
handles chunk merging on `push`, so the LLM op can push raw provider chunks
and get a clean merged final result.

### Machine driver

The driver (`machine::reply`) is the only place that:

- persists messages and conversations via `SessionManager`
- mutates `session` (push to conversation, replace, future field updates)
- runs the `applies`/`run` loop
- turns ops' emitted events into the client `AgentEvent` stream
- forwards `HistoryReplaced` on `ReplaceConversation`

Loop termination: either an op returns `YieldToClient`, or no op applies
(every op's `applies` returned false).

---

## Two kinds of work

Not everything the machine does is an operation. There are two categories:

1. **Turns** — operations the loop runs *sequentially*: each reads the
   conversation and returns a `TurnOutcome` the machine awaits before
   continuing. The LLM call, tool execution, compaction, etc. These are
   selected by `applies` every iteration and are the substance of the loop.

2. **Out-of-band side effects** — concurrent, conversation-independent,
   fire-and-forget work triggered at reply *boundaries*. They run alongside
   the loop (`tokio::spawn`), never block it, produce no `TurnOutcome`, and
   their results reach the outside world via a side channel rather than the
   `AgentEvent` stream.

**Session naming** is the canonical out-of-band effect, spawned once at the
start of `reply` (mirroring the old loop): `maybe_update_name` generates a
title via the provider, persists it as a *session field* (not a message), and
publishes a `SessionNameUpdate` on `session_name_update_tx` for the UI. It is
deliberately *not* an operation:

- It must be **concurrent** with the first turn — title generation is itself
  an LLM call, and the whole point is to overlap it with the user's real
  response. An op would block the turn or spawn-inside-an-op (a spawn in a
  costume).
- Its output is **not a `TurnOutcome`** — it touches no message, only a
  session metadata field plus a UI side-channel.
- It is **once-per-reply**, not once-per-turn, so re-evaluating `applies`
  every iteration is the wrong model and would need a "did I run" flag — the
  cross-iteration state we are trying to eliminate.

The boundary matters for what comes next: **tool-pair compaction** is a
*hybrid* — spawned concurrently like naming, but unlike naming its result
feeds back into the conversation (marks a request/response pair invisible,
inserts a summary). An out-of-band effect that mutates the very state the loop
reads is the hard case; see Open questions.

---

## Operations to port

Roughly in order of value, with the code in `agents/agent.rs` they replace:

| Operation | Replaces | Notes |
|---|---|---|
| **LLM** | `stream_response_from_provider` + the main `while let Some(next) = stream.next()` arms | **Landed.** Streams the response with the real `tools` list, so the model can emit `ToolRequest`s. Persists the assistant message (requests + thinking/reasoning) as-is. Constructor: `(Arc<dyn Provider>, system_prompt, tools)`. |
| **Tool approval** | `tool_inspection_manager.inspect_tools` + `process_inspection_results_with_permission_inspector` + `handle_approval_tool_requests` | Not started. Annotates `ToolRequest`s with approval state. YOLO short-circuits. Approval is a **separate op** that runs *before* execution; the state lives on the request in the conversation, not in a side map. |
| **Tool execution** | `handle_approved_and_denied_tools` + `combined.next()` `tokio::select!` loop + frontend tool sub-flow | **Landed (bare path only).** `applies` when the last message is an assistant message with parseable tool requests. Dispatches each via `dispatch_tool_call`, drains streams forwarding `McpNotification`s, collects responses into one user message, `AppendMessages`. On cancel: cancels the dispatch token and synthesizes interrupted-tool responses so the committed tail is valid. Constructor: `(&Agent)`. **Deferred:** approval/inspection, frontend tools, chat-mode skip, the elicitation/100ms-tick drain loop, `MANAGE_EXTENSIONS`/`tools_updated`, unparseable-tool-call error path. |
| **Compaction** | `check_if_compaction_needed` block + `ContextLengthExceeded` arm in `reply()` | **Landed (proactive + reactive).** Proactive: cheap synchronous ratio check (`session.total_tokens` vs model context limit, both captured at construction) when the last message is a pending user prompt. Reactive: when the tail is a `ContextLengthExceeded` error message (the LLM op appends one instead of bubbling), compact-and-retry up to `MAX_CONTEXT_ERROR_RETRIES`, counted from the conversation. `run` strips the trailing error before summarizing. The machine clears `total_tokens` after a replace so it can't re-trigger on a stale count. Constructor: `(Arc<dyn Provider>)`. **Deferred:** per-compaction usage metrics. |

### Errors as conversation state

An error during a turn is now first-class **conversation** state, not a
state-machine invention: `MessageContent::Error(ErrorContent { kind, message })`
lives in `conversation/message.rs` with a typed `MessageErrorKind`
(`From<&ProviderError>`). `Message::from_provider_error` builds the user-facing
message (user-visible / agent-invisible); `Message::error_kind()` reads the kind
back. It's part of the OpenAPI surface, so the desktop renders it as a real
`error` message content.

The LLM op catches `ProviderError` (at stream creation and mid-stream), discards
any partial turn, and appends that error message instead of unwinding the
stream. Recovery ops dispatch on the kind: compaction reacts to
`ContextLengthExceeded`; everything else falls through to
`ExitOnErrorOperation` (last in the op list), which yields so the user can read
the error and retry with a new message. This replaces the old fire-and-forget
`yield notification; break`.
| **Tool-call pair compaction** | `crate::context_mgmt::maybe_summarize_tool_pairs` background task | Synchronous first cut; revisit backgrounding if it regresses latency. |
| **Elicitation** | `drain_elicitation_messages` + `ActionRequiredManager` calls | When a tool request needs elicitation and has no response: `YieldToClient` (after emitting an elicitation request). Re-entry via `reply()` with `ElicitationResponse`. |
| **Max turns** | `if turns_taken > max_turns` block | Trivial. Counter is per-op or per-machine state (TBD when needed). |
| **Retry / goal / grind / final-output** | `handle_retry_logic` + `goal` / `grind` / `final_output` blocks | One op when last assistant message has no tool requests. May append a nudge or `YieldToClient`. |
| **Subagent sync** | `subagent_handler` + `moim::inject_moim` | When subagents have results to report: append, run another turn. |
| **Hooks (cross-cutting)** | scattered `hook_manager.emit(...)` and `emit_blocking(...)` calls | Run alongside ops, not in the ordered list. `UserPromptSubmit` on entry, `Stop` before `YieldToClient`. Denial flows back via session state. |
| **Slash commands** | `execute_command` block in `reply()` | **Landed.** First-turn-only op. Non-turn assistant responses append user-visible / agent-invisible command + response and yield. `/goal` / `/grind` commands that start turns append the visible command/confirmation plus a hidden kickoff user message, then continue into the LLM op. Prompt/skill commands that resolve to agent-visible user text append that hidden text and continue. |
| **Refresh tools after `manage_extensions`** | `tools_updated` block | Either a tail-step of the Tool execution op or a separate op. |

---

## Open questions

- **Emit as the single channel; machine collects (don't return messages).**
  Today an op both emits a message to the client *and* returns it in
  `AppendMessages`, and the machine persists the returned payload. That's two
  places to keep in sync and lets "shown to client" drift from "persisted".
  Cleaner: ops **always emit** messages (LLM deltas, MCP notifications, each
  tool response *as it lands*), never return them; the **machine collects the
  emitted messages and persists them**. One path, machine is the sole
  persister, identical by construction. Bonus: with two tool calls running
  concurrently, the current op holds both results and emits one merged message
  only when *both* finish — emit-as-you-go shows each result the moment it
  lands. Wrinkles to handle:
  - LLM op emits streaming deltas *and* a final coalesced message; the machine
    must persist only the final (coalesce by message id, like
    `Conversation::push` already does) — confirm the `with_id` path covers it.
  - Tool responses become N emits (per-id) instead of one merged user message;
    the machine coalesces by id. Slightly changes the persisted shape — be
    deliberate.
  - `YieldToClient` / `ReplaceConversation` stay as return outcomes (control
    flow / whole-conversation), so `TurnOutcome` keeps those — only
    `AppendMessages` dissolves into "the machine collected what you emitted".
  Not now (touches machine + LLM op + tool op together); current code is
  correct, just batches tool results suboptimally.
- **Platform tools are not handled by the toolcalling op.** The old
  `dispatch_tool_call` intercepts `final_output` and `platform__manage_schedule`
  before the extension manager — they're not real MCP tools. These are
  leftovers that should move to the dedicated platform-tools class. The
  toolcalling op deliberately doesn't special-case them: `final_output`
  belongs with the retry/goal/grind/final-output op; schedule belongs with
  whatever owns scheduling. Until then, calling them via the state machine
  produces a tool-not-found error response.
- **Where do turn counters live?** Today there are none. When the max-turns
  op lands, it needs to count turns across loop iterations. Options: pass a
  mutable counter into the op constructor (`Arc<AtomicU32>`), or reintroduce
  a thin `TurnState { session, counters }` wrapper. Defer until needed.
- **System prompt rebuild policy.** Baked at construction is fine for chat;
  for tools we need to rebuild when extensions change. Likely an
  `Arc<PromptManager>` on the LLM op and a session-side version marker the
  op reads.
- **Persistence granularity.** Per-outcome (write after each append) — same
  as today's behaviour. Fine.
- **Subagent reporting** is push-driven today (subagent posts back via a
  channel). The state-machine framing wants pull-driven (a `SubagentSync`
  op checks for queued results). Where does the buffer live? Probably on
  the agent, not the session.
- **Hooks as "cross-cutting"** — likely the machine fires hooks at
  well-known points (turn start, before LLM, after tool execution, before
  yield) rather than ops doing it.
- **First-class background operations.** Right now out-of-band work (session
  naming) is a raw `tokio::spawn` that floats free of the machine — the loop
  has no handle on it, can't cancel it, and can't wait for it. A cleaner
  model: let an op return `TurnOutcome::RunningInBackground(JoinHandle<...>)`.
  The machine keeps these handles in a set, continues the loop immediately,
  and at termination (`YieldToClient` / no-op-applies / cancel) either
  **awaits** them (work that must finish, e.g. flush a summary) or
  **aborts** them (cancel). This brings background work back under the
  machine's lifecycle: cancellation cleanup becomes uniform (the machine
  aborts the set, ops don't each hand-roll `.abort()`), and shutdown is
  deterministic instead of detached-and-hope.
  Open sub-questions before adopting it:
  - **Result feedback.** Naming's result goes to a UI side-channel and never
    re-enters the loop — fine to fire-and-forget. But **tool-pair
    compaction's** result *mutates the conversation* (marks a pair invisible,
    inserts a summary). If it completes mid-loop, does its write land via the
    SessionManager (and get picked up by the next iteration's reload), or does
    the machine need to *join* it at a turn boundary and apply a
    `ReplaceConversation`-like outcome so ordering is deterministic? The
    former keeps the "single source of truth, reload each iteration" model but
    makes the conversation mutate underneath an in-flight turn; the latter is
    ordered but reintroduces a join point.
  - **Await-vs-abort policy.** Per-handle (naming = abort-ok, compaction =
    must-finish) or a flag on the variant.
  - Until this is designed, naming stays a plain spawn and the tool-execution
    op owns its summarization `JoinHandle` (see Cancellation cleanup).
- **Cancellation cleanup.** Resolved for the LLM op (drop in-flight chunk,
  commit whole chunks). The tool-execution op, on cancel, must (1) synthesize
  a cancellation `ToolResponse` for each in-flight `ToolRequest` so its
  `committed` tail is valid, and (2) `.abort()` any background work it owns
  (e.g. the tool-pair summarization `JoinHandle`) before returning. The
  machine doesn't yank a running op — it relies on the op observing
  `emit.cancelled()` and cleaning up itself.

---

## Migration steps remaining

1. Add ops in the order in the backlog table.
2. Fold `reply()` entry-point logic in (elicitation response, slash
   commands, `UserPromptSubmit` hook, pre-turn auto-compact) as
   first-turn-only ops.
3. Tests: scenario tests driven by a scripted provider. Because ops are
   independently constructable and the machine just sequences them, each op
   can be instrumented on its own and in combination with others. Build these
   out once there's enough op surface to be worthwhile — not a differential
   oracle against the old loop. Tests call `state_machine::reply` directly
   (parallel-safe, no env var). A scripted provider returns canned
   responses/tool-requests per call so a scenario can drive LLM → tool
   execution → LLM, etc.
4. Flip the flag default after a release with no regressions.
5. Delete `reply_internal` and friends.
6. Public API for swapping the pipeline (`AgentConfig::operations`,
   dynamic insert/remove).
