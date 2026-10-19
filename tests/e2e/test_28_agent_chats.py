"""Story #28: agent chats belong to a workspace, with keyboard-reachable links.

The chats talk to the scripted fake agent in fake_acp_agent.py (ACP over stdio, the interface of Claude Code and Codex in Zed's agent panel), registered in the test settings as the custom agent `claude-acp`.
This file holds the representative path of AC5 and AC6 (links and file mentions in a reply, reached and opened from the keyboard); the workspace criteria follow with the status contract.
"""

from __future__ import annotations

import json
import os
import unittest
from typing import Any

from cod_e2e import CodTestCase
from fake_acp_agent import fake_agent_settings

AGENT_ID = "claude-acp"

INVOICE_LINES = [
    "pub struct Invoice {",
    "    pub number: u32,",
    "    pub lines: Vec<u64>,",
    "}",
    "",
    "impl Invoice {",
    "    pub fn new(number: u32) -> Self {",
    "        Self { number, lines: Vec::new() }",
    "    }",
    "",
    "    pub fn add(&mut self, cents: u64) {",
    "        self.lines.push(cents);",
    "    }",
    "}",
    "",
    "/// Totals the lines and rounds to whole cents.",
    "pub fn total(invoice: &Invoice) -> u64 {",
    "    let sum: u64 = invoice.lines.iter().sum();",
    "    sum",
    "}",
]

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": "\n".join(INVOICE_LINES) + "\n",
}

TRACKER_URL = "https://tracker.example.com/PAY-1234"
GUIDE_URL = "https://wiki.example.com/billing/rounding"
GUIDE_LINK = f"[the rounding guide]({GUIDE_URL})"
REPLY = (
    f"The rounding happens in src/invoice.rs:18. The ticket is {TRACKER_URL} "
    f"and the rule is in {GUIDE_LINK}."
)


def chat(state: dict[str, Any]) -> dict[str, Any]:
    return state.get("chat") or {}


def focused_link_text(state: dict[str, Any]) -> str | None:
    link = chat(state).get("focused_link")
    return link["text"] if link else None


class AgentChatLinksTest(CodTestCase):
    def test_links_and_file_mentions_in_a_reply_are_reached_and_opened_from_the_keyboard(self) -> None:
        with self.step("create the fixture repo billing-api and the agent script"):
            repo = self.make_repo("billing-api", BILLING_API)
            script = self.workdir / "agent-script.json"
            script.write_text(
                json.dumps(
                    {
                        "session_ids": ["claude-session-pay-1234"],
                        "turns": [{"steps": [{"chunk": REPLY}, {"end": "end_turn"}]}],
                    }
                )
            )
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}

        with self.step("start Cod on billing-api"):
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )

        with self.step("check that no chat of the fake agent is open"):
            start = self.state()
            self.expect_equal("chat agent", False, chat(start).get("agent") == AGENT_ID)
            self.expect_equal("tabs", [], start["tabs"])

        with self.step("open the agent panel and start a chat with the fake agent from the new thread menu"):
            # The agent commands join the palette once the panel has loaded, a moment after the window.
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            # The menu ends with the external agents and "Add More Agents"; the fake agent is the only external agent.
            self.keys("up up enter")
            self.wait_for(lambda state: chat(state).get("agent") == AGENT_ID, 30, "a chat with the fake agent")

        with self.step("send a prompt and wait for the scripted reply"):
            self.type("Where does the rounding happen?")
            self.keys("enter")
            self.wait_for(
                lambda state: TRACKER_URL in chat(state).get("transcript", "") and GUIDE_URL in chat(state).get("transcript", ""),
                30,
                "the scripted reply in the transcript",
            )

        with self.step("move the focus from the message box into the reply"):
            self.keys("alt-up")
            state = self.wait_for(lambda state: chat(state).get("reply_focused") is True, 10, "the reply to take the focus")
            self.expect_equal("focused link", None, focused_link_text(state))

        with self.step("press tab through the links in reading order"):
            for expected in ["src/invoice.rs:18", TRACKER_URL, GUIDE_LINK]:
                self.keys("tab")
                self.wait_for(lambda state, expected=expected: focused_link_text(state) == expected, 10, f"the focus on {expected}")

        with self.step("press shift-tab back to the file mention"):
            for expected in [TRACKER_URL, "src/invoice.rs:18"]:
                self.keys("shift-tab")
                self.wait_for(lambda state, expected=expected: focused_link_text(state) == expected, 10, f"the focus on {expected}")
            link = chat(self.state())["focused_link"]
            self.expect_equal("file mention leads to a file", True, link["url"].startswith("file://") and "invoice.rs" in link["url"])

        with self.step("press enter on the file mention"):
            self.keys("enter")
            self.wait_for_values(
                "opened file",
                {"active_tab": "invoice.rs", "row": 18},
                lambda state: {"active_tab": state["active_tab"], "row": (state["cursor"] or {}).get("row")},
                10,
            )

        with self.step("check that the file opened in the same workspace"):
            state = self.state()
            self.expect_equal("workspace count", 1, state["workspace_count"])
            self.expect_equal("window count", 1, state["window_count"])


