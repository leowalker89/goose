# Model Council — Feasibility Research & PRD

**Status:** Research complete, ready for implementation  
**Audience:** Product and engineering (internal design doc — not published user documentation)  
**Scope:** Perplexity-style multi-model fan-out with synthesis, consensus/disagreement surfacing, and full tool access per council member  
**Code references:** File paths and line numbers verified against commit `7026e1458` (2026-07-01). Line numbers drift with refactors — treat symbol names as authoritative.

---

## Executive summary

**Verdict: Buildable.** goose already has the core primitives — parallel subagent execution via `summon`/`delegate`, per-member provider+model overrides, structured JSON output via `FinalOutputTool`, SQLite WAL-backed concurrent session writes, and a desktop drill-down pattern via `subagent_session_id`.

**Recommendation:** Implement Model Council as a **first-class orchestration mode** (not LLM-discretionary delegate calls), reusing `run_subagent_task` for tool-capable members and `FinalOutputTool` for synthesis. Trigger via a new goose ACP unstable method (desktop-first), with metadata-tagged messages in the main session.

**Important prior art in goose:** The [Council of Mine MCP extension](../../documentation/docs/mcp/council-of-mine-mcp.md) already provides multi-LLM debate/vote/synthesis as an MCP tool, but it is personality-based, agent-invoked, and not equivalent to Perplexity's user-triggered cross-model verification with real provider diversity and tool-using members.

---

## Research task 1: Fan-out validation

### Question

Can 2–3 concurrent tool-using subagent sessions run safely today via existing `delegate`, and what are the latency/contention characteristics?

### Codebase findings

| Area | Finding | Evidence |
|------|---------|----------|
| Parallel execution | Multiple tool calls in one assistant turn execute concurrently via `stream::select_all` | `crates/goose/src/agents/agent.rs` ~2182–2189 |
| Per-member isolation | Each `delegate` creates a new `SessionType::SubAgent` session with its own SQLite `session_id` | `crates/goose/src/agents/platform_extensions/summon.rs` ~1257–1287 |
| DB concurrency | SQLite uses **WAL mode** + **30s busy timeout**; writes use `BEGIN IMMEDIATE` per transaction | `crates/goose/src/session/session_manager.rs` ~756–761, 1601–1633 |
| Cross-session writes | Concurrent subagents write to **different `session_id` rows** — no shared row lock contention beyond pool-level SQLite serialization | messages table keyed by `session_id` |
| Provider concurrency | `Arc<dyn Provider>` is `Send + Sync`; each subagent holds its own provider instance via `update_provider` | `crates/goose-providers/src/base.rs`, `subagent_handler.rs` ~143–149 |
| Per-member model override | `delegate` accepts `provider` and `model` params | `summon.rs` `DelegateParams` ~57–58 |
| Tool access | Members inherit parent extensions by default (`EnabledExtensionsState::extensions_or_default`) | `summon.rs` ~1533–1535 |
| Nesting limit | SubAgents **cannot** delegate further | `summon.rs` ~1220–1222 |
| Permission mode | Subagents forced to `GooseMode::Auto` (no user approval UI forwarded to parent yet) | `summon.rs` ~1244–1253 |
| Sync vs async delegate | Sync `delegate` blocks until member completes; async mode has `max_background_tasks()` cap | `summon.rs` ~433, 1755–1767 |

### What subagents do NOT get automatically

