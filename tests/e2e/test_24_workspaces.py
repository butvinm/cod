"""Story #24: create, name and switch workspaces in one window, kept across restarts. One test per acceptance criterion, keys only.

Fixtures are the throwaway git repos billing-api, billing-web and docs-site with invented content.
The workspace commands are run from the command palette (ctrl-shift-p) by their exact names; run_command() only presses enter when the palette highlights exactly that command.
test_stock_steps_the_criteria_build_on checks the stock Zed steps the criteria use (Zed's path picker, the save prompt and its answer by keys), so it passes before the story is built.
"""

from __future__ import annotations

import os
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

from cod_e2e import CodTestCase

NEW_WORKSPACE = "workspace: new workspace"
RENAME_WORKSPACE = "workspace: rename workspace"
NEXT_WORKSPACE = "workspace: next workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"
CLOSE_WORKSPACE = "workspace: close workspace"
ADD_FOLDER = "workspace: add folder to project"
SPLIT_AND_MOVE_RIGHT = "pane: split and move right"

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding"
DOCS_NAME = "Docs: release notes"

# How long a workspace command, a file open or a restore may take to show.
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# How long a cancelled change must stay away before the test believes it.
SETTLE_TIME = 1.5


def numbered_lines(prefix: str, count: int) -> str:
    return "".join(f"{prefix} {number:02}\n" for number in range(1, count + 1))


INVOICE_RS = (
    "//! Invoices of the invented billing-api service.\n"
    "\n"
    "pub struct Invoice {\n"
    "    pub number: u32,\n"
    "    pub customer: String,\n"
    "    pub lines: Vec<Line>,\n"
    "}\n"
    "\n"
    "pub struct Line {\n"
    "    pub description: String,\n"
    "    pub quantity: u32,\n"
    "    pub unit_cents: u64,\n"
    "}\n"
    "\n"
    "impl Invoice {\n"
    "    pub fn total_cents(&self) -> u64 {\n"
    "        self.lines\n"
    "            .iter()\n"
    "            .map(|line| line.unit_cents * u64::from(line.quantity))\n"
    "            .sum()\n"
    "    }\n"
    "\n"
    "    pub fn rounded_total_cents(&self) -> u64 {\n"
    "        let total = self.total_cents();\n"
    "        (total + 50) / 100 * 100\n"
    "    }\n"
    "}\n"
    "\n"
    + numbered_lines("// invoice note", 12)
)

LEDGER_RS = (
    "//! The ledger of the invented billing-api service.\n"
    "\n"
    "pub struct Entry {\n"
    "    pub invoice: u32,\n"
    "    pub amount_cents: i64,\n"
    "}\n"
    "\n"
    "pub struct Ledger {\n"
    "    pub entries: Vec<Entry>,\n"
    "}\n"
    "\n"
    "impl Ledger {\n"
    "    pub fn new() -> Self {\n"
    "        Self { entries: Vec::new() }\n"
    "    }\n"
    "\n"
    "    pub fn record(&mut self, invoice: u32, amount_cents: i64) {\n"
    "        self.entries.push(Entry { invoice, amount_cents });\n"
    "    }\n"
    "\n"
    "    pub fn balance_cents(&self) -> i64 {\n"
    "        self.entries.iter().map(|entry| entry.amount_cents).sum()\n"
    "    }\n"
    "}\n"
    "\n"
    + numbered_lines("// ledger note", 12)
)

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# billing-api\n\nAn invented service that issues invoices and keeps a ledger.\n",
    "src/main.rs": "mod invoice;\nmod ledger;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": INVOICE_RS,
    "src/ledger.rs": LEDGER_RS,
}

BILLING_WEB = {
    "package.json": '{\n  "name": "billing-web",\n  "version": "0.1.0",\n  "private": true\n}\n',
    "README.md": "# billing-web\n\nAn invented web front end for billing-api.\n",
    "src/invoices.ts": "export function formatCents(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n",
}

