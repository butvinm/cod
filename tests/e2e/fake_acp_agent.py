#!/usr/bin/env python3
"""A scripted fake agent for Cod's end-to-end tests (story #28).

It speaks the Agent Client Protocol (ACP, protocol version 1) as JSON-RPC 2.0 over stdio, the same interface Zed's agent panel uses for Claude Code and Codex, so the app cannot tell it from a real external agent.
It uses the Python standard library only.

Cod starts it as a custom agent from the test settings (`agent_servers`, see `fake_agent_settings`). The behavior comes from a JSON script named by `COD_FAKE_ACP_SCRIPT`:

    {
      "session_ids": ["claude-session-1", "claude-session-2"],
      "turns": [
        {"steps": [
          {"chunk": "See src/invoice.rs:18 and https://tracker.example.com/PAY-1234"},
          {"title": "fix invoice rounding"},
          {"permission": "Allow edit to src/invoice.rs?"},
          {"wait": "finish-1"},
          {"sleep": 0.2},
          {"end": "end_turn"}
        ]},
        {"steps": [{"error": "the fake agent failed"}]}
      ]
    }

- `session_ids`: the ids `session/new` returns, in order (the provider's own session ids; the app must keep them, never make its own). After the list runs out, `fake-session-<n>` is used.
- `turns`: one entry per `session/prompt`, in order over all sessions. A prompt without a turn left gets a single chunk "(no scripted reply)" and ends the turn.
- Steps: `chunk` streams agent text (`agent_message_chunk`), `thought` streams agent thought text, `title` sends `session_info_update` with that title, `permission` sends a pending edit tool call and asks `session/request_permission` (the answer is recorded in the state file), `wait` blocks until the file of that name exists in the control folder, `sleep` waits that many seconds, `end` ends the turn with that stop reason (default `end_turn`), `error` fails the prompt with a JSON-RPC error carrying that message.
- A turn without `end` or `error` ends with `end_turn` after its last step.

Optional environment:

- `COD_FAKE_ACP_STATE`: a JSON file that keeps the sessions (ids, titles, history), the next turn and the permission answers across agent processes, so `session/load` can replay a session after Cod restarts. Default: no state is kept.
- `COD_FAKE_ACP_CONTROL`: the control folder for `wait` steps. Default: the folder of the script.
- `COD_FAKE_ACP_LOG`: a file that gets one line per message in and out, for debugging failed tests.
"""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
import time
from pathlib import Path
from typing import Any

PROTOCOL_VERSION = 1
METHOD_NOT_FOUND = -32601
INTERNAL_ERROR = -32603
NO_REPLY_TEXT = "(no scripted reply)"
WAIT_POLL = 0.05


