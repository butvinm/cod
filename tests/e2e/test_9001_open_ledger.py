"""Beta trial of #22 (invented story #9001, never merged): open the ledger file of a fresh git repo with the file finder (Ctrl-P), using only keys."""

from __future__ import annotations

import os
import unittest

from cod_e2e import CodTestCase

LEDGER_SERVICE = {
    "Cargo.toml": '[package]\nname = "ledger-service"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# ledger-service\n\nAn invented service that keeps a ledger of payments.\n",
    "src/main.rs": "mod ledger;\n\nfn main() {\n    println!(\"ledger-service\");\n}\n",
    "src/ledger.rs": (
        "pub struct Entry {\n    pub id: u32,\n    pub amount_cents: i64,\n}\n\n"
        "pub fn balance(entries: &[Entry]) -> i64 {\n    entries.iter().map(|entry| entry.amount_cents).sum()\n}\n"
    ),
}


class OpenLedgerTest(CodTestCase):
    def test_open_ledger_with_the_file_finder(self) -> None:
        with self.step("create the fixture repo ledger-service"):
            repo = self.make_repo("ledger-service", LEDGER_SERVICE)

        with self.step("start Cod on ledger-service"):
            self.start_cod(repo)

        with self.step("wait for the ledger-service worktree"):
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the ledger-service folder to open",
            )

        with self.step("check that no file is open and only ledger-service is open"):
            start = self.state()
            self.expect_equal("tabs", [], start["tabs"])
            self.expect_equal("active tab", None, start["active_tab"])
            self.expect_equal("modal", None, start["modal"])
            self.expect_equal(
                "worktrees",
                [os.path.realpath(repo)],
                [os.path.realpath(path) for path in start["worktrees"]],
            )

        with self.step("press ctrl-p"):
            self.keys("ctrl-p")
            self.wait_for(lambda state: state["modal"] == "file_finder", 10, "the file finder to open")

        with self.step("type ledger"):
            self.type("ledger")

        with self.step("press enter"):
            self.keys("enter")
            self.wait_for(
                lambda state: state["modal"] is None and state["active_tab"] is not None,
                10,
                "the file finder to close and a tab to open",
            )

        with self.step("check the active tab and the only tab"):
            end = self.state()
            self.expect_equal("active tab", "src/ledger.rs", end["active_tab"])
            self.expect_equal("tabs", ["src/ledger.rs"], end["tabs"])
            self.expect_equal("modal", None, end["modal"])


if __name__ == "__main__":
    unittest.main()
