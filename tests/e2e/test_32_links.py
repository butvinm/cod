"""Story #32: links in files open in a page tab inside Cod; other schemes go to the system as in Zed. Keys only.

The pages are local look-alikes served by page_server.PageServer on 127.0.0.1 (a fake tracker and a fake wiki with invented content). The fixture repo is billing-api, with the links in comments of src/invoice.rs.
The system's URL handler is a fake xdg-open first on PATH: it writes every URL it gets to a file, so a test can tell what Cod handed to the system.
The driver reports page tabs under state["pages"]["open"] (see test_31_pages.py).
"""

from __future__ import annotations

import json
import os
import stat
import time
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

from cod_e2e import CodTestCase
from cod_status_fixtures import center, drawn
from fake_acp_agent import fake_agent_settings
from page_server import PageServer

EDITOR_OPEN_URL = "editor: open url"
AGENT_ID = "claude-acp"

WIKI = "Billing wiki - Home"
PAY_1234 = "PAY-1234 Fix invoice rounding"
FTP_URL = "ftp://files.example.com/invoices/2024"

# How long a page may take to open and load.
LOAD_TIMEOUT = 30.0
# How long the system handler may take to get a URL.
HANDOFF_TIMEOUT = 15.0
# How long nothing must change before the test believes nothing else opened.
SETTLE_TIME = 2.0

FAKE_XDG_OPEN = """#!/usr/bin/env python3
import os
import sys

with open(os.environ["COD_E2E_OPENED_URLS"], "a") as opened:
    opened.write(" ".join(sys.argv[1:]) + "\\n")
"""


def pages(state: dict[str, Any]) -> list[dict[str, Any]]:
    return state["pages"]["open"]


def page_titled(state: dict[str, Any], title: str) -> dict[str, Any] | None:
    return next((page for page in pages(state) if page["title"] == title), None)


def chat(state: dict[str, Any]) -> dict[str, Any]:
    return state.get("chat") or {}


def link_bounds(links: list[dict[str, Any]], url: str) -> dict[str, Any] | None:
    """The drawn bounds of the link to `url` among `links` (a chat's `link_bounds` or a markdown preview's `links`), or None without such a link."""
    link = next((link for link in links if link["url"] == url), None)
    return link["bounds"] if link else None


class LinksTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.server = PageServer()
        self.addCleanup(self.server.close)
        self.opened_urls = self.workdir / "opened-urls.txt"
        bin_dir = self.workdir / "bin"
        bin_dir.mkdir()
        xdg_open = bin_dir / "xdg-open"
        xdg_open.write_text(FAKE_XDG_OPEN)
        xdg_open.chmod(xdg_open.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        environment = mock.patch.dict(
            os.environ,
            {
                "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
                "COD_E2E_OPENED_URLS": str(self.opened_urls),
            },
        )
        environment.start()
        self.addCleanup(environment.stop)

    # Steps

    def click_at(self, x: float, y: float, ctrl: bool = False) -> None:
        """Moves the mouse to the window position and clicks the left button there, holding Ctrl when `ctrl` is set (#32 AC3).

        A real click: the same `click` driver request `test_25_workspace_tabs.py` uses on title bar
        tabs, dispatched as mouse down and up at window coordinates, not a call into the handler.
        """
        self.driver.request("click", x=x, y=y, ctrl=ctrl)

    def start_on_billing_api(self) -> None:
        invoice = (
            f"// Rounding rules: {self.server.url('/wiki/Home')}\n"
            f"// Old exports: {FTP_URL}\n"
            "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n"
        )
        readme = f"# Billing API\n\nSee the [wiki]({self.server.url('/wiki/Home')}) for background.\n"
        repo = self.make_repo(
            "billing-api",
            {
                "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
                "src/invoice.rs": invoice,
                "docs/README.md": readme,
            },
        )
        self.start_cod(repo)
        self.wait_for(
            lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
            30,
            "the billing-api folder to open",
        )

    def open_invoice(self) -> None:
        self.keys("ctrl-p")
        self.wait_for(lambda state: state["file_finder_ready"], 10, "the file finder to open")
        self.type("invoice")
        self.keys("enter")
        self.wait_for(lambda state: state["active_tab"] == "invoice.rs", 10, "invoice.rs to open")

    def put_cursor_after_link(self, line: int) -> None:
        """Moves the cursor to the end of the line, right after its link."""
        self.keys("ctrl-home")
        for _ in range(line - 1):
            self.keys("down")
        self.keys("end")

    def put_cursor_within_link(self, line: int) -> None:
        """Moves the cursor one column left of the end of the line: the last character of its link, so a click at the cursor's pixel position lands inside the link's range rather than right after it."""
        self.put_cursor_after_link(line)
        self.keys("left")

    def system_urls(self) -> list[str]:
        if not self.opened_urls.exists():
            return []
        return [line for line in self.opened_urls.read_text().splitlines() if line]

    def stays(self, predicate: Callable[[dict[str, Any]], bool], seconds: float, description: str) -> dict[str, Any]:
        deadline = time.monotonic() + seconds
        while True:
            state = self.state()
            if not predicate(state):
                raise AssertionError(f"{description} did not hold; state: {state}")
            if time.monotonic() >= deadline:
                return state
            time.sleep(0.1)

    def open_wiki_link_from_invoice(self) -> dict[str, Any]:
        self.put_cursor_after_link(1)
        self.run_command(EDITOR_OPEN_URL)
        return self.wait_for(
            lambda state: state["active_tab"] == WIKI and (page_titled(state, WIKI) or {}).get("state") == "loaded",
            LOAD_TIMEOUT,
            "the wiki page to open in a tab and load",
        )

    # AC1 (keyboard path: alt-up, tab, enter)

    def test_ac1_opening_a_link_from_an_agent_reply_opens_a_page_tab(self) -> None:
        with self.step("create billing-api and an agent script whose reply links to the fake tracker page"):
            repo = self.make_repo(
                "billing-api",
                {
                    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
                    "src/main.rs": 'fn main() {\n    println!("billing-api");\n}\n',
                },
            )
            tracker_url = self.server.url("/tracker/PAY-1234")
            script = self.workdir / "agent-script.json"
            script.write_text(
                json.dumps(
                    {
                        "session_ids": ["claude-session-pay-1234"],
                        "turns": [{"steps": [{"chunk": f"The ticket is {tracker_url}."}, {"end": "end_turn"}]}],
                    }
                )
            )
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}

        with self.step("start Cod on billing-api with the fake agent"):
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )

        with self.step("start a chat with the fake agent and send a prompt"):
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            # The menu ends with the external agents and "Add More Agents"; the fake agent is the only external agent.
            self.keys("end up enter")
            self.wait_for(lambda state: chat(state).get("agent") == AGENT_ID, 30, "a chat with the fake agent")
            self.type("Where is the ticket?")
            self.keys("enter")
            self.wait_for(lambda state: tracker_url in chat(state).get("transcript", ""), 30, "the scripted reply with the link")

        with self.step("move the focus from the message box into the reply, then tab to the link"):
            self.keys("alt-up")
            self.wait_for(lambda state: chat(state).get("reply_focused") is True, 10, "the reply to take the focus")
            self.keys("tab")
            state = self.wait_for(
                lambda state: (chat(state).get("focused_link") or {}).get("url") == tracker_url,
                10,
                "the focus on the tracker link",
            )
            transcript_before = chat(state)["transcript"]

        with self.step("press enter on the link: the page opens as a tab in the editor pane"):
            self.keys("enter")
            state = self.wait_for(
                lambda state: state["active_tab"] == PAY_1234 and (page_titled(state, PAY_1234) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the tracker page to open and load",
            )
            page = page_titled(state, PAY_1234)
            assert page is not None
            self.expect_equal("page url", tracker_url, page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])

        with self.step("focus the agent panel again: the chat kept its transcript, so opening the page did not disturb it"):
            self.run_command("agent: toggle focus")
            state = self.wait_for(lambda state: state.get("agent_panel_focused") is True, 10, "the agent panel to take the focus")
            self.expect_equal("chat transcript unchanged", transcript_before, chat(state).get("transcript"))

    # AC1 (real mouse click)
    #
    # A real click: the driver dispatches mouse down and up at the window coordinates the chat
    # reports for the link (`Markdown::cod_link_bounds`, recorded every frame from the same text
    # layout the mouse hit test and the focus ring already use), the same way test_25 clicks title
    # bar tabs. No click into a handler, no keyboard shortcut invented for the test.

    def test_ac1_clicking_a_link_from_an_agent_reply_opens_a_page_tab(self) -> None:
        with self.step("create billing-api and an agent script whose reply links to the fake tracker page"):
            repo = self.make_repo(
                "billing-api",
                {
                    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
                    "src/main.rs": 'fn main() {\n    println!("billing-api");\n}\n',
                },
            )
            tracker_url = self.server.url("/tracker/PAY-1234")
            script = self.workdir / "agent-script.json"
            script.write_text(
                json.dumps(
                    {
                        "session_ids": ["claude-session-pay-1234"],
                        "turns": [{"steps": [{"chunk": f"The ticket is {tracker_url}."}, {"end": "end_turn"}]}],
                    }
                )
            )
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}

        with self.step("start Cod on billing-api with the fake agent"):
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )

        with self.step("start a chat with the fake agent and send a prompt"):
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, 30, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            # The menu ends with the external agents and "Add More Agents"; the fake agent is the only external agent.
            self.keys("end up enter")
            self.wait_for(lambda state: chat(state).get("agent") == AGENT_ID, 30, "a chat with the fake agent")
            self.type("Where is the ticket?")
            self.keys("enter")
            self.wait_for(lambda state: tracker_url in chat(state).get("transcript", ""), 30, "the scripted reply with the link")

        with self.step("find where the tracker link was drawn in the reply"):
            state = self.wait_for(
                lambda state: drawn(link_bounds(chat(state).get("link_bounds") or [], tracker_url)),
                10,
                "the tracker link to be drawn",
            )
            bounds = link_bounds(chat(state)["link_bounds"], tracker_url)
            assert bounds is not None
            transcript_before = chat(state)["transcript"]

        with self.step("click the link: the page opens as a tab in the editor pane"):
            self.click_at(*center(bounds))
            state = self.wait_for(
                lambda state: state["active_tab"] == PAY_1234 and (page_titled(state, PAY_1234) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the tracker page to open and load",
            )
            page = page_titled(state, PAY_1234)
            assert page is not None
            self.expect_equal("page url", tracker_url, page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])

        with self.step("focus the agent panel again: the chat kept its transcript, so opening the page did not disturb it"):
            self.run_command("agent: toggle focus")
            state = self.wait_for(lambda state: state.get("agent_panel_focused") is True, 10, "the agent panel to take the focus")
            self.expect_equal("chat transcript unchanged", transcript_before, chat(state).get("transcript"))

    # AC2 (markdown preview, real mouse click)

    def test_ac2_clicking_a_link_in_the_markdown_preview_opens_a_page_tab(self) -> None:
        wiki_url = self.server.url("/wiki/Home")

        with self.step("start Cod on billing-api and open docs/README.md"):
            self.start_on_billing_api()
            self.keys("ctrl-p")
            self.wait_for(lambda state: state["file_finder_ready"], 10, "the file finder to open")
            self.type("README")
            self.keys("enter")
            self.wait_for(lambda state: state["active_tab"] == "README.md", 10, "README.md to open")

        with self.step("open Zed's markdown preview and find where the wiki link was drawn"):
            self.run_command("markdown: open preview")
            state = self.wait_for(
                lambda state: drawn(link_bounds((state.get("markdown_preview") or {}).get("links") or [], wiki_url)),
                10,
                "the wiki link to be drawn in the preview",
            )
            bounds = link_bounds(state["markdown_preview"]["links"], wiki_url)
            assert bounds is not None

        with self.step("click the wiki link in the preview: the page opens as a tab"):
            self.click_at(*center(bounds))
            state = self.wait_for(
                lambda state: state["active_tab"] == WIKI and (page_titled(state, WIKI) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the wiki page to open in a tab and load",
            )
            page = page_titled(state, WIKI)
            assert page is not None
            self.expect_equal("page url", wiki_url, page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])

        with self.step("check that nothing went to the system browser"):
            self.stays(lambda state: len(pages(state)) == 1, SETTLE_TIME, "one page tab")
            self.expect_equal("urls handed to the system", [], self.system_urls())

    # AC3

    def test_ac3_editor_open_url_on_a_link_in_a_comment_opens_a_page_tab(self) -> None:
        with self.step("start Cod on billing-api and open invoice.rs"):
            self.start_on_billing_api()
            self.open_invoice()

        with self.step("put the cursor on the wiki link and run editor: open url"):
            state = self.open_wiki_link_from_invoice()

        with self.step("check the page tab next to invoice.rs"):
            self.expect_equal("tabs", ["invoice.rs", WIKI], state["tabs"])
            page = page_titled(state, WIKI)
            assert page is not None
            self.expect_equal("page url", self.server.url("/wiki/Home"), page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])

        with self.step("check that nothing went to the system browser"):
            self.stays(lambda state: len(pages(state)) == 1, SETTLE_TIME, "one page tab")
            self.expect_equal("urls handed to the system", [], self.system_urls())

    # AC3 (real Ctrl-click)
    #
    # A real Ctrl-click: the driver dispatches mouse down and up with the control modifier at the
    # window pixel position of a buffer offset in the active editor. That position is
    # `Pane::pixel_position_of_cursor`, already public (pane-split navigation uses it); the test
    # only moves the keyboard cursor onto the link first, the same navigation #32 AC3's
    # "editor: open url" test already relies on, so the click lands inside the link's range rather
    # than invoking `Editor::open_url` directly.

    def test_ac3_ctrl_click_on_a_link_in_a_comment_opens_a_page_tab(self) -> None:
        with self.step("start Cod on billing-api and open invoice.rs"):
            self.start_on_billing_api()
            self.open_invoice()

        with self.step("put the cursor on the wiki link and read its pixel position"):
            self.put_cursor_within_link(1)
            state = self.wait_for(
                lambda state: state.get("cursor_pixel_position") is not None,
                10,
                "the cursor's pixel position",
            )
            position = state["cursor_pixel_position"]

        with self.step("ctrl-click at that position: the page opens as a tab next to invoice.rs"):
            self.click_at(position["x"], position["y"], ctrl=True)
            state = self.wait_for(
                lambda state: state["active_tab"] == WIKI and (page_titled(state, WIKI) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the wiki page to open in a tab and load",
            )
            self.expect_equal("tabs", ["invoice.rs", WIKI], state["tabs"])
            page = page_titled(state, WIKI)
            assert page is not None
            self.expect_equal("page url", self.server.url("/wiki/Home"), page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])

        with self.step("check that nothing went to the system browser"):
            self.stays(lambda state: len(pages(state)) == 1, SETTLE_TIME, "one page tab")
            self.expect_equal("urls handed to the system", [], self.system_urls())

    # AC4

    def test_ac4_opening_a_link_that_is_open_focuses_its_tab(self) -> None:
        with self.step("start Cod on billing-api, open invoice.rs and its wiki link"):
            self.start_on_billing_api()
            self.open_invoice()
            first = page_titled(self.open_wiki_link_from_invoice(), WIKI)
            assert first is not None
            loads = self.server.gets("/wiki/Home")

        with self.step("go back to invoice.rs with ctrl-tab"):
            self.keys("ctrl-tab")
            self.wait_for(lambda state: state["active_tab"] == "invoice.rs", 10, "ctrl-tab to reach invoice.rs")

        with self.step("open the same link again"):
            self.put_cursor_after_link(1)
            self.run_command(EDITOR_OPEN_URL)
            state = self.wait_for(lambda state: state["active_tab"] == WIKI, LOAD_TIMEOUT, "the wiki tab to be focused")

        with self.step("check that the open tab was focused and no second one was added"):
            state = self.stays(lambda state: len(pages(state)) == 1, SETTLE_TIME, "one page tab")
            self.expect_equal("tabs", ["invoice.rs", WIKI], state["tabs"])
            self.expect_equal("page id", first["id"], pages(state)[0]["id"])
            self.expect_equal("GET count of the wiki page", loads, self.server.gets("/wiki/Home"))
            self.expect_equal("urls handed to the system", [], self.system_urls())

    # AC5

    def test_ac5_a_link_with_another_scheme_goes_to_the_system(self) -> None:
        with self.step("start Cod on billing-api and open invoice.rs"):
            self.start_on_billing_api()
            self.open_invoice()

        with self.step("put the cursor on the ftp link and run editor: open url"):
            self.put_cursor_after_link(2)
            self.run_command(EDITOR_OPEN_URL)

        with self.step("check that the system got the link and no page tab opened"):
            self.wait_for(lambda state: FTP_URL in self.system_urls(), HANDOFF_TIMEOUT, "the system to get the ftp link")
            state = self.stays(lambda state: pages(state) == [], SETTLE_TIME, "no page tab")
            self.expect_equal("tabs", ["invoice.rs"], state["tabs"])


if __name__ == "__main__":
    unittest.main()