- **Parent conversation history.** Subagents start with only `recipe.prompt` as the user message (`subagent_handler.rs` ~171–172). Prior turns are not forwarded unless explicitly injected via the delegate `context` parameter, which `build_instructions_with_context` (`summon.rs` ~315) prepends to the system instructions as a `# Reference Context` section.
- **Wall-clock timeouts.** Neither `DelegateParams` nor `SubagentRunParams` supports a timeout today — only `max_turns` caps runaway loops. The per-member and per-turn timeouts in research task 6 are **net-new orchestrator work** (e.g. `tokio::time::timeout` wrapping each member future, cancelling via the member's `CancellationToken` on expiry).
- **Shared retrieval.** Each member runs its own tool loop independently; web search / file reads are not deduplicated.

### Manual spike procedure (recommended before build)

Run in goose desktop with `summon` enabled and 2+ providers configured:

1. Prompt: *"Call delegate 3 times in parallel with provider overrides — provider A/B/C, same instructions: summarize the trade-offs of Rust vs Go for CLI tools. Use async:false."*
2. Observe: latency spread, whether all three complete, subagent session links in tool results.
3. Open each `subagent_session_id` in a new window and compare transcripts.

### Decision

**Proceed with subagent-based fan-out.** Code analysis shows no architectural blockers for 3 concurrent members. The main gaps are orchestration (automatic fan-out + synthesis) and explicit parent-context injection, not concurrency mechanics.

---

## Research task 2: Structured synthesis output

### Question

Can `FinalOutputTool` (recipe `response.json_schema` pattern) reliably produce `{narrative, consensus[], disagreements[], unresolved[]}` for the synthesizer step?

### Codebase findings

`FinalOutputTool` (`crates/goose/src/agents/final_output_tool.rs`):

- Requires non-empty JSON Schema at construction; validates with `jsonschema::meta::validate`
- Injects system prompt instructing model to call `recipe__final_output` tool
- Validates tool call arguments against schema at execution time; returns validation errors for retry
- Stores validated output as single-line JSON string in `final_output`
- Already wired into subagents via `apply_recipe_components(recipe.response, true)` in `subagent_handler.rs` ~162–165

Subagent flow when `recipe.response` is set:

1. Agent runs full tool loop
2. On completion, `get_final_output()` reads validated JSON from `FinalOutputTool`
3. Returns structured string instead of free-text extraction

### Limitations

- Synthesizer would need its own short agent session (or direct orchestration) with a recipe containing `response.json_schema` — not a raw `provider.complete()` call
- Model must successfully call the tool; agent loop may need continuation nudge (`FINAL_OUTPUT_CONTINUATION_MESSAGE`) if model responds with text instead
- Schema must be authored carefully; overly strict schemas cause retry loops

### Decision

**Use `FinalOutputTool` for synthesis (Option B).** Proposed schema:

```json
{
  "type": "object",
  "required": ["narrative", "consensus", "disagreements", "unresolved"],
  "properties": {
    "narrative": { "type": "string", "description": "Unified answer synthesizing all member outputs" },
    "consensus": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Points all or most members agree on"
    },
    "disagreements": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["topic", "positions"],
        "properties": {
          "topic": { "type": "string" },
          "positions": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["model", "claim"],
              "properties": {
                "model": { "type": "string" },
                "claim": { "type": "string" }
              }
            }
          }
        }
      }
    },
    "unresolved": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Conflicts that could not be reconciled"
    }
  }
}
```

---

## Research task 3: Shared context across members

### Options evaluated

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **A — Shared history, independent tools** | Pass serialized parent conversation via `context`; each member runs own tool loop forward | Matches cross-checking value prop; fair comparison on same facts | Large context injection; token cost ×3; may hit context limits |
| **B — Independent (prompt only)** | Each member sees only the council query | Simple; lower token cost | Loses mid-conversation Council utility |
| **C — Shared retrieval, independent reasoning** | Run retrieval once, inject shared tool results into all members | Isolates reasoning differences from information differences | Complex orchestration; doesn't match Perplexity's undocumented behavior |

### Decision

**Option A for v1**, with pragmatic truncation:

- Include parent conversation history (agent-visible messages only) in each member's `context` parameter
- Truncate to fit per-model context budget (reuse compaction/summarization utilities if needed)
- Each member executes tools independently (accept 3× tool cost for v1)
- Document that Option C is a v2 optimization if cost becomes prohibitive

**Rationale:** Perplexity positions Council for verification and strategic analysis mid-research — that requires conversational continuity. Option B is insufficient for the primary use case.

**Injection mechanism caveat:** the existing `context` param lands in the *system prompt* (prepended via `build_instructions_with_context`), not in message history. For long conversations that means very large system prompts × N members, no prefix-cache benefit across turns on providers that cache by prefix, and degraded instruction-following on some providers. The orchestrator is not bound to the `context` param — it calls `run_subagent_task` directly and can instead reconstruct the parent turns as real conversation messages ahead of the council query. Decide system-prompt vs. reconstructed-history injection during Phase 1 with a quick eval; the decision here settles *what* is shared (Option A), not the mechanism.

---

## Research task 4: Trigger surface (ACP / API)

### Current transport

| Path | Used by | Council relevance |
|------|---------|-------------------|
| ACP `session/prompt` | Desktop UI (`ui/desktop/src/acp/prompt.ts`) | Primary trigger surface |
| HTTP `POST /sessions/{id}/reply` + SSE events | External clients, not desktop chat | Secondary |
| LLM `delegate` tool | Agent-discretionary | Wrong UX for explicit user action |

### ACP extensibility

goose already extends ACP via `_goose/unstable/*` methods registered in `crates/goose/acp-meta.json` (40+ custom methods). Pattern:

1. Define request/response types in goose-sdk
2. Register in `acp-meta.json`
3. Add `#[custom_method(...)]` handler in `crates/goose/src/acp/server/custom_dispatch.rs`
4. Desktop calls via `client.goose.*_unstable(...)`

`on_prompt` (`crates/goose/src/acp/server.rs` ~2431) always calls `agent.reply()` — no mode flag today. Standard ACP `PromptRequest` has no council field.

### Options

| Option | Approach | Recommendation |
|--------|----------|----------------|
| Flag on prompt | Extend prompt with metadata (non-standard ACP) | Avoid — breaks protocol assumptions |
| New unstable method | `_goose/unstable/session/council/prompt` | **Recommended** |
| Dedicated HTTP endpoint | `POST /sessions/{id}/council` | Add for API parity, lower priority |
| Session config toggle | `setSessionConfigOption({ configId: "council", value: "true" })` | Possible for "arm council for next send" UX |

### Decision

**Primary: new ACP unstable method** `_goose/unstable/session/council/prompt` accepting:

- `sessionId`
- `prompt` (same shape as standard prompt)
- Optional `councilConfigId` (defaults to user's configured roster)

Behavior mirrors `on_prompt` for streaming (`SessionNotification` chunks) but internally calls `run_council` instead of `agent.reply`. Respects existing one-active-run-per-session constraint (`start_active_run` / `clear_active_run` in `on_prompt`).

**Secondary:** Session config option `councilMode: boolean` so the desktop toggle can persist per-session without changing the standard send path.

---

## Research task 5: Message storage representation

### Current schema

- One row per message in `messages` table (`session_id`, `content_json`, `metadata_json`)
- `MessageMetadata` has singular `inference: Option<InferenceMetadata>` (provider + model)
- Existing extension pattern: `steer: bool` flag for UI-only metadata (`message.rs` ~679–683)
- Subagent drill-down: `_meta.subagent_session_id` on tool results, not on assistant messages

### Options

| Option | Approach | Migration | UI impact |
|--------|----------|-----------|-----------|
| **A — Metadata extension** | Add `council: Option<CouncilMetadata>` to `MessageMetadata` | Additive JSON field, no DB migration | Render `CouncilMessage` when present |
| **B — New content type** | `MessageContent::CouncilResult(...)` | Breaks content schema assumptions | Cleaner typing |
| **C — New table** | `council_turns` keyed by session + turn | Full migration + second read path | Best for analytics, overkill for v1 |

### Proposed `CouncilMetadata` shape

```rust
pub struct CouncilMetadata {
    pub council_id: String,           // correlates member summaries + synthesis
    pub role: CouncilRole,            // MemberSummary | Synthesis
    pub member_label: Option<String>, // e.g. "Claude Opus 4.5"
    pub subagent_session_id: Option<String>,
    pub status: CouncilMemberStatus,  // Completed | Failed | TimedOut
}

pub enum CouncilRole { MemberSummary, Synthesis }
```

### Decision

**Option A — metadata extension.** Persist into main session:

1. User message (normal)
2. Optional progress messages (agent-only metadata, user-visible status text)
3. One **synthesis** assistant message with parsed `FinalOutputTool` JSON rendered as structured UI + `CouncilMetadata { role: Synthesis }`
4. Optional lightweight **member summary** messages (1–2 sentence preview each) with `subagent_session_id` for drill-down

Full member transcripts remain in subagent sessions (consistent with existing `delegate` pattern). Reuse `window.electron.createChatWindow({ resumeSessionId, viewType: 'pair' })` from `ToolCallWithResponse.tsx`.

---

## Research task 6: Cost, latency, and failure-handling policy

### Cost model

| Component | Calls per Council turn | Notes |
|-----------|------------------------|-------|
| Member 1..N | N full agent loops (each may include multiple LLM + tool calls) | Dominant cost |
| Synthesizer | 1 agent loop (preferably 1–2 turns with `FinalOutputTool`) | Fixed overhead |
| **Total LLM calls** | **N × (1 + tool_turns) + 1–2** | ~4× minimum for N=3, no tools |

### Latency model

- **Wall clock bounded by slowest member** (parallel fan-out)
- **Synthesis adds sequential tail** after all members complete or timeout
- Expected range: 30s–3min for research queries with tools; 10s–30s for pure completion

### Recommended policy

| Parameter | v1 default | Rationale |
|-----------|------------|-----------|
| Roster size | 3 (configurable 2–5) | Matches Perplexity; diminishing returns beyond 5 |
| Per-member timeout | 120s (configurable) | Prevents one slow provider blocking indefinitely. **Net-new plumbing** — no timeout exists in subagent infra today (see research task 1) |
| Overall turn timeout | 180s | Hard ceiling including synthesis. Also net-new |
| Partial failure | **Proceed with N−k members** | Show failed member as "Unavailable" in UI; synthesizer notes reduced confidence |
| Minimum members to synthesize | 2 | If <2 succeed, return error with per-member diagnostics |
| Rate limiting | None in v1 (open-source) | Perplexity gates to Max tier; goose can gate via config flag later |
| Cancellation | Propagate parent `CancellationToken` to all members | Already supported in `run_subagent_task` |
| Tool execution | Independent per member (3× tool cost) | User requirement; document cost clearly in UI |
| Progress streaming | Emit per-member status events ("Claude responding…", "GPT complete") | Required for acceptable UX at 30s+ latency |

### Failure UX contract

- Member timeout → `{ status: TimedOut, summary: null, subagent_session_id: <partial session> }`
- Member error → `{ status: Failed, error: "<reason>" }`
- Synthesis failure → Fall back to side-by-side member summaries only (no unified narrative); show warning banner

---

## PRD: Model Council for goose

### 1. Problem statement

Users doing high-stakes research, verification, or strategic decisions manually paste the same question into multiple LLM interfaces and mentally diff the answers. goose supports multiple providers and subagent delegation, but has no unified "ask 3 models, compare, synthesize" workflow with consensus/disagreement surfacing.

### 2. Goals

- Fan a single user query to N configurable models **in parallel**, each with full session tool access
- Produce a **synthesized answer** highlighting consensus, disagreements, and unresolved conflicts
- Preserve **per-model drill-down** to full reasoning/tool transcripts
- Integrate into **desktop UI** as an explicit user action (not agent-discretionary)
- Reuse existing goose infrastructure (`summon`, `FinalOutputTool`, `SessionManager`, ACP streaming)

### 3. Non-goals (v1)

- Mobile clients
- Inline side-by-side diff view (drill-down to subagent windows is sufficient for v1)
- Shared retrieval deduplication across members (v2 optimization)
- Plan-tier / billing gating
- CLI-first experience (CLI consumer is v1.1, thin wrapper)
- Replacing Council of Mine MCP (different product — personality debate vs cross-model verification)

### 4. Target users

- Power users with 2+ provider API keys configured
- Researchers, engineers, and decision-makers who value accuracy over speed
- goose desktop users (primary); API consumers (secondary)

### 5. User stories

| ID | Story | Acceptance |
|----|-------|------------|
| US-1 | As a user, I toggle "Model Council" before sending a message | Toggle visible in chat input; persists for session until toggled off |
| US-2 | As a user, I configure which 3 models sit on my council + synthesizer model | Settings UI lists configured providers; roster saved to config |
| US-3 | As a user, I see progress while 3 models work in parallel | Status shows per-member progress; total elapsed time visible |
| US-4 | As a user, I receive one synthesized answer with consensus/disagreement sections | Structured sections render in main chat |
| US-5 | As a user, I drill into any member's full transcript | "View session" opens subagent window (existing pattern) |
| US-6 | As a user, I continue the conversation after a Council turn | Synthesized answer becomes context for subsequent normal turns |
| US-7 | As a user, if one model fails, I still get a useful result | Synthesis proceeds with 2/3; failed member marked unavailable |

### 6. Functional requirements

#### 6.1 Configuration

```yaml
# Proposed config shape (config.yaml)
council:
  enabled: true
  # model values are illustrative — validate against the configured
  # provider's catalog at config-save time, same as the model picker does
  members:
    - provider: anthropic
      model: claude-opus-4-5
      label: "Claude Opus 4.5"       # optional display name
    - provider: openai
      model: gpt-5.1
    - provider: google
      model: gemini-3-pro
  synthesizer:
    provider: anthropic
    model: claude-sonnet-4-5          # fast/cheap synthesizer recommended
  limits:
    member_timeout_secs: 120
    turn_timeout_secs: 180
    min_successful_members: 2
```

- Roster: 2–5 members, user-configurable
- Synthesizer: separate provider/model (defaults to user's fast model if configured)
- Validates all members have configured API keys before starting turn

#### 6.2 Orchestration pipeline

```
User prompt (Council mode)
  → Persist user message
  → For each member (parallel):
      create SubAgent session
      inject parent conversation as context
      run_subagent_task(provider, model, extensions, prompt)
      collect { answer, subagent_session_id, usage, status }
  → Synthesizer session:
      input = { query, member_outputs[] }
      FinalOutputTool schema → structured synthesis JSON
  → Persist synthesis + member summaries to main session
  → Stream events to UI
```

#### 6.3 Output contract (UI rendering)

**Synthesis message sections:**

1. **Unified narrative** — primary readable answer
2. **Consensus** — bullet list
3. **Disagreements** — grouped by topic with per-model claims
4. **Unresolved** — explicit "we couldn't reconcile" items
5. **Member cards** — model name, status, 1-line preview, "View full session" link

#### 6.4 API surface

| Method | Purpose |
|--------|---------|
| `_goose/unstable/session/council/prompt` | Send Council turn (streams like `on_prompt`) |
| `_goose/unstable/council/config/read` | Read roster + synthesizer config |
| `_goose/unstable/council/config/save` | Save roster + synthesizer config |
| Session config option `councilMode` | Toggle Council for next send |

### 7. UX specification (desktop)

#### 7.1 Trigger

- Toggle in chat input footer near model picker (`ModelsBottomBar` area)
- Label: "Model Council" with brief tooltip: *"Ask 3 models, compare answers"*
- When active, send button indicates Council mode (visual distinction)

#### 7.2 Settings

- New section under provider/model settings: **Model Council**
- Member slots (3 default) with provider + model dropdowns
- Synthesizer model picker
- Timeout sliders (advanced)

#### 7.3 In-chat rendering

- New `CouncilMessage` component for synthesis messages
- Expandable sections for consensus / disagreements / unresolved
- Member row with status badge (✓ Complete, ⏱ Timed out, ✗ Failed)
- Reuse subagent session link pattern from `ToolCallWithResponse`

#### 7.4 Progress state

- New loading state distinct from single-model `LoadingGoose`
- Show 3 member status rows updating independently
- Cancel button cancels all members + synthesis

### 8. Technical architecture (recommended)

```
┌─────────────┐     council/prompt      ┌──────────────────┐
│ Desktop UI  │ ────────────────────────▶│ ACP Server       │
└─────────────┘                          │ on_council_prompt│
                                         └────────┬─────────┘
                                                  │
                                         ┌────────▼─────────┐
                                         │ CouncilOrchestrator│
                                         │ (new module)       │
                                         └───┬───────┬──────┘
                              ┌──────────────┘       └──────────────┐
                    ┌─────────▼────────┐                 ┌─────────▼────────┐
                    │ Member subagents │  ×N parallel    │ Synthesizer      │
                    │ run_subagent_task│                 │ FinalOutputTool  │
                    └─────────┬────────┘                 └─────────┬────────┘
                              │                                    │
                    ┌─────────▼────────────────────────────────────▼────────┐
                    │ Main session (metadata-tagged messages)              │
                    └──────────────────────────────────────────────────────┘
```

**Module placement:** `crates/goose/src/agents/council.rs` (orchestrator), config in `crates/goose/src/config/council.rs`

**Reuse (do not rebuild):**

- `run_subagent_task` / `TaskConfig` from subagent infrastructure
- `FinalOutputTool` for synthesis
- `SessionManager` for persistence
- ACP streaming from `on_prompt` pattern
- Desktop subagent drill-down from `ToolCallWithResponse`

**Net-new work (does not exist today — budget for it):**

- `CouncilOrchestrator` module: fan-out, join, partial-failure policy, synthesis handoff
- Wall-clock timeout enforcement per member and per turn (`tokio::time::timeout` + `CancellationToken` cancellation) — subagent infra only has `max_turns`
- Parent-context injection strategy (system-prompt vs. reconstructed message history — see research task 3 caveat) including truncation to per-model budgets
- `council` config section + validation (roster size, API-key presence, catalog model IDs)
- `CouncilMetadata` on `MessageMetadata` + serialization
- Three ACP unstable methods + `councilMode` session config option
- Desktop: toggle, settings section, `CouncilMessage` renderer, per-member progress state

### 9. Success metrics

| Metric | Target (90 days post-launch) |
|--------|------------------------------|
| Council turn completion rate | >85% (including partial member success) |
| Users who drill into ≥1 member session | >30% (validates transparency value) |
| Repeat Council usage (2+ turns in session) | >20% of Council users |
| Synthesis schema validation first-try rate | >90% |
| P95 turn latency (no tools) | <45s |
| P95 turn latency (with tools) | <120s |

### 10. Risks and mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| 3× cost surprise | User churn | Cost estimate before send; member count visible |
| Slowest-member latency | Poor UX | Per-member timeout; progress UI |
| Synthesis false consensus | Trust erosion | Prompt engineering; surface unresolved explicitly |
| Context overflow on history inject | Member failures | Truncate/summarize parent history |
| Subagent Auto mode tool abuse | Unintended actions | Same trust model as delegate today; document |
| Council of Mine confusion | Wrong expectations | Clear naming; link to difference in docs |

### 11. Implementation phases (post-PRD)

These are **sequencing recommendations**, not research deliverables:

1. **Orchestrator + config** — backend only, CLI/dev test hook; includes timeout plumbing and the context-injection mechanism decision (system prompt vs. message history)
2. **ACP methods + streaming** — wire to desktop transport
3. **Desktop UI** — toggle, settings, `CouncilMessage`, progress
4. **CLI** — thin `/council` command
5. **v2** — shared retrieval, inline compare view, analytics table

### 12. Open questions deferred to implementation

- Exact synthesis system prompt (needs eval against real member outputs)
- Context injection mechanism: system-prompt `# Reference Context` vs. reconstructed message history (see research task 3 caveat)
- Whether member summaries are auto-generated (extra LLM call) or first-paragraph extraction
- Compaction interaction: does Council turn count toward auto-compaction thresholds 3×?
- Recipe integration: can a recipe declare `council: true`?

---

## Appendix A: Comparison with Perplexity Model Council

| Capability | Perplexity (2026 docs) | goose PRD v1 |
|------------|------------------------|--------------|
| Fan-out to 3 models | Yes | Yes (2–5 configurable) |
| Synthesizer | Yes (undocumented model) | Yes (`FinalOutputTool`) |
| Consensus/disagreement UI | Yes | Yes (structured sections) |
| Per-model drill-down | Undocumented | Yes (subagent sessions) |
| Tool-using members | Undocumented | Yes (full extensions) |
| Shared search context | Undocumented | No (v2) |
| Gating | Max / Enterprise Max | Open (config flag optional) |
| Mobile | Coming soon | Not in v1 |

## Appendix B: Comparison with Council of Mine MCP

| Dimension | Council of Mine | Model Council (proposed) |
|-----------|-----------------|--------------------------|
| Trigger | Agent calls MCP tool | User toggles Council mode |
| Members | 9 fixed personalities (same underlying model likely) | 3 user-chosen providers/models |
| Mechanism | MCP extension | Native orchestrator |
| Tools | MCP tool only | Full goose extensions per member |
| Output | Vote + synthesis | Consensus/disagreement schema |
| Use case | Debate / decision framing | Cross-model verification |

## Appendix C: Key source files

| Component | Path |
|-----------|------|
| Provider trait | `crates/goose-providers/src/base.rs` |
| Subagent runner | `crates/goose/src/agents/subagent_handler.rs` |
| Delegate / summon | `crates/goose/src/agents/platform_extensions/summon.rs` |
| Parallel tool execution | `crates/goose/src/agents/agent.rs` |
| FinalOutputTool | `crates/goose/src/agents/final_output_tool.rs` |
| Message metadata | `crates/goose-providers/src/conversation/message.rs` |
| Session storage | `crates/goose/src/session/session_manager.rs` |
| ACP prompt handler | `crates/goose/src/acp/server.rs` |
| ACP custom methods | `crates/goose/acp-meta.json` |
| Desktop prompt | `ui/desktop/src/acp/prompt.ts` |
| Subagent drill-down UI | `ui/desktop/src/components/ToolCallWithResponse.tsx` |

---

## Research decisions summary

| # | Question | Decision |
|---|----------|----------|
| 1 | Fan-out feasible? | **Yes** — subagent parallel execution + SQLite WAL |
| 2 | Structured synthesis? | **Yes** — `FinalOutputTool` with defined schema |
| 3 | Shared context? | **Yes** — parent history shared with each member (injection mechanism decided in Phase 1), independent tools |
| 4 | Trigger surface? | **ACP unstable method** + session config toggle |
| 5 | Message storage? | **Metadata extension** on existing messages |
| 6 | Failure policy? | **Graceful degradation**, min 2 members, 120s/180s timeouts |

**Next step:** Implementation planning and eval harness for synthesis prompt quality (Phase 1 in section 11).