PAY_SESSION = "claude-session-pay-1234"
LEDGER_SESSION = "claude-session-ledger-1300"
PAY_REPLY = "The rounding happens in src/invoice.rs:18."
LEDGER_REPLY = "The ledger export reads src/main.rs first."

NEW_WORKSPACE = "workspace: new workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"
ADD_FOLDER = "workspace: add folder to project"
SHOW_TIMEOUT = 15.0
RESTORE_TIMEOUT = 45.0


def real(path: str | os.PathLike[str]) -> str:
    return os.path.realpath(path)


def chat_of(state: dict[str, Any]) -> dict[str, Any]:
    """The active workspace and what its active chat shows: agent, provider session id and which scripted replies its transcript holds."""
    transcript = chat(state).get("transcript", "")
    return {
        "active_workspace": state["active_workspace"],
        "agent": chat(state).get("agent"),
        "session_id": chat(state).get("session_id"),
        "pay_reply": PAY_REPLY in transcript,
        "ledger_reply": LEDGER_REPLY in transcript,
    }


class WorkspaceChatsTest(CodTestCase):
    def start_chat(self, prompt: str, reply: str, other_sessions: list[str]) -> str:
        """Opens the agent panel of the active workspace, starts a chat with the fake agent from the new thread menu, sends the prompt, waits for the reply and returns the chat's session id."""
        # The agent commands join the palette once the panel has loaded, a moment after the workspace.
        self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
        self.run_command("agent: toggle focus")
        self.run_command("agent: toggle new thread menu")
        # A chat that was already open (for example the fresh chat of a new workspace) is not the new one.
        known = [*other_sessions, *self.provider_sessions(), chat(self.state()).get("session_id")]
        # The menu ends with the external agents and "Add More Agents"; the fake agent is the only external agent.
        self.keys("up up enter")
        self.wait_for(
            lambda state: chat(state).get("agent") == AGENT_ID and chat(state).get("session_id") not in [None, "", *known],
            30,
            "a new chat with the fake agent",
        )
        self.type(prompt)
        self.keys("enter")
        state = self.wait_for(lambda state: reply in chat(state).get("transcript", ""), 30, "the scripted reply in the transcript")
        return chat(state)["session_id"]

    def provider_sessions(self) -> list[str]:
        """The session ids the fake agent itself made, from its state file."""
        path = self.workdir / "agent-state.json"
        return sorted(json.loads(path.read_text())["sessions"]) if path.exists() else []

    def start_fake_agent_cod(self, persistent: bool) -> Any:
        """Creates billing-api and the agent script with two scripted replies, starts Cod on billing-api with the fake agent and returns the repo."""
        repo = self.make_repo("billing-api", BILLING_API)
        script = self.workdir / "agent-script.json"
        script.write_text(
            json.dumps(
                {
                    "session_ids": [PAY_SESSION, LEDGER_SESSION],
                    "turns": [
                        {"steps": [{"chunk": PAY_REPLY}, {"end": "end_turn"}]},
                        {"steps": [{"chunk": LEDGER_REPLY}, {"end": "end_turn"}]},
                    ],
                }
            )
        )
        agent_servers = {
            AGENT_ID: fake_agent_settings(script, state=self.workdir / "agent-state.json", log=self.workdir / "agent.log")
        }
        self.start_cod(repo, persistent=persistent, extra_settings={"agent_servers": agent_servers})
        self.wait_for(
            lambda state: [real(path) for path in state["worktrees"]] == [real(repo)],
            30,
            "the billing-api folder to open",
        )
        return repo

    def test_ac2_a_new_workspace_opens_with_a_fresh_chat_ready_for_typing(self) -> None:
        with self.step("start Cod on billing-api with the fake agent"):
            self.start_fake_agent_cod(persistent=False)

        with self.step("workspace A: chat with the fake agent, which makes it the default agent"):
            a_session = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])

        with self.step(f"run {NEW_WORKSPACE!r}: the new workspace shows a fresh chat of the default agent with the focus in it"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            state = self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1
                and state["active_workspace"] != before["active_workspace"]
                and state["agent_panel_focused"] is True
                and chat(state).get("agent") == AGENT_ID
                and chat(state).get("session_id") not in [None, "", a_session],
                30,
                "a fresh chat of the fake agent in the new workspace, with the focus",
            )
            self.expect_equal("fresh chat is empty", False, PAY_REPLY in chat(state).get("transcript", ""))

        with self.step("type a prompt right away and press enter: the fresh chat sends it"):
            self.type("What does the ledger export read?")
            self.keys("enter")
            self.wait_for(lambda state: LEDGER_REPLY in chat(state).get("transcript", ""), 30, "the scripted reply in the fresh chat")

    def add_folder(self, folder: os.PathLike[str]) -> None:
        """Adds the folder to the active workspace with Zed's "add folder to project" and Zed's own path picker, as in test_24."""
        self.run_command(ADD_FOLDER)
        self.wait_for(lambda state: state["path_prompt"] is not None, 10, "the path picker to open")
        self.keys("ctrl-a")
        self.type(str(folder))
        self.wait_for(
            lambda state: state["path_prompt"] is not None
            and state["path_prompt"]["query"] == str(folder)
            and state["path_prompt"]["match_count"] == 1,
            10,
            "the path picker to list exactly the folder",
        )
        self.keys("enter")
        self.wait_for(
            lambda state: [real(path) for path in state["worktrees"]] == [real(folder)] and state["path_prompt"] is None,
            SHOW_TIMEOUT,
            "the folder to be added",
        )

    def test_ac1_ac4_two_workspaces_on_the_same_folder_keep_their_own_chat_across_a_restart(self) -> None:
        with self.step("start Cod on billing-api with the fake agent, keeping its session"):
            repo = self.start_fake_agent_cod(persistent=True)

        with self.step("workspace A: start a chat with the fake agent and wait for its reply"):
            a_index = self.state()["active_workspace"]
            a_session = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])
            a_chat = {"active_workspace": a_index, "agent": AGENT_ID, "session_id": a_session, "pay_reply": True, "ledger_reply": False}
            self.wait_for_values("workspace A's chat", a_chat, chat_of, 30)
            self.expect_equal("A's session id is the provider's own", True, a_session in self.provider_sessions())

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add billing-api too, start a chat and wait for its reply"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            state = self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1 and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )
            b_index = state["active_workspace"]
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")
            self.add_folder(repo)
            b_session = self.start_chat("What does the ledger export read?", LEDGER_REPLY, [a_session])
            b_chat = {"active_workspace": b_index, "agent": AGENT_ID, "session_id": b_session, "pay_reply": False, "ledger_reply": True}
            self.wait_for_values("workspace B's chat", b_chat, chat_of, 30)
            self.expect_equal("B's session id is the provider's own", True, b_session in self.provider_sessions())

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r}: the panel shows A's chat only"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for_values("workspace A's chat", a_chat, chat_of, SHOW_TIMEOUT)

        with self.step("go back to workspace B, quit Cod with ctrl-q and start it again"):
            self.run_command("workspace: next workspace")
            self.wait_for_values("workspace B's chat", b_chat, chat_of, SHOW_TIMEOUT)
            self.restart_cod()

        with self.step("check that workspace B is back with its own chat, loaded by its provider session id"):
            self.wait_for_values("workspace B's restored chat", b_chat, chat_of, RESTORE_TIMEOUT)

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r} and check that its own chat is back"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for_values("workspace A's restored chat", a_chat, chat_of, RESTORE_TIMEOUT)

        with self.step("check that the fake agent was asked to load both sessions by their own ids"):
            loaded = set()
            for line in (self.workdir / "agent.log").read_text().splitlines():
                direction, _, message = line.partition(" ")
                if direction == "in" and json.loads(message).get("method") == "session/load":
                    loaded.add(json.loads(message)["params"]["sessionId"])
            self.expect_equal("loaded sessions", sorted([a_session, b_session]), sorted(loaded))


if __name__ == "__main__":
    unittest.main()
