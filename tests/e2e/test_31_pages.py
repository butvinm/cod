"""Story #31: open a URL as a page tab in the workspace, kept in its workspace and restored after a restart. One test per acceptance criterion, keys only.

The pages are local look-alikes served by page_server.PageServer on 127.0.0.1 (a fake tracker and a fake wiki with invented content). The fixture repo is billing-api.
The driver reports page tabs under state["pages"]: "open" (id, url, title, loading, state, kind, workspace, pane, active, hidden, browser_started, focused), "saved" (id, url) and "prompt" (query, error) of "web: open url".
"""

from __future__ import annotations

import os
import time
import unittest
from collections.abc import Callable
from typing import Any

from cod_e2e import CodTestCase
from page_server import PageServer

OPEN_URL = "web: open url"
SPLIT_RIGHT = "pane: split right"
NEW_WORKSPACE = "workspace: new workspace"
NEXT_WORKSPACE = "workspace: next workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"

PAY_1234 = "PAY-1234 Fix invoice rounding"
PAY_1235 = "PAY-1235 Round ledger totals"
WIKI = "Billing wiki - Home"

# How long a page may take to open and load.
LOAD_TIMEOUT = 30.0
# How long a tab, a split or a workspace switch may take to show.
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# How long a page must stay unchanged before the test believes no reload happened.
SETTLE_TIME = 2.0

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/invoice.rs": (
        "pub struct Invoice {\n    pub number: u32,\n    pub total_cents: u64,\n}\n\n"
        "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n"
    ),
}


def pages(state: dict[str, Any]) -> list[dict[str, Any]]:
    return state["pages"]["open"]


def page_titled(state: dict[str, Any], title: str) -> dict[str, Any] | None:
    return next((page for page in pages(state) if page["title"] == title), None)


class PagesTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.server = PageServer()
        self.addCleanup(self.server.close)

    # Steps

    def start_on_billing_api(self, persistent: bool = False) -> None:
        repo = self.make_repo("billing-api", BILLING_API)
        self.start_cod(repo, persistent=persistent)
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

    def open_url_prompt(self) -> None:
        self.run_command(OPEN_URL)
        self.wait_for(lambda state: state["pages"]["prompt"] is not None, 10, "the open url prompt to show")

    def open_page(self, text: str, title: str) -> dict[str, Any]:
        """Opens the text with "web: open url" and waits until a page tab with that title is the active tab and its page loaded."""
        self.open_url_prompt()
        self.type(text)
        self.wait_for(lambda state: state["pages"]["prompt"]["query"] == text, 10, f"the prompt to hold {text!r}")
        self.keys("enter")
        return self.wait_for(
            lambda state: state["pages"]["prompt"] is None
            and state["active_tab"] == title
            and (page_titled(state, title) or {}).get("state") == "loaded",
            LOAD_TIMEOUT,
            f"the page {title!r} to open and load",
        )

    def stays(self, predicate: Callable[[dict[str, Any]], bool], seconds: float, description: str) -> dict[str, Any]:
        deadline = time.monotonic() + seconds
        while True:
            state = self.state()
            if not predicate(state):
                raise AssertionError(f"{description} did not hold; state: {state}")
            if time.monotonic() >= deadline:
                return state
            time.sleep(0.1)

    # AC1

    def test_ac1_open_a_url_from_the_palette_next_to_a_file(self) -> None:
        with self.step("start Cod on billing-api and open invoice.rs"):
            self.start_on_billing_api()
            self.open_invoice()

        with self.step("check that no page is open"):
            start = self.state()
            self.expect_equal("tabs", ["invoice.rs"], start["tabs"])
            self.expect_equal("pages", [], pages(start))

        with self.step("open the fake tracker page with web: open url"):
            url = self.server.url("/tracker/PAY-1234")
            state = self.open_page(url, PAY_1234)

        with self.step("check the page tab next to invoice.rs"):
            self.expect_equal("tabs", ["invoice.rs", PAY_1234], state["tabs"])
            page = page_titled(state, PAY_1234)
            assert page is not None
            self.expect_equal("page url", url, page["url"])
            self.expect_equal("page kind", "external", page["kind"])
            self.expect_equal("page active", True, page["active"])
            self.expect_equal("browser started", True, page["browser_started"])
            if not str(page["id"]).startswith("page-"):
                raise AssertionError(f"expected a page id like page-N, actual {page['id']!r}")
            self.expect_equal("GET count of the tracker page", 1, self.server.gets("/tracker/PAY-1234"))

    # AC2

    def test_ac2_a_url_without_a_scheme_gets_https_and_other_text_opens_nothing(self) -> None:
        with self.step("start Cod on billing-api"):
            self.start_on_billing_api()

        with self.step("open tracker.example.com/PAY-1234"):
            self.open_url_prompt()
            self.type("tracker.example.com/PAY-1234")
            self.keys("enter")
            state = self.wait_for(
                lambda state: state["pages"]["prompt"] is None and len(pages(state)) == 1,
                SHOW_TIMEOUT,
                "a page tab to open",
            )
            self.expect_equal("page url", "https://tracker.example.com/PAY-1234", pages(state)[0]["url"])

        with self.step("type text that is not a URL"):
            self.open_url_prompt()
            self.type("fix invoice rounding")
            self.keys("enter")
            state = self.wait_for(
                lambda state: state["pages"]["prompt"] is not None and state["pages"]["prompt"]["error"] is not None,
                10,
                "an inline error in the prompt",
            )

        with self.step("check that nothing opened"):
            state = self.stays(lambda state: len(pages(state)) == 1, SETTLE_TIME, "one page tab")
            self.expect_equal("prompt text", "fix invoice rounding", state["pages"]["prompt"]["query"])
            self.keys("escape")
            self.wait_for(lambda state: state["pages"]["prompt"] is None, 10, "the prompt to close")

    # AC3

    def test_ac3_the_page_tab_behaves_like_a_file_tab(self) -> None:
        with self.step("start Cod on billing-api, open invoice.rs and the tracker page"):
            self.start_on_billing_api()
            self.open_invoice()
            self.open_page(self.server.url("/tracker/PAY-1234"), PAY_1234)

        with self.step("check that the page has the keyboard focus"):
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("focused") is True,
                10,
                "the page to have the focus",
            )

        with self.step("go back to invoice.rs and to the page with ctrl-tab"):
            self.keys("ctrl-tab")
            self.wait_for(lambda state: state["active_tab"] == "invoice.rs", 10, "ctrl-tab to reach invoice.rs")
            self.keys("ctrl-tab")
            self.wait_for(lambda state: state["active_tab"] == PAY_1234, 10, "ctrl-tab to reach the page")

        with self.step("split the page to the right"):
            self.run_command(SPLIT_RIGHT)
            state = self.wait_for(
                lambda state: len(state["panes"]) == 2
                and state["panes"][1]["active_tab"] == PAY_1234
                and len(pages(state)) == 2,
                LOAD_TIMEOUT,
                "the page split to the right",
            )
            ids = {page["id"] for page in pages(state)}
            self.expect_equal("distinct page ids", 2, len(ids))
            self.expect_equal("split urls", {self.server.url("/tracker/PAY-1234")}, {page["url"] for page in pages(state)})

        with self.step("close the split page with ctrl-w"):
            self.keys("ctrl-w")
            self.wait_for(lambda state: len(pages(state)) == 1, SHOW_TIMEOUT, "one page tab left")

    # AC4

    def test_ac4_switching_workspaces_hides_pages_without_reloading(self) -> None:
        path = "/tracker/PAY-1234"
        with self.step("start Cod on billing-api and open the tracker page"):
            self.start_on_billing_api()
            self.open_page(self.server.url(path), PAY_1234)
            loads = self.server.gets(path)

        with self.step("make workspace B and switch to it"):
            self.run_command(NEW_WORKSPACE)
            state = self.wait_for(
                lambda state: state["workspace_count"] == 2 and state["active_workspace"] == 1 and state["tabs"] == [],
                SHOW_TIMEOUT,
                "workspace B to show",
            )
            self.expect_equal("drawn tabs", [], state["drawn_tabs"])

        with self.step("check that A's page is hidden"):
            state = self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("hidden") is True,
                SHOW_TIMEOUT,
                "A's page to be hidden",
            )
            self.expect_equal("page workspace", 0, page_titled(state, PAY_1234)["workspace"])

        with self.step("switch back to A"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for(
                lambda state: state["active_workspace"] == 0
                and state["active_tab"] == PAY_1234
                and (page_titled(state, PAY_1234) or {}).get("hidden") is False,
                SHOW_TIMEOUT,
                "A's page to show again",
            )

        with self.step("check that the page did not reload"):
            self.stays(lambda state: page_titled(state, PAY_1234) is not None, SETTLE_TIME, "the page tab")
            self.expect_equal("GET count of the tracker page", loads, self.server.gets(path))

    # AC5

    def test_ac5_two_pages_in_a_split_come_back_after_a_restart(self) -> None:
        with self.step("start Cod with a saved session and open two pages in a split"):
            self.start_on_billing_api(persistent=True)
            self.open_page(self.server.url("/tracker/PAY-1234"), PAY_1234)
            self.run_command(SPLIT_RIGHT)
            self.wait_for(lambda state: len(state["panes"]) == 2, SHOW_TIMEOUT, "two panes")
            self.open_page(self.server.url("/wiki/Home"), WIKI)
            before = self.state()
            layout_before = [pane["tabs"] for pane in before["panes"]]
            ids_before = {page["title"]: page["id"] for page in pages(before)}
            urls_before = {page["id"]: page["url"] for page in pages(before)}

        with self.step("restart Cod"):
            self.restart_cod()

        with self.step("check that the pages are back in the same panes and load again"):
            state = self.wait_for(
                lambda state: [pane["tabs"] for pane in state["panes"]] == layout_before
                and len(pages(state)) == len(urls_before)
                and all(page["state"] == "loaded" for page in pages(state)),
                RESTORE_TIMEOUT,
                "the pages to come back and load",
            )
            self.expect_equal("page urls by id", urls_before, {page["id"]: page["url"] for page in pages(state)})
            self.expect_equal("page ids by title", ids_before, {page["title"]: page["id"] for page in pages(state)})

    # AC6

    def test_ac6_the_tab_follows_navigation_and_the_last_url_is_restored(self) -> None:
        with self.step("start Cod with a saved session and open the tracker page"):
            self.start_on_billing_api(persistent=True)
            self.open_page(self.server.url("/tracker/PAY-1234"), PAY_1234)
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("focused") is True,
                10,
                "the page to have the focus",
            )

        with self.step("follow the PAY-1235 link with tab and enter"):
            self.keys("tab")
            self.keys("enter")
            state = self.wait_for(
                lambda state: state["active_tab"] == PAY_1235
                and (page_titled(state, PAY_1235) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the tab to follow the PAY-1235 page",
            )
            self.expect_equal("page url", self.server.url("/tracker/PAY-1235"), pages(state)[0]["url"])
            self.wait_for(
                lambda state: self.server.url("/tracker/PAY-1235") in [page["url"] for page in state["pages"]["saved"]],
                SHOW_TIMEOUT,
                "the new url to be saved",
            )

        with self.step("restart Cod"):
            self.restart_cod()

        with self.step("check that the last url is restored"):
            state = self.wait_for(
                lambda state: state["active_tab"] == PAY_1235
                and (page_titled(state, PAY_1235) or {}).get("state") == "loaded",
                RESTORE_TIMEOUT,
                "the PAY-1235 page to come back",
            )
            self.expect_equal("page url", self.server.url("/tracker/PAY-1235"), pages(state)[0]["url"])

    # Lazy restore

    def test_pages_of_a_workspace_not_shown_start_no_browser(self) -> None:
        with self.step("start Cod with a saved session, open a page in A and an empty workspace B"):
            self.start_on_billing_api(persistent=True)
            self.open_page(self.server.url("/wiki/Home"), WIKI)
            self.run_command(NEW_WORKSPACE)
            self.wait_for(
                lambda state: state["workspace_count"] == 2 and state["active_workspace"] == 1,
                SHOW_TIMEOUT,
                "workspace B to show",
            )
            saved_id = page_titled(self.state(), WIKI)["id"]

        with self.step("restart Cod"):
            loads = self.server.gets("/wiki/Home")
            self.restart_cod()

        with self.step("check that A's page loads no page while B is shown"):
            self.wait_for(lambda state: state["workspace_count"] == 2, RESTORE_TIMEOUT, "both workspaces to come back")
            self.stays(
                lambda state: all(not page["browser_started"] for page in pages(state)),
                SETTLE_TIME,
                "no browser for a page that is not shown",
            )
            self.expect_equal("GET count of the wiki page", loads, self.server.gets("/wiki/Home"))

        with self.step("switch to A and check that its page loads with the same id"):
            self.run_command(PREVIOUS_WORKSPACE)
            state = self.wait_for(
                lambda state: state["active_workspace"] == 0
                and (page_titled(state, WIKI) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "A's page to load",
            )
            self.expect_equal("page id", saved_id, page_titled(state, WIKI)["id"])


if __name__ == "__main__":
    unittest.main()
