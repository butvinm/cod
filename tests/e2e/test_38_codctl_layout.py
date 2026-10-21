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

from cod_e2e import CodTestCase, git_environment, make_repo
from page_server import PageServer

COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
LOAD_TIMEOUT = 30.0
SETTLE_TIME = 1.5

INVOICE = "".join(f"// line {number}: invented invoice rule {number}\n" for number in range(1, 30))
INVOICE_OLD = INVOICE.replace("rule 18", "old rule 18")
INVOICE_ROUNDED = INVOICE.replace("rule 18", "rounded rule 18")
BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": INVOICE,
    "src/invoice_old.rs": INVOICE_OLD,
}
# Zed's FileDiffView names its tab "<old> <left right arrow> <new>".
DIFF_TAB = "invoice.rs " + chr(0x2194) + " invoice_old.rs"
# Zed's CommitView names its tab "<short sha> <em dash> <subject>".
PAY_1234 = "PAY-1234 Fix invoice rounding"
PAY_1235 = "PAY-1235 Round ledger totals"


def commit_change(repo: Path, relative: str, content: str, message: str) -> str:
    """Writes `content` to `relative` in the repo and commits it, returning the new commit's sha."""
    (repo / relative).write_text(content)
    env = git_environment(os.environ)

    def git(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True, text=True)

    git("add", "--all")
    git("-c", "commit.gpgsign=false", "commit", "--quiet", "--no-verify", "-m", message)
    return git("rev-parse", "HEAD").stdout.strip()


def pages(state: dict[str, Any]) -> list[dict[str, Any]]:
    return state["pages"]["open"]


def page_titled(state: dict[str, Any], title: str) -> dict[str, Any] | None:
    return next((page for page in pages(state) if page["title"] == title), None)


class LayoutTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.home = self.workdir / "home"
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        self.binary = Path(run_dir) / "codctl"
        self.server = PageServer()
        self.addCleanup(self.server.close)

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

        with self.step(f"run codctl pane resize {first} --size 40% and check the split's widths"):
            result = self.codctl("pane", "resize", str(first), "--size", "40%")
            self.expect_result(result, 0, stdout="", stderr="")

            def share_of_first(_: dict[str, Any]) -> float:
                widths = [pane["bounds"]["width"] for pane in self.panes_json()]
                return widths[0] / sum(widths)

            self.wait_for(lambda state: abs(share_of_first(state) - 0.4) < 0.03, SHOW_TIMEOUT, f"pane {first} to take 40% of the split")

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

    def test_a5_resize_picks_the_actual_containing_split_not_an_unrelated_pane(self) -> None:
        """RC review A5: split A right to make B (full height), then split A down (A keeps the
        top-left quarter, C gets the bottom-left quarter). Resizing B, a full-height pane, must
        use the outer, horizontal split it is actually in, not get confused by A's half-height
        bounds into picking the inner, vertical split and silently doing nothing."""
        with self.step("start Cod on billing-api, open a file, and split it right then down"):
            repo = self.start_on_billing_api()
            self.expect_result(self.codctl("open", "src/invoice.rs", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("panes", {"tabs": [["invoice.rs"]], "split": None}, self.pane_view, SHOW_TIMEOUT)
            first = self.panes_json()[0]["id"]

            split_right = self.codctl("pane", "split", str(first), "--right")
            self.expect_result(split_right, 0, stderr="")
            b_id = int(split_right.stdout)
            self.wait_for_values("panes", {"tabs": [["invoice.rs"], []], "split": "vertical"}, self.pane_view, SHOW_TIMEOUT)

            split_down = self.codctl("pane", "split", str(first), "--down")
            self.expect_result(split_down, 0, stderr="")
            c_id = int(split_down.stdout)
            self.wait_for(lambda state: len(self.panes_json()) == 3, SHOW_TIMEOUT, "three panes after the second split")

        with self.step("B is full height before any resize, A and C each about half of it"):
            # One snapshot for everything below: splitting A down may replace A's own pane id
            # with two fresh ones (top and bottom), c_id is one of them by the split command's
            # own result, and the third id, whatever it now is, is A's other half, found here by
            # elimination instead of assumed to still be `first`. Two separate driver calls could
            # race a layout still settling right after the split; one snapshot cannot.
            before = {pane["id"]: pane["bounds"] for pane in self.panes_json()}
            self.expect_equal("three panes after both splits", 3, len(before))
            a_id = next(pane_id for pane_id in before if pane_id not in (b_id, c_id))
            full_height = before[b_id]["height"]
            self.expect_equal("A is about half height", True, abs(before[a_id]["height"] - full_height / 2) < 5)
            self.expect_equal("C is about half height", True, abs(before[c_id]["height"] - full_height / 2) < 5)

        with self.step(f"run codctl pane resize {b_id} --size 70% and check B's width actually changes"):
            result = self.codctl("pane", "resize", str(b_id), "--size", "70%")
            self.expect_result(result, 0, stdout="", stderr="")

            def share_of_b(_: dict[str, Any]) -> float:
                widths = {pane["id"]: pane["bounds"]["width"] for pane in self.panes_json()}
                # A and C share the left column and so share its width; the split's total width
                # is A's (or C's) width plus B's, not the sum of every pane's width.
                total = widths[a_id] + widths[b_id]
                return widths[b_id] / total

            self.wait_for(
                lambda state: abs(share_of_b(state) - 0.7) < 0.05,
                SHOW_TIMEOUT,
                f"pane {b_id} to take 70% of the split's width",
            )

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

    # AC3

    def test_ac3_open_a_url_as_a_page(self) -> None:
        with self.step("start Cod on billing-api and open invoice.rs"):
            repo = self.start_on_billing_api()
            self.expect_result(self.codctl("open", "src/invoice.rs", cwd=repo), 0, stdout="", stderr="")
            self.wait_for_values("panes", {"tabs": [["invoice.rs"]], "split": None}, self.pane_view, SHOW_TIMEOUT)

        with self.step("run codctl open <tracker url> and check the printed page id and the page tab"):
            url = self.server.url("/tracker/PAY-1234")
            result = self.codctl("open", url, cwd=repo)
            self.expect_result(result, 0, stderr="")
            if not re.fullmatch(r"page-\d+\n", result.stdout):
                raise AssertionError(f"open {url!r} printed {result.stdout!r}, not a page id")
            page_id = result.stdout.strip()
            state = self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("state") == "loaded"
                and state["active_tab"] == PAY_1234,
                LOAD_TIMEOUT,
                "the page to open and load",
            )
            page = page_titled(state, PAY_1234)
            assert page is not None
            self.expect_equal("page id", page_id, page["id"])
            self.expect_equal("page url", url, page["url"])
            self.expect_equal("tabs", ["invoice.rs", PAY_1234], state["tabs"])

    # AC4

    def test_ac4_page_get_reports_state_and_updates_after_navigation(self) -> None:
        with self.step("start Cod on billing-api and open the tracker page"):
            repo = self.start_on_billing_api()
            url = self.server.url("/tracker/PAY-1234")
            page_id = self.codctl("open", url, cwd=repo).stdout.strip()
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the page to load",
            )

        with self.step("run codctl page get and check the url, title and state"):
            result = self.codctl("page", "get", page_id)
            self.expect_result(result, 0, stdout=f"{url}\t{PAY_1234}\tloaded\n", stderr="")
            json_result = self.codctl("page", "get", page_id, "--json")
            self.expect_result(json_result, 0, stderr="")
            self.expect_equal(
                "page get --json",
                {"id": page_id, "url": url, "title": PAY_1234, "state": "loaded"},
                json.loads(json_result.stdout),
            )

        with self.step("follow the PAY-1235 link and check that page get shows the new url and title"):
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("focused") is True,
                10,
                "the page to have the focus",
            )
            self.keys("tab")
            self.keys("enter")
            new_url = self.server.url("/tracker/PAY-1235")
            self.wait_for(
                lambda state: (page_titled(state, PAY_1235) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the page to follow the link",
            )
            after = self.codctl("page", "get", page_id)
            self.expect_result(after, 0, stdout=f"{new_url}\t{PAY_1235}\tloaded\n", stderr="")

        with self.step("an unknown page id fails with one line and exit code 1"):
            self.expect_one_line_error(self.codctl("page", "get", "page-999999"))

    # AC5

    def test_ac5_page_reload_back_forward_close(self) -> None:
        with self.step("start Cod on billing-api and open the tracker page"):
            repo = self.start_on_billing_api()
            url = self.server.url("/tracker/PAY-1234")
            page_id = self.codctl("open", url, cwd=repo).stdout.strip()
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the page to load",
            )

        with self.step("follow the PAY-1235 link, then run codctl page back and codctl page forward"):
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("focused") is True,
                10,
                "the page to have the focus",
            )
            self.keys("tab")
            self.keys("enter")
            self.wait_for(
                lambda state: (page_titled(state, PAY_1235) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the page to follow the link",
            )
            self.expect_result(self.codctl("page", "back", page_id), 0, stdout="", stderr="")
            self.wait_for(
                lambda state: (page_titled(state, PAY_1234) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "codctl page back to return",
            )
            self.expect_result(self.codctl("page", "forward", page_id), 0, stdout="", stderr="")
            self.wait_for(
                lambda state: (page_titled(state, PAY_1235) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "codctl page forward to follow again",
            )

        with self.step("run codctl page reload and check the page reloads"):
            loads = self.server.gets("/tracker/PAY-1235")
            self.expect_result(self.codctl("page", "reload", page_id), 0, stdout="", stderr="")
            self.wait_for(
                lambda state: self.server.gets("/tracker/PAY-1235") > loads,
                LOAD_TIMEOUT,
                "codctl page reload to reload the page",
            )

        with self.step("run codctl page close and check the tab is gone"):
            self.expect_result(self.codctl("page", "close", page_id), 0, stdout="", stderr="")
            self.wait_for(lambda state: page_titled(state, PAY_1235) is None, SHOW_TIMEOUT, "the page tab to close")

        with self.step("an unknown page id fails with one line and exit code 1 for reload, back, forward and close"):
            for action in ("reload", "back", "forward", "close"):
                self.expect_one_line_error(self.codctl("page", action, "page-999999"))

    # AC6 rest

    def test_ac6_diff_of_two_revisions(self) -> None:
        with self.step("start Cod on billing-api and add a second commit"):
            repo = self.start_on_billing_api()
            sha = commit_change(repo, "src/invoice.rs", INVOICE_ROUNDED, "Round invoice totals")

        with self.step("run codctl diff HEAD~1 HEAD and check the commit diff tab"):
            self.expect_result(self.codctl("diff", "HEAD~1", "HEAD", cwd=repo), 0, stdout="", stderr="")
            expected_tab = f"{sha[:7]} {chr(0x2014)} Round invoice totals"
            self.wait_for_values(
                "active tab", {"active_tab": expected_tab}, lambda state: {"active_tab": state["active_tab"]}, SHOW_TIMEOUT
            )

        with self.step("check that an unknown revision fails with one line and exit code 1"):
            self.expect_one_line_error(self.codctl("diff", "HEAD~2", "HEAD", cwd=repo))


if __name__ == "__main__":
    unittest.main()