DOCS_SITE = {
    "README.md": (
        "# docs-site\n"
        "\n"
        "Invented documentation for the billing services.\n"
        "\n"
        "## Release notes\n"
        "\n"
        "- Invoices round to whole units.\n"
        "- The ledger keeps every entry.\n"
    ),
    "guide/getting-started.md": "# Getting started\n\nInstall billing-api, then billing-web.\n",
}


def real(path: str | Path) -> str:
    return os.path.realpath(path)


def real_paths(paths: list[str]) -> list[str]:
    return [real(path) for path in paths]


def active(state: dict[str, Any]) -> dict[str, Any]:
    """The entry of the active workspace in state["workspaces"]."""
    index = state["active_workspace"]
    if index is None:
        raise AssertionError(f"no active workspace; workspaces: {state['workspaces']}")
    return state["workspaces"][index]


def others(state: dict[str, Any]) -> list[dict[str, Any]]:
    """The entries of every workspace but the active one."""
    return [workspace for index, workspace in enumerate(state["workspaces"]) if index != state["active_workspace"]]


def layout(state: dict[str, Any]) -> dict[str, Any]:
    """What one workspace shows: folders, panes with their tabs, split, active tab, cursor and unsaved tabs."""
    return {
        "worktrees": real_paths(state["worktrees"]),
        "panes": state["panes"],
        "split": state["split"],
        "active_tab": state["active_tab"],
        "cursor": state["cursor"],
        "dirty_tabs": state["dirty_tabs"],
    }


def workspace_list(state: dict[str, Any]) -> list[dict[str, Any]]:
    """Name and real folders of every workspace, in window order."""
    return [{"name": workspace["name"], "worktrees": real_paths(workspace["worktrees"])} for workspace in state["workspaces"]]


