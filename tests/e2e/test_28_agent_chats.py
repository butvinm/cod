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


if __name__ == "__main__":
    unittest.main()