class Agent:
    def __init__(self, script: dict[str, Any], state_path: Path | None, control: Path, log_path: Path | None) -> None:
        self.script = script
        self.state_path = state_path
        self.control = control
        self.log_path = log_path
        self.write_lock = threading.Lock()
        self.state_lock = threading.Lock()
        self.next_request_id = 1
        self.pending: dict[int, queue.Queue[dict[str, Any]]] = {}
        self.cancelled: set[str] = set()
        self.state = self.load_state()

    # Transport

    def log(self, direction: str, message: dict[str, Any]) -> None:
        if self.log_path is None:
            return
        with self.log_path.open("a", encoding="utf-8") as log:
            log.write(f"{direction} {json.dumps(message)}\n")

    def send(self, message: dict[str, Any]) -> None:
        message = {"jsonrpc": "2.0", **message}
        self.log("out", message)
        with self.write_lock:
            sys.stdout.write(json.dumps(message) + "\n")
            sys.stdout.flush()

    def reply(self, request_id: Any, result: Any) -> None:
        self.send({"id": request_id, "result": result})

    def reply_error(self, request_id: Any, code: int, message: str) -> None:
        self.send({"id": request_id, "error": {"code": code, "message": message}})

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self.send({"method": method, "params": params})

    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        """Sends a request to the client and blocks until its response arrives."""
        with self.write_lock:
            request_id = self.next_request_id
            self.next_request_id += 1
            answer: queue.Queue[dict[str, Any]] = queue.Queue()
            self.pending[request_id] = answer
        self.send({"id": request_id, "method": method, "params": params})
        return answer.get()

    def session_update(self, session_id: str, update: dict[str, Any]) -> None:
        self.notify("session/update", {"sessionId": session_id, "update": update})

    # State

    def load_state(self) -> dict[str, Any]:
        state: dict[str, Any] = {"sessions": {}, "next_turn": 0, "created": 0, "permissions": []}
        if self.state_path is not None and self.state_path.exists():
            state.update(json.loads(self.state_path.read_text(encoding="utf-8")))
        return state

    def save_state(self) -> None:
        if self.state_path is None:
            return
        temporary = self.state_path.with_suffix(self.state_path.suffix + ".tmp")
        temporary.write_text(json.dumps(self.state, indent=2) + "\n", encoding="utf-8")
        temporary.replace(self.state_path)

    def record(self, session_id: str, role: str, text: str) -> None:
        with self.state_lock:
            session = self.state["sessions"].setdefault(session_id, {"title": None, "history": []})
            history = session["history"]
            if history and history[-1]["role"] == role:
                history[-1]["text"] += text
            else:
                history.append({"role": role, "text": text})
            self.save_state()

    # Requests from the client

    def handle(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        request_id = message.get("id")
        if method is None:
            # A response to a request of ours.
            answer = self.pending.pop(request_id, None)
            if answer is not None:
                answer.put(message)
            return
        params = message.get("params") or {}
        if method == "initialize":
            self.reply(
                request_id,
                {
                    "protocolVersion": PROTOCOL_VERSION,
                    "agentCapabilities": {
                        "loadSession": True,
                        "promptCapabilities": {"image": False, "audio": False, "embeddedContext": False},
                    },
                    "authMethods": [],
                },
            )
        elif method == "session/new":
            self.reply(request_id, {"sessionId": self.new_session_id()})
        elif method == "session/load":
            self.load_session(request_id, params)
        elif method == "session/prompt":
            threading.Thread(target=self.prompt, args=(request_id, params), daemon=True).start()
        elif method == "session/cancel":
            self.cancelled.add(params.get("sessionId", ""))
        elif request_id is not None:
            self.reply_error(request_id, METHOD_NOT_FOUND, f"method not supported by the fake agent: {method}")

    def new_session_id(self) -> str:
        with self.state_lock:
            index = self.state["created"]
            self.state["created"] = index + 1
            ids = self.script.get("session_ids") or []
            session_id = ids[index] if index < len(ids) else f"fake-session-{index + 1}"
            self.state["sessions"].setdefault(session_id, {"title": None, "history": []})
            self.save_state()
        return session_id

    def load_session(self, request_id: Any, params: dict[str, Any]) -> None:
        session_id = params.get("sessionId", "")
        session = self.state["sessions"].get(session_id)
        if session is None:
            self.reply_error(request_id, INTERNAL_ERROR, f"unknown session: {session_id}")
            return
        for entry in session["history"]:
            kind = "user_message_chunk" if entry["role"] == "user" else "agent_message_chunk"
            self.session_update(session_id, {"sessionUpdate": kind, "content": {"type": "text", "text": entry["text"]}})
        if session.get("title"):
            self.session_update(session_id, {"sessionUpdate": "session_info_update", "title": session["title"]})
        self.reply(request_id, {})

    def next_turn(self) -> dict[str, Any] | None:
        with self.state_lock:
            index = self.state["next_turn"]
            self.state["next_turn"] = index + 1
            self.save_state()
        turns = self.script.get("turns") or []
        return turns[index] if index < len(turns) else None

    def prompt(self, request_id: Any, params: dict[str, Any]) -> None:
        session_id = params.get("sessionId", "")
        self.cancelled.discard(session_id)
        prompt_text = "".join(block.get("text", "") for block in params.get("prompt", []) if block.get("type") == "text")
        self.record(session_id, "user", prompt_text)
        turn = self.next_turn()
        steps = turn["steps"] if turn is not None else [{"chunk": NO_REPLY_TEXT}]
        try:
            for step in steps:
                if session_id in self.cancelled:
                    self.reply(request_id, {"stopReason": "cancelled"})
                    return
                if self.run_step(request_id, session_id, step):
                    return
        except Exception as error:  # A broken script must fail the turn, not hang it.
            self.reply_error(request_id, INTERNAL_ERROR, f"fake agent script failed: {error}")
            return
        self.reply(request_id, {"stopReason": "end_turn"})

    def run_step(self, request_id: Any, session_id: str, step: dict[str, Any]) -> bool:
        """Runs one step. Returns true when the step ended the turn."""
        if "chunk" in step:
            text = str(step["chunk"])
            self.session_update(session_id, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})
            self.record(session_id, "agent", text)
        elif "thought" in step:
            self.session_update(
                session_id,
                {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": str(step["thought"])}},
            )
        elif "title" in step:
            with self.state_lock:
                self.state["sessions"].setdefault(session_id, {"title": None, "history": []})["title"] = step["title"]
                self.save_state()
            self.session_update(session_id, {"sessionUpdate": "session_info_update", "title": step["title"]})
        elif "permission" in step:
            self.ask_permission(session_id, str(step["permission"]))
        elif "wait" in step:
            target = self.control / str(step["wait"])
            while not target.exists():
                if session_id in self.cancelled:
                    break
                time.sleep(WAIT_POLL)
        elif "sleep" in step:
            time.sleep(float(step["sleep"]))
        elif "end" in step:
            self.reply(request_id, {"stopReason": step["end"] or "end_turn"})
            return True
        elif "error" in step:
            self.reply_error(request_id, INTERNAL_ERROR, str(step["error"]))
            return True
        else:
            raise ValueError(f"unknown step: {json.dumps(step)}")
        return False

    def ask_permission(self, session_id: str, title: str) -> None:
        with self.state_lock:
            tool_call_id = f"fake-tool-{len(self.state['permissions']) + 1}"
        tool_call = {"toolCallId": tool_call_id, "title": title, "kind": "edit", "status": "pending"}
        self.session_update(session_id, {"sessionUpdate": "tool_call", **tool_call})
        response = self.request(
            "session/request_permission",
            {
                "sessionId": session_id,
                "toolCall": tool_call,
                "options": [
                    {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                ],
            },
        )
        outcome = (response.get("result") or {}).get("outcome") or {}
        answer = outcome.get("optionId") if outcome.get("outcome") == "selected" else "cancelled"
        with self.state_lock:
            self.state["permissions"].append({"session": session_id, "title": title, "answer": answer})
            self.save_state()
        status = "completed" if answer == "allow" else "failed"
        self.session_update(session_id, {"sessionUpdate": "tool_call_update", "toolCallId": tool_call_id, "status": status})

    def run(self) -> None:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            self.log("in", message)
            self.handle(message)


def main() -> int:
    script_path = os.environ.get("COD_FAKE_ACP_SCRIPT")
    script: dict[str, Any] = {}
    control = Path.cwd()
    if script_path:
        script = json.loads(Path(script_path).read_text(encoding="utf-8"))
        control = Path(script_path).parent
    if os.environ.get("COD_FAKE_ACP_CONTROL"):
        control = Path(os.environ["COD_FAKE_ACP_CONTROL"])
    state = os.environ.get("COD_FAKE_ACP_STATE")
    log = os.environ.get("COD_FAKE_ACP_LOG")
    Agent(script, Path(state) if state else None, control, Path(log) if log else None).run()
    return 0


def fake_agent_settings(script: Path, state: Path | None = None, log: Path | None = None) -> dict[str, Any]:
    """The `agent_servers` entry that starts this fake agent as a custom agent with the given script."""
    env = {"COD_FAKE_ACP_SCRIPT": str(script)}
    if state is not None:
        env["COD_FAKE_ACP_STATE"] = str(state)
    if log is not None:
        env["COD_FAKE_ACP_LOG"] = str(log)
    return {"type": "custom", "command": sys.executable, "args": [str(Path(__file__).resolve())], "env": env}


if __name__ == "__main__":
    sys.exit(main())
