"""Story #38, first part: codctl pane, open and diff commands (no pages yet). One test per acceptance criterion that is built.

Each test starts Cod on the throwaway git repo ~/work/billing-api (invented content) and runs the shipped codctl binary as a separate process, from the repo folder, the way an agent does through Bash. It checks what codctl prints and its exit code, and the window through the driver state (panes, tabs, the split, the cursor).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import time
import unittest
from pathlib import Path
from typing import Any

from cod_e2e import CodTestCase, make_repo

COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
SETTLE_TIME = 1.5

INVOICE = "".join(f"// line {number}: invented invoice rule {number}\n" for number in range(1, 30))
INVOICE_OLD = INVOICE.replace("rule 18", "old rule 18")
BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": INVOICE,
    "src/invoice_old.rs": INVOICE_OLD,
}
# Zed's FileDiffView names its tab "<old> <left right arrow> <new>".
DIFF_TAB = "invoice.rs " + chr(0x2194) + " invoice_old.rs"


class LayoutTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.home = self.workdir / "home"
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        self.binary = Path(run_dir) / "codctl"

    # Helpers

    def codctl(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        for name in ("COD_WORKSPACE", "COD_SOCKET"):
            env.pop(name, None)
        env["HOME"] = str(self.home)
        return subprocess.run(
            [str(self.binary), *args],
            env=env,
            cwd=str(cwd) if cwd else None,
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT,
            stdin=subprocess.DEVNULL,
        )

    def expect_result(
        self, result: subprocess.CompletedProcess[str], code: int, stdout: str | None = None, stderr: str | None = None
    ) -> None:
        details = f"stdout {result.stdout!r}, stderr {result.stderr!r}"
        self.expect_equal(f"exit code of {result.args[1:]} ({details})", code, result.returncode)
        if stdout is not None:
            self.expect_equal(f"stdout of {result.args[1:]}", stdout, result.stdout)
        if stderr is not None:
            self.expect_equal(f"stderr of {result.args[1:]}", stderr, result.stderr)

    def expect_one_line_error(self, result: subprocess.CompletedProcess[str]) -> None:
        self.expect_result(result, 1, stdout="")
        self.expect_equal(f"number of stderr lines of {result.args[1:]} ({result.stderr!r})", 1, len(result.stderr.splitlines()))

    def panes_json(self) -> list[dict[str, Any]]:
        result = self.codctl("pane", "list", "--json")
        self.expect_result(result, 0)
        panes = json.loads(result.stdout)
        if not isinstance(panes, list):
            raise AssertionError(f"pane list --json printed {result.stdout!r}, not a JSON array")
        return panes

    def start_on_billing_api(self) -> Path:
        repo = make_repo(self.home / "work", "billing-api", BILLING_API)
        self.start_cod(repo)
        self.wait_for(
            lambda state: [os.path.realpath(path) for path in state["worktrees"]] == [os.path.realpath(repo)],
            SHOW_TIMEOUT,
            "the billing-api folder to open",
        )
        deadline = time.monotonic() + COMMAND_TIMEOUT
        while True:
            last = self.codctl("pane", "list", "--json")
            if last.returncode == 0:
                break
            if time.monotonic() > deadline:
                raise AssertionError(f"codctl did not reach Cod: exit {last.returncode}, stderr {last.stderr!r}")
            time.sleep(0.2)
        return repo

    def pane_view(self, state: dict[str, Any]) -> dict[str, Any]:
        return {"tabs": [pane["tabs"] for pane in state["panes"]], "split": state["split"]}

    # AC1

    def test_ac1_pane_list_split_focus_close(self) -> None:
        with self.step("start Cod on billing-api and open src/invoice.rs with codctl"):
            repo = self.start_on_billing_api()
            self.expect_result(self.codctl("open", "src/invoice.rs", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("panes", {"tabs": [["invoice.rs"]], "split": None}, self.pane_view, SHOW_TIMEOUT)

        with self.step("check that pane list prints the one active pane with its tab"):
            panes = self.panes_json()
            self.expect_equal("listed panes", [{"active": True, "tabs": ["invoice.rs"]}], [{"active": pane["active"], "tabs": pane["tabs"]} for pane in panes])
            first = panes[0]["id"]
            text = self.codctl("pane", "list")
            self.expect_result(text, 0, stdout=f"{first}\tactive\tinvoice.rs\n", stderr="")

        with self.step(f"run codctl pane split {first} --right and check the new empty pane on the right"):
            result = self.codctl("pane", "split", str(first), "--right")
            self.expect_result(result, 0, stderr="")
            if not re.fullmatch(r"\d+\n", result.stdout):
                raise AssertionError(f"pane split printed {result.stdout!r}, not one pane ID")
            second = int(result.stdout)
            self.wait_for_values("panes", {"tabs": [["invoice.rs"], []], "split": "vertical"}, self.pane_view, SHOW_TIMEOUT)
            self.expect_equal("listed pane IDs", [first, second], [pane["id"] for pane in self.panes_json()])

        with self.step(f"run codctl pane focus {first} and then {second}, and check the active pane"):
            self.expect_result(self.codctl("pane", "focus", str(first)), 0, stdout="", stderr="")
            self.wait_for(lambda state: [pane["active"] for pane in self.panes_json()] == [True, False], SHOW_TIMEOUT, f"pane {first} to be active")
            self.expect_result(self.codctl("pane", "focus", str(second)), 0, stdout="", stderr="")
            self.wait_for(lambda state: [pane["active"] for pane in self.panes_json()] == [False, True], SHOW_TIMEOUT, f"pane {second} to be active")

        with self.step(f"run codctl pane close {second} and check that one pane is left"):
            self.expect_result(self.codctl("pane", "close", str(second)), 0, stdout="", stderr="")
            self.wait_for_values("panes", {"tabs": [["invoice.rs"]], "split": None}, self.pane_view, SHOW_TIMEOUT)
            self.expect_equal("listed pane IDs", [first], [pane["id"] for pane in self.panes_json()])

        with self.step("check that an unknown pane, the only pane and a bad direction fail with one line and exit code 1"):
            for args in (
                ["pane", "focus", "999999999"],
                ["pane", "close", str(first)],
                ["pane", "split", str(first)],
                ["pane", "split", "first", "--down"],
            ):
                self.expect_one_line_error(self.codctl(*args))
            self.expect_window_unchanged({"tabs": [["invoice.rs"]], "split": None})

    def expect_window_unchanged(self, expected: dict[str, Any]) -> None:
        time.sleep(SETTLE_TIME)
        self.expect_equal("panes", expected, self.pane_view(self.state()))

    # AC2

    def test_ac2_open_file_at_line_and_column(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("run codctl open src/invoice.rs:18:9 and check the tab and the cursor"):
            self.expect_result(self.codctl("open", "src/invoice.rs:18:9", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("editor", {"active_tab": "invoice.rs", "cursor": {"row": 18, "column": 9}}, lambda state: {"active_tab": state["active_tab"], "cursor": state["cursor"]}, SHOW_TIMEOUT)

        with self.step("run codctl open src/invoice.rs:4 and check that the cursor moves to line 4, column 1"):
            self.expect_result(self.codctl("open", "src/invoice.rs:4", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("cursor", {"cursor": {"row": 4, "column": 1}}, lambda state: {"cursor": state["cursor"]}, SHOW_TIMEOUT)

        with self.step("split the pane and run codctl open src/main.rs --pane <new pane> with an absolute path"):
            first = self.panes_json()[0]["id"]
            split = self.codctl("pane", "split", str(first), "--down")
            self.expect_result(split, 0, stderr="")
            second = int(split.stdout)
            self.expect_result(self.codctl("pane", "focus", str(first)), 0)
            self.expect_result(self.codctl("open", str(repo / "src" / "main.rs"), "--pane", str(second)), 0, stdout="", stderr="")
            self.wait_for_values("panes", {"tabs": [["invoice.rs"], ["main.rs"]], "split": "horizontal"}, self.pane_view, SHOW_TIMEOUT)

        with self.step("check that a missing file fails with one line and exit code 1 and opens nothing"):
            self.expect_one_line_error(self.codctl("open", "src/missing.rs:3", cwd=repo))
            self.expect_window_unchanged({"tabs": [["invoice.rs"], ["main.rs"]], "split": "horizontal"})

    # AC6

    def test_ac6_diff_of_two_files(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("run codctl diff src/invoice.rs src/invoice_old.rs and check the diff tab"):
            self.expect_result(self.codctl("diff", "src/invoice.rs", "src/invoice_old.rs", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("active tab", {"active_tab": DIFF_TAB}, lambda state: {"active_tab": state["active_tab"]}, SHOW_TIMEOUT)

        with self.step("check that a diff with a missing file fails with one line and exit code 1"):
            self.expect_one_line_error(self.codctl("diff", "src/invoice.rs", "src/missing.rs", cwd=repo))


if __name__ == "__main__":
    unittest.main()
