# goose-uniffi

PoC multi-language SDK for the Goose Rust agent core, using [UniFFI](https://mozilla.github.io/uniffi-rs/).

This crate compiles the `goose` Rust agent into one dynamic library plus generated bindings for **Python** and **Kotlin**. The same `libgoose_uniffi` powers every language, so adding a feature in Rust ships to all of them.

> **TypeScript:** UniFFI core does not ship a TS backend, and the React-Native-only `uniffi-bindgen-react-native` doesn't fit a plain-Node use case. For TS clients today, use the existing ACP-over-stdio path in [`crates/goose-sdk`](../crates/goose-sdk) / [`ui/sdk`](../ui/sdk) instead.

This sits next to two other client paths:

| Path | Transport | Used by |
|---|---|---|
| v1 | HTTP (`goose-server`) | Electron desktop |
| v2 | ACP over stdio/HTTP/WS (`crates/goose/src/acp`) | `ui/text` TUI, `ui/sdk` TS client, `crates/goose-sdk` Rust client |
| **this PoC** | **In-process FFI** (uniffi) | Any host language embedding goose |

## API surface

Deliberately tiny:

```rust
Agent::new()
Agent::configure(ProviderSpec, Vec<ExtensionSpec>) -> session_id
Agent::reply(prompt, EventSink)              // streaming via callback
Agent::reply_collect(prompt) -> Vec<AgentEvent>  // one-shot
```

`EventSink` is a foreign callback interface — the host implements it, Rust invokes it for every assistant chunk, tool request, tool response, thinking, plus `on_done` / `on_error`.

## Quick start

Each example sends the prompt `"ping apple.com"` to a configured agent and prints the streamed response.

```bash
cd sdk
just            # list recipes
just python     # build, generate bindings, run ping_apple.py
just kotlin     # build, generate bindings, fetch jna, compile, run PingApple.kt
```

Prereqs: a Rust toolchain, `python3`, `kotlinc` (`brew install kotlin`) + a JDK, and one `goose configure`-d provider on your machine. Override at runtime with `GOOSE_PROVIDER=openai GOOSE_MODEL=gpt-4o`.

## Code examples

All three speak the same shape: build an `Agent`, `configure` it with a provider, implement an `EventSink`, call `reply`.

`AgentEvent` is a tagged union — `AssistantText { text }`, `Thinking { text }`, `ToolRequest { id, name, arguments }`, `ToolResponse { id, output, isError }`.

### Python — [`examples/ping_apple.py`](examples/ping_apple.py)

```python
from goose_uniffi import Agent, ProviderSpec, ExtensionSpec, EventSink, AgentEvent

class Printer(EventSink):
    def on_event(self, event):
        if isinstance(event, AgentEvent.ASSISTANT_TEXT):
            print(event.text, end="", flush=True)
        elif isinstance(event, AgentEvent.TOOL_REQUEST):
            print(f"→ {event.name} {event.arguments}")
    def on_error(self, error): print(f"error: {error}")
    def on_done(self): pass

agent = Agent()
agent.configure(
    ProviderSpec(name="anthropic", model="claude-sonnet-4-5"),
    [ExtensionSpec.BUILTIN(name="developer")],
)
agent.reply("ping apple.com", Printer())
```

Run with `just python`.

### Kotlin — [`examples/PingApple.kt`](examples/PingApple.kt)

```kotlin
import uniffi.goose_uniffi.*

class Printer : EventSink {
    override fun onEvent(event: AgentEvent) = when (event) {
        is AgentEvent.AssistantText -> print(event.text)
        is AgentEvent.ToolRequest   -> println("→ ${event.name} ${event.arguments}")
        else -> {}
    }
    override fun onError(error: String) = System.err.println("error: $error")
    override fun onDone() {}
}

fun main() {
    val agent = Agent()
    agent.configure(
        ProviderSpec(name = "anthropic", model = "claude-sonnet-4-5"),
        listOf(ExtensionSpec.Builtin(name = "developer")),
    )
    agent.reply("ping apple.com", Printer())
}
```

Run with `just kotlin`.

## Out of scope (for now)

Extension/MCP wiring, permission/elicitation callbacks, session resume, recipes, hooks, mode switching, mid-stream cancellation. All present in the underlying `goose` crate — just not bound yet.