class WorkspacesTest(CodTestCase):
    # Actions, each a short sequence of keys.

    def open_file(self, name: str) -> None:
        """Opens the file with the file finder (ctrl-p), as in story #22."""
        self.keys("ctrl-p")
        self.wait_for(lambda state: state["modal"] == "file_finder", 10, "the file finder to open")
        self.type(name)
        self.keys("enter")
        self.wait_for(
            lambda state: state["modal"] is None and state["active_tab"] == name, SHOW_TIMEOUT, f"{name} to open in the active tab"
        )

    def go_to_line(self, line: int) -> None:
        """Moves the cursor of the active editor to the start of the line with go to line (ctrl-g)."""
        self.keys("ctrl-g")
        self.wait_for(lambda state: state["modal"] == "other", 10, "go to line to open")
        self.type(str(line))
        self.keys("enter")
        self.wait_for(
            lambda state: state["modal"] is None and state["cursor"] == {"row": line, "column": 1},
            10,
            f"the cursor to reach line {line}",
        )

    def add_folder(self, folder: Path) -> None:
        """Adds the folder to the active workspace with Zed's "add folder to project" and Zed's own path picker: select the default text, type the absolute path, enter."""
        before = len(self.state()["worktrees"])
        self.run_command(ADD_FOLDER)
        self.wait_for(lambda state: state["path_prompt"] is not None, 10, "the path picker to open")
        self.keys("ctrl-a")
        self.type(str(folder))
        self.wait_for(
            lambda state: state["path_prompt"] is not None
            and state["path_prompt"]["query"] == str(folder)
            and state["path_prompt"]["match_count"] == 1,
            10,
            f"the path picker to list exactly {folder.name}",
        )
        self.keys("enter")
        self.wait_for(
            lambda state: state["path_prompt"] is None and len(state["worktrees"]) == before + 1,
            SHOW_TIMEOUT,
            f"{folder.name} to be added",
        )

    def new_workspace(self) -> dict[str, Any]:
        before = self.state()["workspace_count"]
        self.run_command(NEW_WORKSPACE)
        return self.wait_for(lambda state: state["workspace_count"] == before + 1, SHOW_TIMEOUT, "a new workspace")

    def rename_workspace(self, name: str) -> None:
        self.run_command(RENAME_WORKSPACE)
        self.type(name)
        self.keys("enter")
        self.wait_for(lambda state: active(state)["name"] == name, SHOW_TIMEOUT, f"the workspace name {name!r}")

    def switch_workspace(self, command: str, expected_index: int) -> dict[str, Any]:
        self.run_command(command)
        return self.wait_for(
            lambda state: state["active_workspace"] == expected_index and state["modal"] is None,
            SHOW_TIMEOUT,
            f"workspace {expected_index} to become active",
        )

    # Checks

    def stays(self, predicate: Callable[[dict[str, Any]], bool], seconds: float, description: str) -> dict[str, Any]:
        """Reads the state for the given time and fails as soon as predicate(state) is false."""
        deadline = time.monotonic() + seconds
        while True:
            state = self.state()
            if not predicate(state):
                raise AssertionError(f"{description} did not hold; state: {state}")
            if time.monotonic() >= deadline:
                return state
            time.sleep(0.1)

    def expect_layout(self, what: str, expected: dict[str, Any], state: dict[str, Any]) -> None:
        actual = layout(state)
        for key in expected:
            self.expect_equal(f"{what} {key}", expected[key], actual[key])

    def wait_for_worktrees(self, folders: list[Path]) -> dict[str, Any]:
        expected = [real(folder) for folder in folders]
        return self.wait_for(
            lambda state: real_paths(state["worktrees"]) == expected, SHOW_TIMEOUT, f"the folders {[folder.name for folder in folders]}"
        )

    # Stock Zed steps that the criteria build on. This passes before story #24, so a red criterion is never a broken step.

    def test_stock_steps_the_criteria_build_on(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            billing_web = self.make_repo("billing-web", BILLING_WEB)
            source = billing_web / "src" / "invoices.ts"
            source_before = source.read_text()

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one folder, no file open, no prompt"):
            start = self.state()
            self.expect_equal("tabs", [], start["tabs"])
            self.expect_equal("prompt", None, start["prompt"])
            self.expect_equal("path picker", None, start["path_prompt"])

        with self.step("add billing-web with add folder to project and Zed's path picker"):
            self.add_folder(billing_web)

        with self.step("check that the workspace has both folders"):
            state = self.wait_for_worktrees([billing_api, billing_web])
            self.expect_equal("workspace count", 1, state["workspace_count"])
            self.expect_equal("folders of the workspace", [real(billing_api), real(billing_web)], real_paths(active(state)["worktrees"]))

        with self.step("open billing-web's invoices.ts and make an unsaved edit"):
            self.open_file("invoices.ts")
            self.type("// draft ")
            self.wait_for(lambda state: state["dirty_tabs"] == ["invoices.ts"], 10, "invoices.ts to have unsaved changes")

        with self.step("close the tab with ctrl-w and check that Zed asks to save"):
            self.keys("ctrl-w")
            self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save invoices.ts")

        with self.step("answer the prompt with enter (Save)"):
            self.keys("enter")

        with self.step("check that the edit was saved and the tab closed"):
            state = self.wait_for(lambda state: state["prompt"] is None and state["tabs"] == [], SHOW_TIMEOUT, "the tab to close")
            self.expect_equal("dirty tabs", [], state["dirty_tabs"])
            deadline = time.monotonic() + 10
            while source.read_text() != "// draft " + source_before and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("invoices.ts on disk", "// draft " + source_before, source.read_text())

    # AC1

    def test_ac1_new_workspace_opens_empty_in_the_same_window(self) -> None:
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("open invoice.rs"):
            self.open_file("invoice.rs")

        with self.step("check the starting state: one workspace with invoice.rs in one window"):
            start = self.state()
            self.expect_equal("window count", 1, start["window_count"])
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("active workspace", 0, start["active_workspace"])
            self.expect_equal("tabs", ["invoice.rs"], start["tabs"])

        with self.step(f"run {NEW_WORKSPACE!r} from the command palette"):
            state = self.new_workspace()

        with self.step("check that the new workspace is active and empty in the same window"):
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("workspace count", 2, state["workspace_count"])
            self.expect_equal("folders of the active workspace", [], active(state)["worktrees"])
            self.expect_equal("project folders", [], state["worktrees"])
            self.expect_equal("tabs", [], state["tabs"])
            self.expect_equal("active tab", None, state["active_tab"])
            self.expect_equal("panes", [{"tabs": [], "active_tab": None}], state["panes"])
            self.expect_equal("folders of the first workspace", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in others(state)])
            self.expect_equal("modal", None, state["modal"])

    # AC2

    def test_ac2_folders_added_to_the_new_workspace_stay_in_it(self) -> None:
        with self.step("create the fixture repos docs-site, billing-api and billing-web"):
            docs_site = self.make_repo("docs-site", DOCS_SITE)
            billing_api = self.make_repo("billing-api", BILLING_API)
            billing_web = self.make_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on docs-site"):
            self.start_cod(docs_site)
            self.wait_for_worktrees([docs_site])

        with self.step("check the starting state: one workspace with docs-site"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("project folders", [real(docs_site)], real_paths(start["worktrees"]))

        with self.step(f"run {NEW_WORKSPACE!r} from the command palette"):
            self.new_workspace()

        with self.step("add billing-api with add folder to project"):
            self.add_folder(billing_api)

        with self.step("add billing-web with add folder to project"):
            self.add_folder(billing_web)

        with self.step("check that the new workspace has both folders and the first one has neither"):
            state = self.wait_for_worktrees([billing_api, billing_web])
            self.expect_equal("workspace count", 2, state["workspace_count"])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("folders of the active workspace", [real(billing_api), real(billing_web)], real_paths(active(state)["worktrees"]))
            self.expect_equal("folders of the other workspace", [[real(docs_site)]], [real_paths(w["worktrees"]) for w in others(state)])

    # AC3

    def test_ac3_rename_shows_the_name_and_escape_keeps_it(self) -> None:
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: the window title does not show the name yet"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])
            title = start["window_title"]
            if title is None:
                raise AssertionError("expected a window title, actual None")
            if PAY_NAME in title:
                raise AssertionError(f"expected a window title without {PAY_NAME!r}, actual {title!r}")

        with self.step(f"run {RENAME_WORKSPACE!r}, type {PAY_NAME!r} and press enter"):
            self.rename_workspace(PAY_NAME)

        with self.step("check the name of the workspace and the window title"):
            state = self.wait_for(
                lambda state: PAY_NAME in (state["window_title"] or ""), SHOW_TIMEOUT, f"the window title to show {PAY_NAME!r}"
            )
            self.expect_equal("workspace name", PAY_NAME, active(state)["name"])
            self.expect_equal("modal", None, state["modal"])

        with self.step(f"run {RENAME_WORKSPACE!r}, type another name and press escape"):
            self.run_command(RENAME_WORKSPACE)
            self.type("Something else")
            self.keys("escape")

        with self.step("check that the old name is kept"):
            state = self.stays(
                lambda state: active(state)["name"] == PAY_NAME
                and PAY_NAME in (state["window_title"] or "")
                and "Something else" not in (state["window_title"] or ""),
                SETTLE_TIME,
                f"the name {PAY_NAME!r} in the workspace and the window title",
            )
            self.expect_equal("workspace name", PAY_NAME, active(state)["name"])
            self.expect_equal("modal", None, state["modal"])

    # AC4

    def test_ac4_switching_keeps_each_workspace_as_it_was(self) -> None:
        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)
            invoice_on_disk = (billing_api / "src" / "invoice.rs").read_text()

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one workspace, no file open"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("tabs", [], start["tabs"])

        with self.step("workspace A: open invoice.rs and make an unsaved edit at line 5"):
            self.open_file("invoice.rs")
            self.go_to_line(5)
            self.type("// rounding check ")
            self.wait_for(lambda state: state["dirty_tabs"] == ["invoice.rs"], 10, "invoice.rs to have unsaved changes")

        with self.step("workspace A: open ledger.rs and move it into a split on the right"):
            self.open_file("ledger.rs")
            self.run_command(SPLIT_AND_MOVE_RIGHT)
            self.wait_for(lambda state: state["split"] == "vertical" and len(state["panes"]) == 2, 10, "a vertical split")

        with self.step("workspace A: put the cursor of ledger.rs at line 18"):
            self.go_to_line(18)

        a_expected = {
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}, {"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "split": "vertical",
            "active_tab": "ledger.rs",
            "cursor": {"row": 18, "column": 1},
            "dirty_tabs": ["invoice.rs"],
        }
        with self.step("check workspace A"):
            state = self.state()
            self.expect_layout("workspace A", a_expected, state)
            a_index = state["active_workspace"]

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site and open README.md"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")

        b_expected = {
            "worktrees": [real(docs_site)],
            "panes": [{"tabs": ["README.md"], "active_tab": "README.md"}],
            "split": None,
            "active_tab": "README.md",
            "cursor": {"row": 1, "column": 1},
            "dirty_tabs": [],
        }
        with self.step("check workspace B"):
            state = self.state()
            self.expect_layout("workspace B", b_expected, state)
            b_index = state["active_workspace"]
            self.expect_equal("workspace count", 2, state["workspace_count"])
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")

        with self.step(f"switch from B to A with {PREVIOUS_WORKSPACE!r}"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index)

        with self.step("check that workspace A is as it was, with the unsaved edit"):
            self.expect_layout("workspace A", a_expected, state)

        with self.step(f"switch from A to B with {NEXT_WORKSPACE!r}"):
            state = self.switch_workspace(NEXT_WORKSPACE, b_index)

        with self.step("check that workspace B is as it was"):
            self.expect_layout("workspace B", b_expected, state)

        with self.step(f"switch from B back to A with {PREVIOUS_WORKSPACE!r}"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index)

        with self.step("check that workspace A is still as it was and nothing was reloaded from disk"):
            self.expect_layout("workspace A", a_expected, state)
            self.expect_equal("invoice.rs on disk", invoice_on_disk, (billing_api / "src" / "invoice.rs").read_text())
            self.expect_equal("window count", 1, state["window_count"])

    # AC5

    def test_ac5_workspaces_come_back_after_quit_and_start(self) -> None:
        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one workspace, no file open"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("tabs", [], start["tabs"])

        # Zed already restores one workspace; this proves the quit and the restart before anything of story #24 is needed.
        with self.step("restart smoke check: open invoice.rs, quit with ctrl-q, start again"):
            self.open_file("invoice.rs")
            self.restart_cod()

        with self.step("check that billing-api and invoice.rs came back"):
            state = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(billing_api)] and state["active_tab"] == "invoice.rs",
                RESTORE_TIMEOUT,
                "billing-api with invoice.rs to be restored",
            )
            self.expect_equal("tabs", ["invoice.rs"], state["tabs"])

        with self.step("workspace A: open ledger.rs in a split on the right with the cursor at line 18"):
            self.open_file("ledger.rs")
            self.run_command(SPLIT_AND_MOVE_RIGHT)
            self.wait_for(lambda state: state["split"] == "vertical" and len(state["panes"]) == 2, 10, "a vertical split")
            self.go_to_line(18)

        with self.step(f"workspace A: rename it to {PAY_NAME!r}"):
            self.rename_workspace(PAY_NAME)

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site, open README.md at line 3"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")
            self.go_to_line(3)

        with self.step(f"workspace B: rename it to {DOCS_NAME!r}"):
            self.rename_workspace(DOCS_NAME)

        with self.step(f"make workspace A active with {PREVIOUS_WORKSPACE!r}"):
            b_index = self.state()["active_workspace"]
            self.run_command(PREVIOUS_WORKSPACE)
            state = self.wait_for(lambda state: active(state)["name"] == PAY_NAME, SHOW_TIMEOUT, "workspace A to become active")
            a_index = state["active_workspace"]

        a_expected = {
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}, {"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "split": "vertical",
            "active_tab": "ledger.rs",
            "cursor": {"row": 18, "column": 1},
        }
        b_expected = {
            "worktrees": [real(docs_site)],
            "panes": [{"tabs": ["README.md"], "active_tab": "README.md"}],
            "split": None,
            "active_tab": "README.md",
            "cursor": {"row": 3, "column": 1},
        }
        with self.step("check both workspaces before the restart"):
            self.expect_layout("workspace A", a_expected, state)
            before = workspace_list(state)
            self.expect_equal(
                "workspaces",
                sorted([{"name": PAY_NAME, "worktrees": [real(billing_api)]}, {"name": DOCS_NAME, "worktrees": [real(docs_site)]}], key=str),
                sorted(before, key=str),
            )

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that both workspaces came back with names and folders, and A is active"):
            state = self.wait_for(lambda state: state["workspace_count"] == 2, RESTORE_TIMEOUT, "two workspaces to be restored")
            state = self.wait_for(
                lambda state: state["active_workspace"] == a_index and layout(state)["panes"] == a_expected["panes"],
                RESTORE_TIMEOUT,
                "workspace A with its tabs to be active",
            )
            self.expect_equal("workspaces", before, workspace_list(state))
            self.expect_equal("window count", 1, state["window_count"])

        with self.step("check workspace A: tabs, split and cursor"):
            self.expect_layout("workspace A", a_expected, self.state())

        with self.step(f"switch to workspace B with {NEXT_WORKSPACE!r}"):
            state = self.switch_workspace(NEXT_WORKSPACE, b_index)

        with self.step("check workspace B: tab and cursor"):
            self.expect_layout("workspace B", b_expected, state)
            self.expect_equal("workspace name", DOCS_NAME, active(state)["name"])

    # AC6

    def test_ac6_close_workspace_asks_to_save_and_it_stays_closed(self) -> None:
        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)
            readme = docs_site / "README.md"
            readme_before = readme.read_text()

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one workspace"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site, open README.md"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")

        with self.step("workspace B: make an unsaved edit in README.md"):
            self.type("Draft: ")
            self.wait_for(lambda state: state["dirty_tabs"] == ["README.md"], 10, "README.md to have unsaved changes")
            self.expect_equal("README.md on disk", readme_before, readme.read_text())

        with self.step(f"run {CLOSE_WORKSPACE!r}"):
            self.run_command(CLOSE_WORKSPACE)

        with self.step("check that Cod asks before closing"):
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save README.md")
            self.expect_equal("workspace count while asking", 2, state["workspace_count"])

        # Zed's save prompt highlights its first answer, Save (as when closing a window with an unsaved file).
        with self.step("answer the prompt with enter (Save)"):
            self.keys("enter")

        with self.step("check that the edit was saved, the workspace is gone and the other one is active"):
            state = self.wait_for(
                lambda state: state["prompt"] is None and state["workspace_count"] == 1, SHOW_TIMEOUT, "the workspace to close"
            )
            self.expect_equal("folders of the active workspace", [real(billing_api)], real_paths(active(state)["worktrees"]))
            self.expect_equal("project folders", [real(billing_api)], real_paths(state["worktrees"]))
            deadline = time.monotonic() + 10
            while readme.read_text() != "Draft: " + readme_before and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("README.md on disk", "Draft: " + readme_before, readme.read_text())

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that the closed workspace did not come back"):
            state = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(billing_api)], RESTORE_TIMEOUT, "billing-api to be restored"
            )
            state = self.stays(lambda state: state["workspace_count"] == 1, SETTLE_TIME, "exactly one workspace")
            self.expect_equal("workspaces", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in state["workspaces"]])

        with self.step(f"close the last workspace with {CLOSE_WORKSPACE!r}"):
            self.run_command(CLOSE_WORKSPACE)

        with self.step("check that one empty workspace is left in the window"):
            state = self.wait_for(
                lambda state: state["workspace_count"] == 1 and state["worktrees"] == [], SHOW_TIMEOUT, "one empty workspace"
            )
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("tabs", [], state["tabs"])
            self.expect_equal("prompt", None, state["prompt"])


if __name__ == "__main__":
    unittest.main()
