"""Story #22, AC2: open a file of a fresh git repo with the file finder (Ctrl-P), using only keys.

This file is the template for story tests: copy it to test_<issue>_<slug>.py and keep one file per story.
"""

from __future__ import annotations

import os
import unittest

from cod_e2e import CodTestCase

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# billing-api\n\nAn invented service that issues invoices and keeps a ledger.\n",
    "src/main.rs": "mod invoice;\nmod ledger;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": (
        "pub struct Invoice {\n    pub number: u32,\n    pub total_cents: u64,\n}\n\n"
        "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n"
    ),
    "src/ledger.rs": "pub struct Ledger {\n    pub entries: Vec<i64>,\n}\n",
}


class OpenFileTest(CodTestCase):
    def test_open_file_with_the_file_finder(self) -> None:
        with self.step("create the fixture repo billing-api"):
            repo = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(repo)

        with self.step("wait for the billing-api worktree"):
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the billing-api folder to open",
            )

        with self.step("check that no file is open"):
            start = self.state()
            self.expect_equal("tabs", [], start["tabs"])
            self.expect_equal("active tab", None, start["active_tab"])
            self.expect_equal("modal", None, start["modal"])

        with self.step("press ctrl-p"):
            self.keys("ctrl-p")
            self.wait_for(lambda state: state["modal"] == "file_finder", 10, "the file finder to open")

        with self.step("type invoice"):
            self.type("invoice")

        with self.step("press enter"):
            self.keys("enter")
            self.wait_for(
                lambda state: state["modal"] is None and state["active_tab"] is not None,
                10,
                "the file finder to close and a tab to open",
            )

        with self.step("check the active tab"):
            self.expect_equal("active tab", "invoice.rs", self.state()["active_tab"])


if __name__ == "__main__":
    unittest.main()
