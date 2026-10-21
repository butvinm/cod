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
from page_server import PageServer

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
GUIDE_TEXT = "the rounding guide"
GUIDE_LINK = f"[{GUIDE_TEXT}]({GUIDE_URL})"
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
    def setUp(self) -> None:
        super().setUp()
        self.server = PageServer()
        self.addCleanup(self.server.close)

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
            self.keys("end up enter")
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
            for expected in ["src/invoice.rs:18", TRACKER_URL, GUIDE_TEXT]:
                self.keys("tab")
                self.wait_for(lambda state, expected=expected: focused_link_text(state) == expected, 10, f"the focus on {expected}")
            self.expect_equal("guide link target", GUIDE_URL, chat(self.state())["focused_link"]["url"])

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

    def test_ac6_enter_on_a_focused_url_opens_it_in_a_page_tab_like_a_click(self) -> None:
        """#28 AC6, the URL half: Enter on the focused URL of a reply opens it the same way a click does, a page tab (#32)."""
        page_url = self.server.url("/tracker/PAY-1234")
        page_title = "PAY-1234 Fix invoice rounding"
        reply = f"The ticket is [the tracker]({page_url})."

        with self.step("create the fixture repo billing-api and an agent script whose reply links to the local page"):
            repo = self.make_repo("billing-api", BILLING_API)
            script = self.workdir / "agent-script.json"
            script.write_text(
                json.dumps(
                    {
                        "session_ids": ["claude-session-pay-1234"],
                        "turns": [{"steps": [{"chunk": reply}, {"end": "end_turn"}]}],
                    }
                )
            )
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}

        with self.step("start Cod on billing-api, start a chat with the fake agent and send a prompt"):
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            self.keys("end up enter")
            self.wait_for(lambda state: chat(state).get("agent") == AGENT_ID, 30, "a chat with the fake agent")
            self.type("Where is the ticket?")
            self.keys("enter")
            self.wait_for(lambda state: page_url in chat(state).get("transcript", ""), 30, "the scripted reply in the transcript")

        with self.step("move the focus into the reply and tab to its one link"):
            self.keys("alt-up")
            self.wait_for(lambda state: chat(state).get("reply_focused") is True, 10, "the reply to take the focus")
            self.keys("tab")
            self.wait_for(lambda state: focused_link_text(state) == "the tracker", 10, "the focus on the tracker link")

        with self.step("press enter: the page opens in a tab, the same way a click on the link would"):
            self.keys("enter")
            self.wait_for(lambda state: state["active_tab"] == page_title, 30, "the page to open in a tab and become active")

        with self.step("check the page tab"):
            state = self.state()
            page = next((page for page in state["pages"]["open"] if page["url"] == page_url), None)
            assert page is not None, f"expected a page tab for {page_url!r}, actual pages: {state['pages']['open']}"
            self.expect_equal("page title", page_title, page["title"])
            self.expect_equal("page active", True, page["active"])
            self.expect_equal("window count", 1, state["window_count"])

    def test_links_in_an_earlier_reply_stay_keyboard_reachable_after_a_second_reply_arrives(self) -> None:
        """RC review B7/C4: Tab/Shift-Tab used to stay inside the newest reply's last block, so a second reply's arrival made the first reply's links unreachable from the keyboard. Shift-Tab must keep moving back across reply boundaries, and Tab forward, not stop at the one block AC5 already covers."""
        first_url = "https://tracker.example.com/PAY-1234"
        second_url = "https://tracker.example.com/PAY-5678"
        reply_one = f"The ticket is {first_url}."
        reply_two = f"The follow-up ticket is {second_url}."

        with self.step("create the fixture repo billing-api and an agent script with two scripted replies"):
            repo = self.make_repo("billing-api", BILLING_API)
            script = self.workdir / "agent-script.json"
            script.write_text(
                json.dumps(
                    {
                        "session_ids": ["claude-session-pay-1234"],
                        "turns": [
                            {"steps": [{"chunk": reply_one}, {"end": "end_turn"}]},
                            {"steps": [{"chunk": reply_two}, {"end": "end_turn"}]},
                        ],
                    }
                )
            )
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}

        with self.step("start Cod, start a chat with the fake agent and send two prompts"):
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            self.keys("end up enter")
            self.wait_for(lambda state: chat(state).get("agent") == AGENT_ID, 30, "a chat with the fake agent")
            self.type("Where is the ticket?")
            self.keys("enter")
            self.wait_for(lambda state: reply_one in chat(state).get("transcript", ""), 30, "the first scripted reply")
            self.type("Is there a follow-up ticket?")
            self.keys("enter")
            self.wait_for(lambda state: reply_two in chat(state).get("transcript", ""), 30, "the second scripted reply")

        with self.step("move the focus into the newest reply: unchanged AC5 behaviour, the second reply's block"):
            self.keys("alt-up")
            self.wait_for(lambda state: chat(state).get("reply_focused") is True, 10, "the reply to take the focus")
            self.expect_equal("no link focused yet", None, focused_link_text(self.state()))

        with self.step("shift-tab into the newest reply's own (only) link"):
            self.keys("shift-tab")
            self.wait_for(
                lambda state: focused_link_text(state) == second_url,
                10,
                "the focus on the follow-up ticket link",
            )

        with self.step("shift-tab twice more: past the newest reply's last link, into the first reply's link"):
            self.keys("shift-tab")
            self.keys("shift-tab")
            self.wait_for(
                lambda state: focused_link_text(state) == first_url,
                10,
                "the focus to continue back into the first reply's link",
            )

        with self.step("tab forward again: back into the newest reply's link"):
            self.keys("tab")
            self.keys("tab")
            self.wait_for(
                lambda state: focused_link_text(state) == second_url,
                10,
                "tab to continue forward into the second reply's link",
            )


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
    def open_chat(self, other_sessions: list[str]) -> dict[str, Any]:
        """Opens the agent panel of the active workspace, starts a chat with the fake agent from the new thread menu and returns the first state that shows it."""
        # The agent commands join the palette once the panel has loaded, a moment after the workspace.
        self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
        self.run_command("agent: toggle focus")
        self.run_command("agent: toggle new thread menu")
        # A chat that already holds a conversation is not the new one. An empty chat of the fake agent (the fresh chat of a new workspace) is: Zed keeps an empty new chat when the user picks its agent again.
        current = chat(self.state())
        known = [*other_sessions]
        if current.get("transcript") or current.get("agent") != AGENT_ID:
            known.append(current.get("session_id"))
        # The menu ends with the external agents and "Add More Agents"; the fake agent is the only external agent.
        # The menu opens with the selected agent highlighted, which is the fake agent once a chat used it, so the keys start from the last row, not from the highlight.
        self.keys("end up enter")
        return self.wait_for(
            lambda state: chat(state).get("agent") == AGENT_ID and chat(state).get("session_id") not in [None, "", *known],
            30,
            "a new chat with the fake agent",
        )

    def start_chat(self, prompt: str, reply: str, other_sessions: list[str]) -> str:
        """Opens a chat with the fake agent (open_chat), sends the prompt, waits for the reply and returns the chat's session id."""
        self.open_chat(other_sessions)
        self.type(prompt)
        self.keys("enter")
        state = self.wait_for(lambda state: reply in chat(state).get("transcript", ""), 30, "the scripted reply in the transcript")
        return chat(state)["session_id"]

    def provider_sessions(self) -> list[str]:
        """The session ids the fake agent itself made, from its state file."""
        path = self.workdir / "agent-state.json"
        return sorted(json.loads(path.read_text())["sessions"]) if path.exists() else []

    def start_fake_agent_cod(self, persistent: bool, turns: list[dict[str, Any]] | None = None) -> Any:
        """Creates billing-api and the agent script (by default two scripted replies), starts Cod on billing-api with the fake agent and returns the repo."""
        repo = self.make_repo("billing-api", BILLING_API)
        script = self.workdir / "agent-script.json"
        if turns is None:
            turns = [
                {"steps": [{"chunk": PAY_REPLY}, {"end": "end_turn"}]},
                {"steps": [{"chunk": LEDGER_REPLY}, {"end": "end_turn"}]},
            ]
        script.write_text(json.dumps({"session_ids": [PAY_SESSION, LEDGER_SESSION], "turns": turns}))
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

    def test_ac3_the_chat_state_follows_the_agent(self) -> None:
        """The chat's own state: working, needs input, done, error. The workspace tab dot and the overview row follow with the status contract (#25, #26)."""
        permission = "Allow edit to src/invoice.rs?"
        with self.step("start Cod on billing-api with a fake agent that streams, asks for permission, ends, then fails"):
            self.start_fake_agent_cod(
                persistent=False,
                turns=[
                    {
                        "steps": [
                            {"chunk": "Reading src/invoice.rs."},
                            {"wait": "streamed"},
                            {"permission": permission},
                            {"chunk": PAY_REPLY},
                            {"end": "end_turn"},
                        ]
                    },
                    {"steps": [{"error": "the fake agent failed"}]},
                ],
            )

        with self.step("open a chat with the fake agent: it is idle"):
            self.open_chat([])
            self.wait_for_values("chat status", {"status": "idle"}, lambda state: {"status": chat(state).get("status")}, 10)

        with self.step("send a prompt: the chat is working while the reply streams"):
            self.type("Fix the rounding.")
            self.keys("enter")
            # #28 AC3: the chat's state must reach the tab dot within one second of the agent
            # streaming, not whenever a lenient poll happens to notice it.
            self.wait_for_values(
                "chat status",
                {"status": "working", "streamed": True},
                lambda state: {"status": chat(state).get("status"), "streamed": "Reading src/invoice.rs." in chat(state).get("transcript", "")},
                2,
            )

        with self.step(f"let the agent ask {permission!r}: the chat needs input"):
            (self.workdir / "streamed").touch()
            self.wait_for_values("chat status", {"status": "needs_input"}, lambda state: {"status": chat(state).get("status")}, 2)

        # Zed 1.23.2's Linux keymap binds shift-alt-a to "editor: toggle block comments" in the message editor, which wins over "agent: allow once" there, so the test allows from the command palette.
        with self.step("allow it once with 'agent: allow once' from the command palette: the turn ends and the chat is done"):
            self.run_command("agent: allow once")
            self.wait_for_values(
                "chat status",
                {"status": "done", "reply": True},
                lambda state: {"status": chat(state).get("status"), "reply": PAY_REPLY in chat(state).get("transcript", "")},
                2,
            )

        with self.step("send another prompt that the agent fails: the chat shows an error"):
            self.type("And the tax lines?")
            self.keys("enter")
            self.wait_for_values("chat status", {"status": "error"}, lambda state: {"status": chat(state).get("status")}, 2)

    def test_ac2_a_new_workspace_opens_with_a_fresh_chat_ready_for_typing(self) -> None:
        with self.step("start Cod on billing-api with the fake agent"):
            repo = self.start_fake_agent_cod(persistent=False)

        with self.step("workspace A: chat with the fake agent, which makes it the default agent"):
            a_session = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])

        with self.step(f"run {NEW_WORKSPACE!r} and add billing-api: the new workspace's agent panel holds a fresh chat of the default agent"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            state = self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1
                and state["active_workspace"] != before["active_workspace"]
                and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )
            # Zed's agent panel holds chats only for a project with a folder, and a new workspace opens without folders (#24).
            self.add_folder(repo)
            state = self.wait_for(
                lambda state: chat(state).get("agent") == AGENT_ID and chat(state).get("session_id") not in [None, "", a_session],
                30,
                "a fresh chat of the fake agent in the new workspace",
            )
            self.expect_equal("fresh chat is empty", "", chat(state).get("transcript"))
            fresh_session = chat(state)["session_id"]

        with self.step("open the agent panel with 'agent: toggle focus': the focus is in the fresh chat"):
            self.run_command("agent: toggle focus")
            self.wait_for(
                lambda state: state["agent_panel_focused"] is True and chat(state).get("session_id") == fresh_session,
                SHOW_TIMEOUT,
                "the focus in the fresh chat",
            )

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

    def test_ac1_ac4_two_chats_in_one_workspace_and_one_in_another_all_come_back_after_a_restart(self) -> None:
        """#28 AC1/AC4: every chat of each workspace comes back after a restart, not only the
        workspace's last active one: two chats in workspace A, one in workspace B."""
        with self.step("start Cod on billing-api with a fake agent that titles its chats, keeping its session"):
            repo = self.start_fake_agent_cod(
                persistent=True,
                turns=[
                    {"steps": [{"title": "fix invoice rounding"}, {"chunk": PAY_REPLY}, {"end": "end_turn"}]},
                    {"steps": [{"title": "read the ledger export"}, {"chunk": LEDGER_REPLY}, {"end": "end_turn"}]},
                    {"steps": [{"title": "plan the next release"}, {"chunk": "No blockers."}, {"end": "end_turn"}]},
                ],
            )

        with self.step("workspace A: start two chats with the fake agent"):
            a_first = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])
            a_second = self.start_chat("What does the ledger export read?", LEDGER_REPLY, [a_first])

        with self.step(f"run {NEW_WORKSPACE!r}, add billing-api there and start one chat"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1
                and state["active_workspace"] != before["active_workspace"]
                and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )
            self.add_folder(repo)
            b_session = self.start_chat("What's next?", "No blockers.", [a_first, a_second])

        with self.step("quit Cod and start it again"):
            self.restart_cod()

        with self.step("workspace B (the last active) comes back with its one chat as a tab"):
            self.wait_for(
                lambda state: [tab["session_id"] for tab in state["chat_tabs"]] == [b_session],
                RESTORE_TIMEOUT,
                "workspace B's one chat tab",
            )

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r}: both of its chats are back as tabs"):
            self.run_command(PREVIOUS_WORKSPACE)
            state = self.wait_for(
                lambda state: sorted(tab["session_id"] for tab in state["chat_tabs"]) == sorted([a_first, a_second]),
                RESTORE_TIMEOUT,
                "workspace A's two chat tabs",
            )
            self.expect_equal(
                "tab titles",
                sorted([f"{AGENT_ID}: fix invoice rounding", f"{AGENT_ID}: read the ledger export"]),
                sorted(tab["title"] for tab in state["chat_tabs"]),
            )

    def test_ac1_tabs_show_only_the_active_workspaces_chats_with_name_and_dot(self) -> None:
        """Given two chats in one workspace and one in another, the panel header's chat tabs show only the active workspace's own chats, each named and with a status dot."""
        with self.step("start Cod on billing-api with a fake agent that titles its chats"):
            repo = self.start_fake_agent_cod(
                persistent=False,
                turns=[
                    {"steps": [{"title": "fix invoice rounding"}, {"chunk": PAY_REPLY}, {"end": "end_turn"}]},
                    {"steps": [{"title": "read the ledger export"}, {"chunk": LEDGER_REPLY}, {"end": "end_turn"}]},
                    {"steps": [{"title": "fix invoice rounding"}, {"chunk": PAY_REPLY}, {"end": "end_turn"}]},
                ],
            )

        with self.step("workspace A: start two chats with the fake agent"):
            a_first = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])
            a_second = self.start_chat("What does the ledger export read?", LEDGER_REPLY, [a_first])

        with self.step("the panel header shows both of workspace A's chats as tabs, each named, the second one active"):
            # start_chat returns once the reply is in the transcript, which can be a frame before the turn's end
            # moves the chat from working (blue) to done (green), so wait for the finished turns too.
            state = self.wait_for(
                lambda state: sorted(tab["session_id"] for tab in state["chat_tabs"]) == sorted([a_first, a_second])
                and all(tab["dot"] == "green" for tab in state["chat_tabs"]),
                SHOW_TIMEOUT,
                "both of workspace A's chats as tabs, their turns done",
            )
            tabs = state["chat_tabs"]
            self.expect_equal("tab count", 2, len(tabs))
            self.expect_equal(
                "tab titles",
                sorted([f"{AGENT_ID}: fix invoice rounding", f"{AGENT_ID}: read the ledger export"]),
                sorted(tab["title"] for tab in tabs),
            )
            self.expect_equal("session ids", sorted([a_first, a_second]), sorted(tab["session_id"] for tab in tabs))
            for tab in tabs:
                self.expect_equal(f"status dot of {tab['title']!r}", "green", tab["dot"])
            active = [tab["session_id"] for tab in tabs if tab["active"]]
            self.expect_equal("only the second chat is the active tab", [a_second], active)

        with self.step(f"run {NEW_WORKSPACE!r}, add billing-api and start one chat there"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1
                and state["active_workspace"] != before["active_workspace"]
                and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )
            self.add_folder(repo)
            b_session = self.start_chat("Where does the rounding happen?", PAY_REPLY, [a_first, a_second])

        with self.step("the panel header now shows only workspace B's one chat"):
            state = self.wait_for(
                lambda state: [tab["session_id"] for tab in state["chat_tabs"]] == [b_session],
                SHOW_TIMEOUT,
                "workspace B's one chat as a tab",
            )
            self.expect_equal("active", True, state["chat_tabs"][0]["active"])

        with self.step(f"switch back to workspace A with {PREVIOUS_WORKSPACE!r}: its own two tabs are back"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for(
                lambda state: sorted(tab["session_id"] for tab in state["chat_tabs"]) == sorted([a_first, a_second]),
                SHOW_TIMEOUT,
                "workspace A's two tabs",
            )

    def test_ac3_status_reaches_the_workspace_tab_dot_and_the_overview_row(self) -> None:
        """The chat status that test_ac3_the_chat_state_follows_the_agent already checks on the chat itself also reaches workspaces[].status, the title bar tab's dot (#25) and the overview's session row (#26), as the same CodEntry."""
        permission = "Allow edit to src/invoice.rs?"
        with self.step("start Cod on billing-api with a fake agent that asks for permission, then ends"):
            self.start_fake_agent_cod(
                persistent=False,
                turns=[{"steps": [{"permission": permission}, {"chunk": PAY_REPLY}, {"end": "end_turn"}]}],
            )

        with self.step("start a chat and send a prompt: the agent asks for permission"):
            self.open_chat([])
            self.type("Fix the rounding.")
            self.keys("enter")
            # #28 AC3: within one second of the agent asking, not a lenient 10 s poll.
            # The title bar reports the dot from its last drawn frame, so wait
            # for all three observations within the same existing timeout.
            state = self.wait_for_values(
                "chat and workspace status",
                {"chat": "needs_input", "workspace": "needs_input", "dot": "yellow"},
                lambda state: {
                    "chat": chat(state).get("status"),
                    "workspace": state["workspaces"][state["active_workspace"]]["status"],
                    "dot": state["title_bar"]["tabs"][state["active_workspace"]]["dot"],
                },
                2,
            )

        with self.step("the workspace tab dot and the overview's session row both show needs_input too"):
            active = state["active_workspace"]
            self.expect_equal("workspace status", "needs_input", state["workspaces"][active]["status"])
            self.expect_equal("title bar dot", "yellow", state["title_bar"]["tabs"][active]["dot"])
            self.keys("ctrl-alt-o")
            overview_state = self.wait_for(
                lambda state: (state.get("overview") or {}).get("focused") is True, 10, "the overview to open"
            )
            session_rows = [row for row in overview_state["overview"]["rows"] if row["kind"] == "session"]
            self.expect_equal("one session row", 1, len(session_rows))
            self.expect_equal("overview row dot", "yellow", session_rows[0]["dot"])
            self.keys("escape")
            self.wait_for(lambda state: state.get("overview") is None, 10, "the overview to close")

        with self.step("allow it once: the turn ends and the workspace tab dot follows to done"):
            self.run_command("agent: allow once")
            state = self.wait_for_values(
                "chat and workspace status",
                {"chat": "done", "workspace": "done", "dot": "green"},
                lambda state: {
                    "chat": chat(state).get("status"),
                    "workspace": state["workspaces"][state["active_workspace"]]["status"],
                    "dot": state["title_bar"]["tabs"][state["active_workspace"]]["dot"],
                },
                2,
            )
            active = state["active_workspace"]
            self.expect_equal("workspace status", "done", state["workspaces"][active]["status"])
            self.expect_equal("title bar dot", "green", state["title_bar"]["tabs"][active]["dot"])

    def test_ac6_enter_on_a_session_row_switches_workspace_and_focuses_the_chat(self) -> None:
        """#26 AC6, the session part: Enter on an overview session row closes the overview, switches to that chat's workspace and focuses the chat."""
        with self.step("start Cod on billing-api with a fake agent that titles its chat"):
            repo = self.start_fake_agent_cod(
                persistent=False,
                turns=[{"steps": [{"title": "fix invoice rounding"}, {"chunk": PAY_REPLY}, {"end": "end_turn"}]}],
            )

        with self.step("workspace A: start a chat"):
            a_session = self.start_chat("Where does the rounding happen?", PAY_REPLY, [])
            a_index = self.state()["active_workspace"]

        with self.step(f"run {NEW_WORKSPACE!r} and add billing-api there, without starting a chat"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1 and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )
            self.add_folder(repo)

        with self.step("open the overview, select workspace A's session row and press enter"):
            self.keys("ctrl-alt-o")
            overview_state = self.wait_for(
                lambda state: (state.get("overview") or {}).get("focused") is True, SHOW_TIMEOUT, "the overview to open"
            )
            rows = overview_state["overview"]["rows"]
            title = f"{AGENT_ID}: fix invoice rounding"
            row_index = next(
                index for index, row in enumerate(rows) if row["kind"] == "session" and row["title"] == title
            )
            self.keys(" ".join(["down"] * row_index))
            self.wait_for(
                lambda state: (state.get("overview") or {}).get("selected_index") == row_index,
                SHOW_TIMEOUT,
                "the session row to be selected",
            )
            self.keys("enter")

        with self.step("Cod switches to workspace A and focuses the chat"):
            self.wait_for(
                lambda state: state["overview"] is None
                and state["modal"] is None
                and state["active_workspace"] == a_index
                and state["agent_panel_focused"] is True
                and chat(state).get("session_id") == a_session,
                SHOW_TIMEOUT,
                "workspace A's chat to be focused",
            )


if __name__ == "__main__":
    unittest.main()
