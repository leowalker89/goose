"""Minimal goose SDK demo: ask the agent to ping apple.com."""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "generated"))

from goose_uniffi import Agent, AgentEvent, EventSink, ExtensionSpec, ProviderSpec  # noqa: E402


@dataclass(frozen=True)
class Style:
    dim: str = "\033[2m"
    cyan: str = "\033[36m"
    green: str = "\033[32m"
    red: str = "\033[31m"
    reset: str = "\033[0m"

    def paint(self, color: str, text: str) -> str:
        return f"{color}{text}{self.reset}"


S = Style()


def _preview(output: str, *, max_lines: int = 3, max_width: int = 100) -> str:
    lines = (line[:max_width] for line in output.splitlines() if line.strip())
    return "\n  ".join(list(lines)[:max_lines])


class Printer(EventSink):
    """Pretty-prints agent events to the terminal."""

    def __init__(self) -> None:
        self._mid_text = False

    def on_event(self, event: AgentEvent) -> None:
        if isinstance(event, AgentEvent.ASSISTANT_TEXT):
            print(event.text, end="", flush=True)
            self._mid_text = True
            return

        self._end_text_line()

        if isinstance(event, AgentEvent.TOOL_REQUEST):
            args = event.arguments.replace("\n", " ")[:120]
            print(f"{S.paint(S.cyan, '→ ' + event.name)} {S.paint(S.dim, args)}", flush=True)

        elif isinstance(event, AgentEvent.TOOL_RESPONSE):
            color = S.red if event.is_error else S.green
            marker = "✗" if event.is_error else "✓"
            print(f"{S.paint(color, marker)} {S.paint(S.dim, _preview(event.output))}\n", flush=True)

    def on_error(self, error: str) -> None:
        print(f"\n{S.paint(S.red, 'error:')} {error}", file=sys.stderr)

    def on_done(self) -> None:
        self._end_text_line()

    def _end_text_line(self) -> None:
        if self._mid_text:
            print()
            self._mid_text = False


def main() -> None:
    print(S.paint(S.dim, "configuring agent…"), file=sys.stderr)

    agent = Agent()
    agent.configure(
        ProviderSpec(
            name=os.environ.get("GOOSE_PROVIDER"),
            model=os.environ.get("GOOSE_MODEL"),
        ),
        [ExtensionSpec.BUILTIN(name="developer")],
    )

    print(S.paint(S.dim, "> ping apple.com") + "\n", file=sys.stderr)
    agent.reply("ping apple.com", Printer())


if __name__ == "__main__":
    main()
