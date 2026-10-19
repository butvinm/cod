"""Story #32: links in files open in a page tab inside Cod; other schemes go to the system as in Zed. Keys only.

The pages are local look-alikes served by page_server.PageServer on 127.0.0.1 (a fake tracker and a fake wiki with invented content). The fixture repo is billing-api, with the links in comments of src/invoice.rs.
The system's URL handler is a fake xdg-open first on PATH: it writes every URL it gets to a file, so a test can tell what Cod handed to the system.
The driver reports page tabs under state["pages"]["open"] (see test_31_pages.py).
"""

from __future__ import annotations

import os
import stat
import time
import unittest
from collections.abc import Callable
from typing import Any
from unittest import mock

from cod_e2e import CodTestCase
from page_server import PageServer

EDITOR_OPEN_URL = "editor: open url"

WIKI = "Billing wiki - Home"
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

    def start_on_billing_api(self) -> None:
        invoice = (
            f"// Rounding rules: {self.server.url('/wiki/Home')}\n"
            f"// Old exports: {FTP_URL}\n"
            "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n"
        )
        repo = self.make_repo(
            "billing-api",
            {
                "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
                "src/invoice.rs": invoice,
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
