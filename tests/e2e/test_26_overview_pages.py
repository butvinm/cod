"""Story #26 (the page part of AC2 and AC6) and #31: an open page tab publishes as a CodEntry row of kind "page" in its workspace's status store, so the overview lists it, and Enter on that row switches to the page's workspace and focuses its tab.

The page is a real page tab (#31), opened with "web: open url" against the local look-alike fake tracker (page_server.PageServer), not the set_entry test fixture test_26_overview.py uses for sessions (that fixture stands in for the agent provider of #28, which does not apply to pages).
"""

from __future__ import annotations

import os
import unittest
from typing import Any

from cod_e2e import CodTestCase
from page_server import PageServer

NEW_WORKSPACE = "workspace: new workspace"
OPEN_URL = "web: open url"

# The overview shortcut on Linux (#26 AC1).
OVERVIEW = "ctrl-alt-o"

SHOW_TIMEOUT = 15.0
LOAD_TIMEOUT = 30.0

PAY_1234 = "PAY-1234 Fix invoice rounding"

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/invoice.rs": "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n",
}


def overview(state: dict[str, Any]) -> dict[str, Any]:
    found = state.get("overview")
    if not isinstance(found, dict):
        raise AssertionError(f"expected the overview to be open, actual {found!r}")
    return found


def is_open(state: dict[str, Any]) -> bool:
    return isinstance(state.get("overview"), dict)


def pages(state: dict[str, Any]) -> list[dict[str, Any]]:
    return state["pages"]["open"]


def page_row(state: dict[str, Any], title: str) -> dict[str, Any]:
    row = next((row for row in overview(state)["rows"] if row["kind"] == "page" and row["title"] == title), None)
    assert row is not None, f"no page row {title!r} in {overview(state)['rows']}"
    return row


class OverviewPagesTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.server = PageServer()
        self.addCleanup(self.server.close)

    # Steps

    def start_on_billing_api(self) -> int:
        """Starts Cod on billing-api and returns the index of its workspace."""
        repo = self.make_repo("billing-api", BILLING_API)
        self.start_cod(repo)
        self.wait_for(
            lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
            30,
            "the billing-api folder to open",
        )
        return self.state()["active_workspace"]

    def open_url_prompt(self) -> None:
        self.run_command(OPEN_URL)
        self.wait_for(lambda state: state["pages"]["prompt"] is not None, SHOW_TIMEOUT, "the open url prompt to show")

    def open_page(self, text: str, title: str) -> dict[str, Any]:
        self.open_url_prompt()
        self.type(text)
        self.wait_for(lambda state: state["pages"]["prompt"]["query"] == text, SHOW_TIMEOUT, f"the prompt to hold {text!r}")
        self.keys("enter")
        return self.wait_for(
            lambda state: state["pages"]["prompt"] is None
            and state["active_tab"] == title
            and (next((page for page in pages(state) if page["title"] == title), None) or {}).get("state") == "loaded",
            LOAD_TIMEOUT,
            f"the page {title!r} to open and load",
        )

    def open_overview(self) -> dict[str, Any]:
        self.keys(OVERVIEW)
        return self.wait_for(
            lambda state: is_open(state) and overview(state)["focused"] and len(overview(state)["rows"]) > 0,
            SHOW_TIMEOUT,
            "the overview to open with the focus",
        )

    # AC2 (page part): a page lists as a row with its host and path, no status dot while idle.

    def test_ac2_an_open_page_lists_as_a_row_with_its_host_and_path(self) -> None:
        with self.step("start Cod on billing-api and open the fake tracker page"):
            self.start_on_billing_api()
            self.open_page(self.server.url("/tracker/PAY-1234"), PAY_1234)

        with self.step("open the overview: the page row shows its title, no dot (idle) and its host and path"):
            state = self.open_overview()
            row = page_row(state, PAY_1234)
            self.expect_equal("kind", "page", row["kind"])
            self.expect_equal("dot", None, row["dot"])
            if "/tracker/PAY-1234" not in row["detail"]:
                raise AssertionError(f"expected the page row's context to hold its path, actual {row['detail']!r}")

    # AC6 (page part): Enter on a page row switches to its workspace and focuses its tab.

    def test_ac6_enter_on_a_page_row_switches_workspace_and_focuses_its_tab(self) -> None:
        with self.step("start Cod on billing-api (workspace A) and open the fake tracker page there"):
            a_index = self.start_on_billing_api()
            self.open_page(self.server.url("/tracker/PAY-1234"), PAY_1234)

        with self.step("open a second workspace B: A is no longer active"):
            before = self.state()
            self.run_command(NEW_WORKSPACE)
            self.wait_for(
                lambda state: state["workspace_count"] == before["workspace_count"] + 1
                and state["active_workspace"] != a_index
                and state["modal"] is None,
                SHOW_TIMEOUT,
                "the new workspace",
            )

        with self.step("open the overview, select the page row and press enter"):
            state = self.open_overview()
            rows = overview(state)["rows"]
            index = next(i for i, row in enumerate(rows) if row["kind"] == "page" and row["title"] == PAY_1234)
            for _ in range(index):
                self.keys("down")
            self.wait_for(
                lambda state: is_open(state) and overview(state)["selected_index"] == index,
                SHOW_TIMEOUT,
                "the page row to be selected",
            )
            self.keys("enter")

        with self.step("check that workspace A is active again and the page tab holds the keyboard focus"):
            state = self.wait_for(
                lambda state: state["overview"] is None
                and state["modal"] is None
                and state["active_workspace"] == a_index
                and state["active_tab"] == PAY_1234,
                SHOW_TIMEOUT,
                "workspace A active with the page tab shown",
            )
            page = next(page for page in pages(state) if page["title"] == PAY_1234)
            self.expect_equal("page tab focused", True, page["focused"])


if __name__ == "__main__":
    unittest.main()
