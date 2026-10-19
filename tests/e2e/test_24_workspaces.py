"""Story #24: create, name and switch workspaces in one window, kept across restarts. One test per acceptance criterion, keys only.

Fixtures are the throwaway git repos billing-api, billing-web and docs-site with invented content.
The workspace commands are run from the command palette (ctrl-shift-p) by their exact names; run_command() only presses enter when the palette highlights exactly that command for the typed query.
Every check of what a workspace shows waits for the complete expected view (folders, panes, split, tabs, cursor, unsaved text, drawn tabs, project panel) and names each value that differs.
The test_stock_* tests check the stock Zed steps the criteria use (Zed's path picker, the project panel, the save prompts and their answers by keys), so they pass before the story is built.
test_stock_zed_asks_to_save_when_a_window_closes records the prompt stock Zed shows when a window with an unsaved file closes; AC6 expects the same prompt.
"""

from __future__ import annotations

import contextlib
import os
import subprocess
import sys
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

import loopback_ssh
from cod_e2e import Cod, CodTestCase, cod_command, cod_environment

NEW_WORKSPACE = "workspace: new workspace"
RENAME_WORKSPACE = "workspace: rename workspace"
NEXT_WORKSPACE = "workspace: next workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"
CLOSE_WORKSPACE = "workspace: close workspace"
ADD_FOLDER = "workspace: add folder to project"
SPLIT_AND_MOVE_RIGHT = "pane: split and move right"
TOGGLE_PROJECT_PANEL_FOCUS = "project panel: toggle focus"
CLOSE_ALL_DOCKS = "workspace: close all docks"
NEW_WINDOW = "workspace: new window"
NEW_FILE = "workspace: new file"
CLOSE_WINDOW = "workspace: close window"
MOVE_PROJECT_TO_NEW_WINDOW = "multi workspace: move project to new window"

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding"
DOCS_NAME = "Docs: release notes"

# How long a workspace command, a file open or a restore may take to show.
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# How long an SSH connection, the remote server start and a remote restore may take on the loopback sshd.
REMOTE_TIMEOUT = 120.0
# How long the slow remote server wrapper sleeps on every run, so a cancel or a window close lands while the remote client starts.
REMOTE_SERVER_DELAY = 20.0
# How long a cancelled change must stay away before the test believes it.
SETTLE_TIME = 1.5
# Seconds after the launch at which the crash-safety test kills Cod: right after the launch
# (while the new session id is published), before Zed starts, while it reads the session, while
# the window's workspaces come back, and after they are back.
KILL_DELAYS = (0.05, 0.1, 0.3, 0.5, 0.7, 1.0, 1.5, 2.0, 3.0)

# The answers of Zed's prompt for an unsaved file (Pane::save_item in crates/workspace/src/pane.rs).
SAVE_ANSWERS = ["Save", "Don't Save", "Cancel"]


def save_prompt(path: str | None) -> dict[str, Any]:
    """The prompt Zed shows when it closes one unsaved file, as the driver reports it: the message for the file's path in its folder (dirty_message_for in crates/workspace/src/pane.rs), no detail, Save, Don't Save and Cancel, Save highlighted.

    path is None for a file that has no path yet. test_stock_zed_asks_to_save_when_a_window_closes and test_stock_steps_the_criteria_build_on check both forms in stock Zed.
    """
    if path is None:
        message = "This buffer contains unsaved edits. Do you want to save it?"
    else:
        message = f"`{path}` contains unsaved edits. Do you want to save it?"
    return {"message": message, "detail": None, "answers": SAVE_ANSWERS, "highlighted": 0}


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

# The remote fixture, opened only over the loopback SSH service.
LEDGER_REMOTE = {
    "README.md": "# ledger-remote\n\nAn invented ledger kept on a remote host.\n",
    "ledger.rs": "pub fn balance(entries: &[i64]) -> i64 {\n    entries.iter().sum()\n}\n",
}

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


def insert_at_line(text: str, line: int, inserted: str) -> str:
    """The text with inserted put at the start of the 1-based line."""
    lines = text.splitlines(keepends=True)
    lines[line - 1] = inserted + lines[line - 1]
    return "".join(lines)


def active(state: dict[str, Any]) -> dict[str, Any]:
    """The entry of the active workspace in state["workspaces"]."""
    index = state["active_workspace"]
    if index is None:
        raise AssertionError(f"no active workspace; workspaces: {state['workspaces']}")
    return state["workspaces"][index]


def others(state: dict[str, Any]) -> list[dict[str, Any]]:
    """The entries of every workspace but the active one."""
    return [workspace for index, workspace in enumerate(state["workspaces"]) if index != state["active_workspace"]]


def panel_summary(panel: dict[str, Any] | None) -> dict[str, Any] | None:
    """What the project panel shows, without its rows and its focus: open, roots, expanded folders, selected row."""
    if panel is None:
        return None
    return {key: panel[key] for key in ("open", "roots", "expanded", "selected")}


def layout(state: dict[str, Any]) -> dict[str, Any]:
    """What the window shows of the active workspace: index and name, folders, panes with their tabs, split, active tab, cursor, unsaved tabs and their text, the tabs of the drawn panes, the project panel (summary and rows), modal, prompt and rename field."""
    index = state["active_workspace"]
    panel = state["project_panel"]
    return {
        "active_workspace": index,
        "workspace_count": state["workspace_count"],
        "window_count": state["window_count"],
        "name": state["workspaces"][index]["name"] if index is not None else None,
        "worktrees": real_paths(state["worktrees"]),
        "panes": state["panes"],
        "split": state["split"],
        "active_tab": state["active_tab"],
        "tabs": state["tabs"],
        "cursor": state["cursor"],
        "dirty_tabs": state["dirty_tabs"],
        "unsaved_texts": state["unsaved_texts"],
        "drawn_tabs": state["drawn_tabs"],
        "panel": panel_summary(panel),
        "panel_rows": panel["rows"] if panel is not None else None,
        "modal": state["modal"],
        "prompt": state["prompt"],
        "rename_input": state["rename_input"],
    }


def workspace_list(state: dict[str, Any]) -> list[dict[str, Any]]:
    """Name and real folders of every workspace, in window order."""
    return [{"name": workspace["name"], "worktrees": real_paths(workspace["worktrees"])} for workspace in state["workspaces"]]


def drawn_list(state: dict[str, Any]) -> list[bool]:
    """For every workspace, in window order, whether the last drawn frame holds any of its panes."""
    return [workspace["drawn"] for workspace in state["workspaces"]]


class WorkspacesTest(CodTestCase):
    # Actions, each a short sequence of keys.

    def open_file(self, name: str) -> None:
        """Opens the file with the file finder (ctrl-p), as in story #22."""
        self.keys("ctrl-p")
        self.wait_for(lambda state: state["modal"] == "file_finder", 10, "the file finder to open")
        # Keys sent before the finder is drawn reach no element of it and are lost (beta run 39128519357).
        self.wait_for(lambda state: state["file_finder_ready"], 10, "the file finder to be drawn with the focus")
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
        before = real_paths(self.state()["worktrees"])
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
        self.wait_for_values(
            f"{folder.name} added", {"worktrees": [*before, real(folder)], "modal": None}, layout, SHOW_TIMEOUT
        )
        self.wait_for(lambda state: state["path_prompt"] is None, SHOW_TIMEOUT, "the path picker to close")

    def show_project_panel(self) -> dict[str, Any]:
        """Opens the project panel of the active workspace with "project panel: toggle focus" (it opens the panel and focuses it), then gives the focus back to the editor area with the same command. The panel stays open."""
        self.run_command(TOGGLE_PROJECT_PANEL_FOCUS)
        self.wait_for(
            lambda state: state["project_panel"] is not None and state["project_panel"]["open"] and state["project_panel"]["focused"],
            SHOW_TIMEOUT,
            "the project panel to open with the focus",
        )
        self.run_command(TOGGLE_PROJECT_PANEL_FOCUS)
        return self.wait_for(
            lambda state: state["project_panel"]["open"] and not state["project_panel"]["focused"] and state["modal"] is None,
            SHOW_TIMEOUT,
            "the project panel to stay open without the focus",
        )

    def new_workspace(self) -> dict[str, Any]:
        """Runs "workspace: new workspace" and waits until the window shows the new workspace: empty, active, one empty pane, no tab drawn."""
        before = self.state()
        self.run_command(NEW_WORKSPACE)
        state = self.wait_for_values(
            "the new workspace",
            {
                "workspace_count": before["workspace_count"] + 1,
                "window_count": before["window_count"],
                "name": None,
                "worktrees": [],
                "panes": [{"tabs": [], "active_tab": None}],
                "tabs": [],
                "active_tab": None,
                "drawn_tabs": [],
                "modal": None,
            },
            layout,
            SHOW_TIMEOUT,
        )
        if state["active_workspace"] == before["active_workspace"]:
            raise AssertionError(f"expected another active workspace than {before['active_workspace']}, actual {state['active_workspace']}")
        return state

    def wait_for_rename_field(self, description: str) -> dict[str, Any]:
        """Waits until the rename field of "workspace: rename workspace" is shown (state["rename_input"]) and returns that state."""
        state = self.wait_for(lambda state: state["rename_input"] is not None, SHOW_TIMEOUT, description)
        text = state["rename_input"].get("text")
        if not isinstance(text, str):
            raise AssertionError(f"expected the rename field to have a text, actual {state['rename_input']!r}")
        return state

    def rename_workspace(self, name: str) -> None:
        """Runs "workspace: rename workspace", checks the proposed text (the current name, when there is one), types the name, checks the field, presses enter and waits for the name."""
        current = active(self.state())["name"]
        self.run_command(RENAME_WORKSPACE)
        state = self.wait_for_rename_field("the rename field to show")
        if current is not None:
            self.expect_equal("proposed text of the rename field", current, state["rename_input"]["text"])
        self.type(name)
        self.wait_for_values("the rename field", {"text": name}, lambda state: state["rename_input"] or {}, SHOW_TIMEOUT)
        self.keys("enter")
        self.wait_for_values(
            f"the workspace named {name!r}", {"name": name, "rename_input": None, "modal": None}, layout, SHOW_TIMEOUT
        )

    def switch_workspace(self, command: str, expected_index: int, what: str, expected: dict[str, Any]) -> dict[str, Any]:
        """Runs the command and waits until workspace expected_index is active and the window shows everything in expected (keys of layout())."""
        self.run_command(command)
        return self.wait_for_values(what, {"active_workspace": expected_index, "modal": None, **expected}, layout, SHOW_TIMEOUT)

    def wait_for_title(self, name: str, timeout: float = SHOW_TIMEOUT) -> dict[str, Any]:
        """Waits until the window title shows the name."""
        return self.wait_for(lambda state: name in (state["window_title"] or ""), timeout, f"the window title to show {name!r}")

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

    # Stock Zed steps that the criteria build on. These pass before story #24, so a red criterion is never a broken step.

    def test_stock_steps_the_criteria_build_on(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            billing_web = self.make_repo("billing-web", BILLING_WEB)
            source = billing_web / "src" / "invoices.ts"
            source_before = source.read_text()

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one folder, no file open, no prompt, the project panel open on billing-api"):
            start = self.wait_for_values(
                "the starting project panel",
                {"open": True, "roots": ["billing-api"], "expanded": []},
                lambda state: panel_summary(state["project_panel"]) or {},
                SHOW_TIMEOUT,
            )
            self.expect_equal("tabs", [], start["tabs"])
            self.expect_equal("prompt", None, start["prompt"])
            self.expect_equal("path picker", None, start["path_prompt"])
            self.expect_equal("rename field", None, start["rename_input"])
            self.wait_for_values("the drawn workspaces", {"drawn": [True]}, lambda state: {"drawn": drawn_list(state)}, SHOW_TIMEOUT)

        with self.step("add billing-web with add folder to project and Zed's path picker"):
            self.add_folder(billing_web)

        with self.step("check that the workspace has both folders and the project panel lists both roots"):
            state = self.wait_for_worktrees([billing_api, billing_web])
            self.expect_equal("workspace count", 1, state["workspace_count"])
            self.expect_equal("folders of the workspace", [real(billing_api), real(billing_web)], real_paths(active(state)["worktrees"]))
            self.wait_for_values(
                "the project panel", {"open": True, "roots": ["billing-api", "billing-web"]}, lambda state: panel_summary(state["project_panel"]) or {}, SHOW_TIMEOUT
            )

        with self.step("open billing-web's invoices.ts and make an unsaved edit"):
            self.open_file("invoices.ts")
            self.type("// draft ")
            self.wait_for(lambda state: state["dirty_tabs"] == ["invoices.ts"], 10, "invoices.ts to have unsaved changes")

        with self.step("check the unsaved text, the drawn tab and the project panel revealing invoices.ts"):
            self.wait_for_values(
                "billing-api with the edited invoices.ts",
                {
                    "unsaved_texts": [{"tab": "invoices.ts", "text": "// draft " + source_before}],
                    "drawn_tabs": ["invoices.ts"],
                    "panel": {
                        "open": True,
                        "roots": ["billing-api", "billing-web"],
                        "expanded": ["billing-web/src"],
                        "selected": "billing-web/src/invoices.ts",
                    },
                },
                layout,
                SHOW_TIMEOUT,
            )

        with self.step("close the tab with ctrl-w and check that Zed asks to save, with its message and answers"):
            self.keys("ctrl-w")
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save invoices.ts")
            self.expect_equal("prompt", save_prompt("src/invoices.ts"), state["prompt"])

        with self.step("answer the prompt with enter (Save)"):
            self.keys("enter")

        with self.step("check that the edit was saved and the tab closed"):
            state = self.wait_for(lambda state: state["prompt"] is None and state["tabs"] == [], SHOW_TIMEOUT, "the tab to close")
            self.expect_equal("dirty tabs", [], state["dirty_tabs"])
            self.expect_equal("unsaved texts", [], state["unsaved_texts"])
            deadline = time.monotonic() + 10
            while source.read_text() != "// draft " + source_before and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("invoices.ts on disk", "// draft " + source_before, source.read_text())

        with self.step(f"close the project panel with {CLOSE_ALL_DOCKS!r}"):
            self.run_command(CLOSE_ALL_DOCKS)
            self.wait_for_values(
                "the closed project panel", {"open": False, "roots": ["billing-api", "billing-web"]}, lambda state: panel_summary(state["project_panel"]) or {}, SHOW_TIMEOUT
            )

        with self.step(f"open it again with {TOGGLE_PROJECT_PANEL_FOCUS!r} twice: open with the focus, then open without it"):
            state = self.show_project_panel()
            self.expect_equal("project panel roots", ["billing-api", "billing-web"], state["project_panel"]["roots"])

    def test_stock_zed_asks_to_save_when_a_window_closes(self) -> None:
        """The reference for AC6: what stock Zed asks when a window with an unsaved file closes.

        Stock Zed does not ask when the window shows a folder: it keeps the unsaved text for the next start (hot exit, Workspace::prepare_to_close in crates/workspace/src/workspace.rs).
        It asks when the window has no folder and another window stays open, so this test closes such a second window.
        """
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: one window, no prompt"):
            start = self.state()
            self.expect_equal("window count", 1, start["window_count"])
            self.expect_equal("sway windows", 1, len(start["sway_windows"]))
            self.expect_equal("prompt", None, start["prompt"])

        with self.step(f"open a second window with {NEW_WINDOW!r}"):
            self.run_command(NEW_WINDOW)
            state = self.wait_for_values(
                "the second window", {"window_count": 2, "worktrees": [], "tabs": [], "modal": None}, layout, SHOW_TIMEOUT
            )
            self.wait_for(lambda state: len(state["sway_windows"]) == 2, SHOW_TIMEOUT, "sway to show two Cod windows")

        with self.step(f"in the second window: {NEW_FILE!r}, then type an unsaved line"):
            self.run_command(NEW_FILE)
            self.wait_for(lambda state: len(state["tabs"]) == 1 and state["modal"] is None, SHOW_TIMEOUT, "a new file tab")
            self.type("Draft note")
            state = self.wait_for(
                lambda state: len(state["dirty_tabs"]) == 1 and [entry["text"] for entry in state["unsaved_texts"]] == ["Draft note"],
                SHOW_TIMEOUT,
                "the new file to hold the unsaved line",
            )

        with self.step(f"close the second window with {CLOSE_WINDOW!r} and check the prompt"):
            self.run_command(CLOSE_WINDOW)
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save the new file")
            self.expect_equal("prompt when a window closes", save_prompt(None), state["prompt"])
            self.expect_equal("window count while asking", 2, state["window_count"])

        with self.step("answer the prompt with escape (Cancel)"):
            self.keys("escape")

        with self.step("check that the window stays open with its unsaved line"):
            state = self.wait_for(lambda state: state["prompt"] is None, SHOW_TIMEOUT, "the prompt to close")
            self.expect_equal("window count", 2, state["window_count"])
            self.expect_equal("unsaved texts", ["Draft note"], [entry["text"] for entry in state["unsaved_texts"]])

    # AC1

    def test_ac1_new_workspace_opens_empty_in_the_same_window(self) -> None:
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("open invoice.rs"):
            self.open_file("invoice.rs")

        with self.step("check the starting state: one workspace with invoice.rs drawn in one window"):
            start = self.wait_for_values(
                "the first workspace", {"drawn_tabs": ["invoice.rs"], "tabs": ["invoice.rs"]}, layout, SHOW_TIMEOUT
            )
            self.expect_equal("window count", 1, start["window_count"])
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("active workspace", 0, start["active_workspace"])
            self.expect_equal("tabs", ["invoice.rs"], start["tabs"])
            self.expect_equal("drawn workspaces", [True], drawn_list(start))
            self.expect_equal("sway windows", 1, len(start["sway_windows"]))
            window_id = start["sway_windows"][0]

        with self.step(f"run {NEW_WORKSPACE!r} from the command palette"):
            state = self.new_workspace()

        with self.step("check that the new workspace is active and empty in the same window, and the first one's tabs are not drawn"):
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("sway windows", [window_id], state["sway_windows"])
            self.expect_equal("workspace count", 2, state["workspace_count"])
            self.expect_equal("folders of the active workspace", [], active(state)["worktrees"])
            self.expect_equal("project folders", [], state["worktrees"])
            self.expect_equal("tabs", [], state["tabs"])
            self.expect_equal("active tab", None, state["active_tab"])
            self.expect_equal("panes", [{"tabs": [], "active_tab": None}], state["panes"])
            self.expect_equal("drawn tabs", [], state["drawn_tabs"])
            self.expect_equal("active workspace drawn", True, active(state)["drawn"])
            self.expect_equal("other workspaces drawn", [False], [workspace["drawn"] for workspace in others(state)])
            self.expect_equal("folders of the first workspace", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in others(state)])
            self.expect_equal("modal", None, state["modal"])
            if state["project_panel"] is None:
                raise AssertionError("expected the new workspace to have a project panel, actual None")
            self.expect_equal("project panel roots", [], state["project_panel"]["roots"])
            self.expect_equal("project panel rows", [], state["project_panel"]["rows"])

        with self.step(f"open the project panel of the new workspace with {TOGGLE_PROJECT_PANEL_FOCUS!r} and check that it lists no folder"):
            state = self.show_project_panel()
            self.expect_equal("project panel roots", [], state["project_panel"]["roots"])
            self.expect_equal("project panel rows", [], state["project_panel"]["rows"])
            self.expect_equal("drawn tabs", [], state["drawn_tabs"])
            self.expect_equal("sway windows", [window_id], state["sway_windows"])

    # AC2

    def test_ac2_folders_added_to_the_new_workspace_stay_in_it(self) -> None:
        with self.step("create the fixture repos docs-site, billing-api and billing-web"):
            docs_site = self.make_repo("docs-site", DOCS_SITE)
            billing_api = self.make_repo("billing-api", BILLING_API)
            billing_web = self.make_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on docs-site"):
            self.start_cod(docs_site)
            self.wait_for_worktrees([docs_site])

        with self.step("check the starting state: one workspace with docs-site in its project panel"):
            start = self.wait_for_values(
                "the first workspace's project panel",
                {"open": True, "roots": ["docs-site"], "expanded": []},
                lambda state: panel_summary(state["project_panel"]) or {},
                SHOW_TIMEOUT,
            )
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("project folders", [real(docs_site)], real_paths(start["worktrees"]))
            first_index = start["active_workspace"]
            # The whole panel as it is now, so the switches below must bring back exactly this panel.
            first_panel = {"panel": panel_summary(start["project_panel"]), "panel_rows": start["project_panel"]["rows"]}

        with self.step(f"run {NEW_WORKSPACE!r} from the command palette"):
            new_index = self.new_workspace()["active_workspace"]

        with self.step("add billing-api with add folder to project"):
            self.add_folder(billing_api)

        with self.step("add billing-web with add folder to project"):
            self.add_folder(billing_web)

        with self.step(f"show the project panel with {TOGGLE_PROJECT_PANEL_FOCUS!r}"):
            self.show_project_panel()

        with self.step("check that the new workspace's project panel lists both folders as roots and the first workspace has neither"):
            self.wait_for_values(
                "the new workspace's project panel",
                {"open": True, "roots": ["billing-api", "billing-web"]},
                lambda state: panel_summary(state["project_panel"]) or {},
                SHOW_TIMEOUT,
            )
            state = self.wait_for_values("the new workspace", {"worktrees": [real(billing_api), real(billing_web)]}, layout, SHOW_TIMEOUT)
            self.expect_equal("workspace count", 2, state["workspace_count"])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("folders of the active workspace", [real(billing_api), real(billing_web)], real_paths(active(state)["worktrees"]))
            self.expect_equal("folders of the other workspace", [[real(docs_site)]], [real_paths(w["worktrees"]) for w in others(state)])
            self.expect_equal("project panel roots", ["billing-api", "billing-web"], state["project_panel"]["roots"])
            new_panel = {"panel": panel_summary(state["project_panel"]), "panel_rows": state["project_panel"]["rows"]}

        with self.step(f"switch to the first workspace with {PREVIOUS_WORKSPACE!r} and check that its project panel lists only docs-site"):
            state = self.switch_workspace(
                PREVIOUS_WORKSPACE, first_index, "the first workspace", {"worktrees": [real(docs_site)], "drawn_tabs": [], **first_panel}
            )
            self.expect_equal("project panel roots", ["docs-site"], state["project_panel"]["roots"])
            self.expect_equal("project panel open", True, state["project_panel"]["open"])
            self.expect_equal("folders of the other workspace", [[real(billing_api), real(billing_web)]], [real_paths(w["worktrees"]) for w in others(state)])

        with self.step(f"switch back with {NEXT_WORKSPACE!r} and check that the project panel lists billing-api and billing-web again"):
            state = self.switch_workspace(
                NEXT_WORKSPACE, new_index, "the new workspace", {"worktrees": [real(billing_api), real(billing_web)], "drawn_tabs": [], **new_panel}
            )
            self.expect_equal("project panel open", True, state["project_panel"]["open"])
            self.expect_equal("project panel roots", ["billing-api", "billing-web"], state["project_panel"]["roots"])
            self.expect_equal("folders of the other workspace", [[real(docs_site)]], [real_paths(w["worktrees"]) for w in others(state)])
            self.expect_equal("window count", 1, state["window_count"])

    # AC3

    def test_ac3_rename_shows_the_name_and_escape_keeps_it(self) -> None:
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            self.wait_for_worktrees([billing_api])

        with self.step("check the starting state: the window title does not show the name yet, no rename field"):
            start = self.state()
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("rename field", None, start["rename_input"])
            title = start["window_title"]
            if title is None:
                raise AssertionError("expected a window title, actual None")
            if PAY_NAME in title:
                raise AssertionError(f"expected a window title without {PAY_NAME!r}, actual {title!r}")

        with self.step(f"run {RENAME_WORKSPACE!r}, type {PAY_NAME!r} and press enter"):
            self.rename_workspace(PAY_NAME)

        with self.step("check the name of the workspace and the window title"):
            state = self.wait_for_title(PAY_NAME)
            self.expect_equal("workspace name", PAY_NAME, active(state)["name"])
            self.expect_equal("modal", None, state["modal"])
            self.expect_equal("rename field", None, state["rename_input"])

        with self.step(f"run {RENAME_WORKSPACE!r}: the rename field proposes the current name"):
            self.run_command(RENAME_WORKSPACE)
            state = self.wait_for_rename_field("the rename field to show")
            self.expect_equal("proposed text of the rename field", PAY_NAME, state["rename_input"]["text"])

        with self.step("type another name and check that the field holds it"):
            self.type("Something else")
            self.wait_for_values("the rename field", {"text": "Something else"}, lambda state: state["rename_input"] or {}, SHOW_TIMEOUT)

        with self.step("press escape"):
            self.keys("escape")

        with self.step("check that the field closed and the old name is kept"):
            self.wait_for_values("the cancelled rename", {"rename_input": None, "modal": None, "name": PAY_NAME}, layout, SHOW_TIMEOUT)
            state = self.stays(
                lambda state: active(state)["name"] == PAY_NAME
                and PAY_NAME in (state["window_title"] or "")
                and "Something else" not in (state["window_title"] or ""),
                SETTLE_TIME,
                f"the name {PAY_NAME!r} in the workspace and the window title",
            )
            self.expect_equal("workspace name", PAY_NAME, active(state)["name"])
            self.expect_equal("modal", None, state["modal"])
            self.expect_equal("rename field", None, state["rename_input"])

    # AC4

    def test_ac4_switching_keeps_each_workspace_as_it_was(self) -> None:
        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)
            invoice_on_disk = (billing_api / "src" / "invoice.rs").read_text()
            invoice_edited = insert_at_line(invoice_on_disk, 5, "// rounding check ")

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

        # Workspace A's project panel is open, with src expanded and ledger.rs selected (Zed reveals the active file).
        a_expected = {
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}, {"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "split": "vertical",
            "active_tab": "ledger.rs",
            "cursor": {"row": 18, "column": 1},
            "dirty_tabs": ["invoice.rs"],
            "unsaved_texts": [{"tab": "invoice.rs", "text": invoice_edited}],
            "drawn_tabs": ["invoice.rs", "ledger.rs"],
            "panel": {"open": True, "roots": ["billing-api"], "expanded": ["billing-api/src"], "selected": "billing-api/src/ledger.rs"},
        }
        with self.step("check workspace A, with its project panel"):
            state = self.wait_for_values("workspace A", a_expected, layout, SHOW_TIMEOUT)
            self.expect_layout("workspace A", a_expected, state)
            a_index = state["active_workspace"]
            # The complete list of rows, so the switches below must bring back exactly this panel.
            a_expected["panel_rows"] = state["project_panel"]["rows"]

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site and open README.md"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")

        with self.step(f"workspace B: close its project panel with {CLOSE_ALL_DOCKS!r}"):
            self.run_command(CLOSE_ALL_DOCKS)

        # Workspace B's project panel is closed, nothing expanded below docs-site, README.md selected.
        b_expected = {
            "worktrees": [real(docs_site)],
            "panes": [{"tabs": ["README.md"], "active_tab": "README.md"}],
            "split": None,
            "active_tab": "README.md",
            "cursor": {"row": 1, "column": 1},
            "dirty_tabs": [],
            "unsaved_texts": [],
            "drawn_tabs": ["README.md"],
            "panel": {"open": False, "roots": ["docs-site"], "expanded": [], "selected": "docs-site/README.md"},
        }
        with self.step("check workspace B, with its project panel"):
            state = self.wait_for_values("workspace B", b_expected, layout, SHOW_TIMEOUT)
            self.expect_layout("workspace B", b_expected, state)
            b_index = state["active_workspace"]
            b_expected["panel_rows"] = state["project_panel"]["rows"]
            self.expect_equal("workspace count", 2, state["workspace_count"])
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")

        with self.step(f"switch from B to A with {PREVIOUS_WORKSPACE!r}"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)

        with self.step("check that workspace A is as it was, with the unsaved edit"):
            self.expect_layout("workspace A", a_expected, state)

        with self.step(f"switch from A to B with {NEXT_WORKSPACE!r}"):
            state = self.switch_workspace(NEXT_WORKSPACE, b_index, "workspace B", b_expected)

        with self.step("check that workspace B is as it was"):
            self.expect_layout("workspace B", b_expected, state)

        with self.step(f"switch from B back to A with {PREVIOUS_WORKSPACE!r}"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)

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
            a_index = self.state()["active_workspace"]

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site, open README.md at line 3"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")
            self.go_to_line(3)

        with self.step(f"workspace B: rename it to {DOCS_NAME!r}"):
            self.rename_workspace(DOCS_NAME)
            b_index = self.state()["active_workspace"]

        a_expected = {
            "name": PAY_NAME,
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}, {"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "split": "vertical",
            "active_tab": "ledger.rs",
            "cursor": {"row": 18, "column": 1},
            "drawn_tabs": ["invoice.rs", "ledger.rs"],
        }
        b_expected = {
            "name": DOCS_NAME,
            "worktrees": [real(docs_site)],
            "panes": [{"tabs": ["README.md"], "active_tab": "README.md"}],
            "split": None,
            "active_tab": "README.md",
            "cursor": {"row": 3, "column": 1},
            "drawn_tabs": ["README.md"],
        }
        with self.step("check workspace B and its name in the window title"):
            state = self.wait_for_values("workspace B", {"active_workspace": b_index, **b_expected}, layout, SHOW_TIMEOUT)
            self.wait_for_title(DOCS_NAME)
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r} and check it and its name in the window title"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)
            self.expect_layout("workspace A", a_expected, state)
            self.wait_for_title(PAY_NAME)

        with self.step(f"leave workspace B active: switch back with {NEXT_WORKSPACE!r}"):
            state = self.switch_workspace(NEXT_WORKSPACE, b_index, "workspace B", b_expected)
            self.wait_for_title(DOCS_NAME)

        with self.step("check both workspaces before the restart"):
            before = workspace_list(state)
            self.expect_equal(
                "workspaces",
                sorted([{"name": PAY_NAME, "worktrees": [real(billing_api)]}, {"name": DOCS_NAME, "worktrees": [real(docs_site)]}], key=str),
                sorted(before, key=str),
            )

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that both workspaces came back with names and folders, and B, the active one, is active again with its tab and cursor"):
            state = self.wait_for_values(
                "the restored window",
                {"workspace_count": 2, "active_workspace": b_index, "modal": None, **b_expected},
                layout,
                RESTORE_TIMEOUT,
            )
            self.wait_for_values("the restored workspaces", {"workspaces": before}, lambda state: {"workspaces": workspace_list(state)}, SHOW_TIMEOUT)
            self.expect_equal("window count", 1, state["window_count"])

        with self.step("check workspace B's name in the window title"):
            state = self.wait_for_title(DOCS_NAME)
            self.expect_equal("workspace name", DOCS_NAME, active(state)["name"])

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r}"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)

        with self.step("check workspace A: tabs, split, cursor and its name in the window title"):
            self.expect_layout("workspace A", a_expected, state)
            state = self.wait_for_title(PAY_NAME)
            self.expect_equal("workspace name", PAY_NAME, active(state)["name"])
            self.expect_equal("workspaces", before, workspace_list(state))

    def test_ac5_two_workspaces_on_the_same_folder_come_back_apart(self) -> None:
        """Two workspaces with exactly the same folder keep their own name, tab and cursor across a restart (Zed keyed saved workspaces by their folders)."""
        ledger_name = "[PAY-1300] (billing-api) Ledger export"

        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])

        with self.step(f"workspace A: open invoice.rs at line 20 and rename it to {PAY_NAME!r}"):
            self.open_file("invoice.rs")
            self.go_to_line(20)
            self.rename_workspace(PAY_NAME)
            a_index = self.state()["active_workspace"]

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add billing-api too, open ledger.rs at line 12 and rename it to {ledger_name!r}"):
            self.new_workspace()
            self.add_folder(billing_api)
            self.open_file("ledger.rs")
            self.go_to_line(12)
            self.rename_workspace(ledger_name)
            b_index = self.state()["active_workspace"]
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")

        a_expected = {
            "name": PAY_NAME,
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}],
            "split": None,
            "active_tab": "invoice.rs",
            "cursor": {"row": 20, "column": 1},
            "drawn_tabs": ["invoice.rs"],
        }
        b_expected = {
            "name": ledger_name,
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "split": None,
            "active_tab": "ledger.rs",
            "cursor": {"row": 12, "column": 1},
            "drawn_tabs": ["ledger.rs"],
        }

        with self.step(f"check workspace A with {PREVIOUS_WORKSPACE!r}, then leave workspace B active with {NEXT_WORKSPACE!r}"):
            self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)
            self.wait_for_title(PAY_NAME)
            state = self.switch_workspace(NEXT_WORKSPACE, b_index, "workspace B", b_expected)
            self.wait_for_title(ledger_name)
            before = workspace_list(state)
            self.expect_equal(
                "workspaces",
                sorted([{"name": PAY_NAME, "worktrees": [real(billing_api)]}, {"name": ledger_name, "worktrees": [real(billing_api)]}], key=str),
                sorted(before, key=str),
            )

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that both workspaces came back and B, the active one, is active again with its tab and cursor"):
            state = self.wait_for_values(
                "the restored window",
                {"workspace_count": 2, "active_workspace": b_index, "modal": None, **b_expected},
                layout,
                RESTORE_TIMEOUT,
            )
            self.wait_for_values("the restored workspaces", {"workspaces": before}, lambda state: {"workspaces": workspace_list(state)}, SHOW_TIMEOUT)
            self.expect_equal("window count", 1, state["window_count"])
            self.wait_for_title(ledger_name)

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r} and check its tab, cursor and name in the window title"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)
            self.expect_layout("workspace A", a_expected, state)
            state = self.wait_for_title(PAY_NAME)
            self.expect_equal("workspaces", before, workspace_list(state))

    def test_ac5_each_workspace_project_panel_comes_back_after_quit_and_start(self) -> None:
        """Each workspace's project panel comes back after a restart as it was: open or closed, roots, expanded folders and selected entry. No tab stays open, so nothing but the saved panel state can expand or select an entry after the restart."""
        a_panel = {"open": True, "roots": ["billing-api"], "expanded": ["billing-api/src"], "selected": "billing-api/src/invoice.rs"}
        b_panel = {"open": False, "roots": ["docs-site"], "expanded": ["docs-site/guide"], "selected": "docs-site/guide/getting-started.md"}

        def panel_of(state: dict[str, Any]) -> dict[str, Any]:
            return panel_summary(state["project_panel"]) or {}

        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])
            self.wait_for_values("the project panel at the start", {"open": True, "roots": ["billing-api"]}, panel_of, SHOW_TIMEOUT)

        with self.step("workspace A: open invoice.rs, so the project panel expands src and selects invoice.rs"):
            self.open_file("invoice.rs")
            self.wait_for_values("workspace A's project panel with invoice.rs", a_panel, panel_of, SHOW_TIMEOUT)

        with self.step("workspace A: close the tab with ctrl-w and check that the panel keeps src expanded and invoice.rs selected"):
            self.keys("ctrl-w")
            self.wait_for(lambda state: state["tabs"] == [] and state["prompt"] is None, SHOW_TIMEOUT, "the tab of invoice.rs to close")
            self.wait_for_values("workspace A's project panel", a_panel, panel_of, SHOW_TIMEOUT)
            a_index = self.state()["active_workspace"]

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site and show the project panel"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.show_project_panel()
            b_index = self.state()["active_workspace"]
            if b_index == a_index:
                raise AssertionError(f"expected workspace B at another index than A ({a_index}), actual {b_index}")

        with self.step("workspace B: open getting-started.md, so the project panel expands guide and selects it, then close the tab with ctrl-w"):
            self.open_file("getting-started.md")
            self.wait_for_values("workspace B's project panel with getting-started.md", {**b_panel, "open": True}, panel_of, SHOW_TIMEOUT)
            self.keys("ctrl-w")
            self.wait_for(lambda state: state["tabs"] == [] and state["prompt"] is None, SHOW_TIMEOUT, "the tab of getting-started.md to close")

        with self.step(f"workspace B: close the project panel with {CLOSE_ALL_DOCKS!r}"):
            self.run_command(CLOSE_ALL_DOCKS)
            self.wait_for_values("workspace B's closed project panel", b_panel, panel_of, SHOW_TIMEOUT)

        with self.step(f"check workspace A's project panel with {PREVIOUS_WORKSPACE!r}, then leave workspace B active with {NEXT_WORKSPACE!r}"):
            self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", {"tabs": [], "panel": a_panel})
            self.switch_workspace(NEXT_WORKSPACE, b_index, "workspace B", {"tabs": [], "panel": b_panel})

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that workspace B, the active one, came back with its project panel closed, guide expanded and getting-started.md selected"):
            state = self.wait_for_values(
                "the restored workspace B",
                {"workspace_count": 2, "active_workspace": b_index, "modal": None, "worktrees": [real(docs_site)], "tabs": [], "panel": b_panel},
                layout,
                RESTORE_TIMEOUT,
            )
            self.expect_equal("workspace B's project panel", b_panel, panel_of(state))

        with self.step(f"switch to workspace A with {PREVIOUS_WORKSPACE!r} and check that its project panel came back open with src expanded and invoice.rs selected"):
            state = self.switch_workspace(
                PREVIOUS_WORKSPACE, a_index, "workspace A", {"worktrees": [real(billing_api)], "tabs": [], "panel": a_panel}
            )
            self.expect_equal("workspace A's project panel", a_panel, panel_of(state))

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

        with self.step("workspace A: open invoice.rs"):
            self.open_file("invoice.rs")

        a_expected = {
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}],
            "active_tab": "invoice.rs",
            "dirty_tabs": [],
            "drawn_tabs": ["invoice.rs"],
        }

        with self.step(f"workspace B: run {NEW_WORKSPACE!r}, add docs-site, open README.md"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")

        with self.step("workspace B: make an unsaved edit in README.md"):
            self.type("Draft: ")
            self.wait_for_values(
                "workspace B with the edited README.md",
                {"dirty_tabs": ["README.md"], "unsaved_texts": [{"tab": "README.md", "text": "Draft: " + readme_before}]},
                layout,
                10,
            )
            self.expect_equal("README.md on disk", readme_before, readme.read_text())

        with self.step(f"run {CLOSE_WORKSPACE!r}"):
            self.run_command(CLOSE_WORKSPACE)

        with self.step("check that Cod asks before closing, with the prompt stock Zed shows when a window with this unsaved file closes"):
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save README.md")
            self.expect_equal("prompt", save_prompt("README.md"), state["prompt"])
            self.expect_equal("workspace count while asking", 2, state["workspace_count"])

        # Zed's save prompt highlights its first answer, Save (checked above).
        with self.step("answer the prompt with enter (Save)"):
            self.keys("enter")

        with self.step("check that the edit was saved, the workspace is gone and the other one is active and drawn"):
            state = self.wait_for_values(
                "workspace A after the close", {"prompt": None, "workspace_count": 1, "modal": None, **a_expected}, layout, SHOW_TIMEOUT
            )
            self.expect_equal("folders of the active workspace", [real(billing_api)], real_paths(active(state)["worktrees"]))
            self.expect_equal("project folders", [real(billing_api)], real_paths(state["worktrees"]))
            self.expect_equal("workspaces", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in state["workspaces"]])
            self.expect_equal("drawn workspaces", [True], drawn_list(state))
            deadline = time.monotonic() + 10
            while readme.read_text() != "Draft: " + readme_before and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("README.md on disk", "Draft: " + readme_before, readme.read_text())

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that the closed workspace did not come back: wait until workspace A is restored with its tab, failing as soon as docs-site shows"):

            def restored(state: dict[str, Any]) -> bool:
                folders = [real_paths(workspace["worktrees"]) for workspace in state["workspaces"]]
                if any(real(docs_site) in paths for paths in folders):
                    raise AssertionError(f"expected the closed workspace with docs-site to stay closed, actual workspaces {folders}")
                return all(layout(state)[key] == value for key, value in a_expected.items())

            state = self.wait_for(restored, RESTORE_TIMEOUT, "workspace A with invoice.rs to be restored")
            self.expect_equal("workspaces", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in state["workspaces"]])
            self.expect_equal("workspace count", 1, state["workspace_count"])
            state = self.stays(lambda state: state["workspace_count"] == 1, SETTLE_TIME, "exactly one workspace")
            self.expect_equal("workspaces", [[real(billing_api)]], [real_paths(w["worktrees"]) for w in state["workspaces"]])

        with self.step(f"close the last workspace with {CLOSE_WORKSPACE!r}"):
            self.run_command(CLOSE_WORKSPACE)

        with self.step("check that one empty workspace is left in the window: one empty pane, no tab, no prompt"):
            state = self.wait_for_values(
                "the empty workspace",
                {
                    "workspace_count": 1,
                    "worktrees": [],
                    "panes": [{"tabs": [], "active_tab": None}],
                    "tabs": [],
                    "active_tab": None,
                    "dirty_tabs": [],
                    "drawn_tabs": [],
                    "prompt": None,
                    "modal": None,
                },
                layout,
                SHOW_TIMEOUT,
            )
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("tabs", [], state["tabs"])
            self.expect_equal("prompt", None, state["prompt"])
            self.expect_equal("workspaces", [[]], [real_paths(w["worktrees"]) for w in state["workspaces"]])
            state = self.wait_for_values("the drawn workspaces", {"drawn": [True]}, lambda state: {"drawn": drawn_list(state)}, SHOW_TIMEOUT)
            if state["project_panel"] is not None:
                self.expect_equal("project panel roots", [], state["project_panel"]["roots"])

    def test_regression_palette_after_restoring_an_unsaved_edit_in_one_of_two_workspaces(self) -> None:
        """Beta run 39128519357: two workspaces on the same folder, the active one with an unsaved edit; after quit and start the command palette did not open. Then close with Cancel (the workspace stays) and with Save, restart: the closed workspace stays gone."""
        b_name = "[PAY-1240] (billing-api) Ledger export"
        ledger_draft = "// draft "
        invoice_draft = "Draft "
        with self.step("create billing-api"):
            repo = self.make_repo("billing-api", BILLING_API)
            ledger_path = repo / "src" / "ledger.rs"
            invoice_path = repo / "src" / "invoice.rs"
            ledger_edited = insert_at_line(LEDGER_RS, 18, ledger_draft)
            invoice_edited = insert_at_line(INVOICE_RS, 9, invoice_draft)
            folder = [real(repo)]

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(repo, persistent=True)
            self.wait_for_worktrees([repo])

        with self.step("workspace A: open invoice.rs and ledger.rs, type an unsaved edit at line 18 of ledger.rs"):
            self.open_file("invoice.rs")
            self.open_file("ledger.rs")
            self.go_to_line(18)
            self.type(ledger_draft)
            self.wait_for_values(
                "the edited ledger.rs",
                {"dirty_tabs": ["ledger.rs"], "unsaved_texts": [{"tab": "ledger.rs", "text": ledger_edited}]},
                layout,
                SHOW_TIMEOUT,
            )

        with self.step(f"workspace A: rename it to {PAY_NAME!r}"):
            self.rename_workspace(PAY_NAME)
            a_index = self.state()["active_workspace"]

        with self.step("workspace B: new workspace on the SAME folder, open invoice.rs at line 9, rename it"):
            self.new_workspace()
            self.add_folder(repo)
            self.open_file("invoice.rs")
            self.go_to_line(9)
            self.rename_workspace(b_name)
            b_index = self.state()["active_workspace"]

        a_expected = {
            "name": PAY_NAME,
            "worktrees": folder,
            "panes": [{"tabs": ["invoice.rs", "ledger.rs"], "active_tab": "ledger.rs"}],
            "active_tab": "ledger.rs",
            "cursor": {"row": 18, "column": 1 + len(ledger_draft)},
            "dirty_tabs": ["ledger.rs"],
            "unsaved_texts": [{"tab": "ledger.rs", "text": ledger_edited}],
            "window_count": 1,
        }
        b_expected = {
            "name": b_name,
            "worktrees": folder,
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}],
            "active_tab": "invoice.rs",
            "cursor": {"row": 9, "column": 1},
            "dirty_tabs": [],
            "unsaved_texts": [],
            "window_count": 1,
        }

        with self.step(f"make workspace A active with {PREVIOUS_WORKSPACE!r}, then quit with ctrl-q and start again"):
            self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)
            self.expect_equal("ledger.rs on disk", LEDGER_RS, ledger_path.read_text())
            self.restart_cod()

        with self.step("both workspaces come back, A active with its unsaved edit"):
            self.wait_for_values(
                "the restored workspace A",
                {"workspace_count": 2, "active_workspace": a_index, "modal": None, **a_expected},
                layout,
                RESTORE_TIMEOUT,
            )
            self.wait_for_title(PAY_NAME)

        with self.step(f"after the restart, the command palette runs {NEXT_WORKSPACE!r}: workspace B"):
            self.switch_workspace(NEXT_WORKSPACE, b_index, "the restored workspace B", b_expected)

        with self.step("workspace B: type an unsaved edit in invoice.rs"):
            self.type(invoice_draft)
            self.wait_for_values(
                "the edited invoice.rs",
                {"dirty_tabs": ["invoice.rs"], "unsaved_texts": [{"tab": "invoice.rs", "text": invoice_edited}]},
                layout,
                SHOW_TIMEOUT,
            )

        with self.step(f"close workspace B with {CLOSE_WORKSPACE!r} and answer escape (Cancel): B stays with its edit"):
            self.run_command(CLOSE_WORKSPACE)
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save invoice.rs")
            self.expect_equal("prompt answers", SAVE_ANSWERS, state["prompt"]["answers"])
            self.keys("escape")
            self.wait_for_values(
                "workspace B after Cancel",
                {"prompt": None, "workspace_count": 2, "active_workspace": b_index, "name": b_name, "dirty_tabs": ["invoice.rs"]},
                layout,
                SHOW_TIMEOUT,
            )
            self.stays(lambda state: state["workspace_count"] == 2 and state["prompt"] is None, SETTLE_TIME, "two workspaces after Cancel")
            self.expect_equal("invoice.rs on disk", INVOICE_RS, invoice_path.read_text())

        with self.step(f"close workspace B again with {CLOSE_WORKSPACE!r} and answer enter (Save)"):
            self.run_command(CLOSE_WORKSPACE)
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "the save prompt again")
            self.expect_equal("highlighted answer", 0, state["prompt"]["highlighted"])
            self.keys("enter")
            self.wait_for_values(
                "workspace A after the close", {"prompt": None, "workspace_count": 1, "modal": None, **a_expected}, layout, SHOW_TIMEOUT
            )
            deadline = time.monotonic() + 10
            while invoice_path.read_text() != invoice_edited and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("invoice.rs on disk", invoice_edited, invoice_path.read_text())

        with self.step("quit and start again: only workspace A comes back, B stays gone"):
            self.restart_cod()
            self.wait_for_values("the restored workspace A", {"modal": None, **a_expected}, layout, RESTORE_TIMEOUT)
            state = self.stays(lambda state: state["workspace_count"] == 1, SETTLE_TIME, "exactly workspace A")
            self.expect_equal("workspaces", [{"name": PAY_NAME, "worktrees": folder}], workspace_list(state))

    # Crash safety of the restore (review finding 4)

    def test_workspaces_come_back_after_cod_is_killed_while_it_restores_them(self) -> None:
        """Three workspaces, two of them on the same folder, come back with their names, folders, tabs and the active one after Cod is killed with SIGKILL (no clean quit) at several points of its start and restore, each time started again on the same data folder."""
        ledger_name = "[PAY-1300] (billing-api) Ledger export"

        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])

        with self.step(f"workspace A: open invoice.rs at line 20 and rename it to {PAY_NAME!r}"):
            self.open_file("invoice.rs")
            self.go_to_line(20)
            self.rename_workspace(PAY_NAME)
            a_index = self.state()["active_workspace"]

        with self.step(f"workspace B: new workspace on the SAME folder billing-api, open ledger.rs at line 12, rename it to {ledger_name!r}"):
            self.new_workspace()
            self.add_folder(billing_api)
            self.open_file("ledger.rs")
            self.go_to_line(12)
            self.rename_workspace(ledger_name)
            b_index = self.state()["active_workspace"]

        with self.step(f"workspace C: new workspace on docs-site, open README.md at line 5, rename it to {DOCS_NAME!r}"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")
            self.go_to_line(5)
            self.rename_workspace(DOCS_NAME)
            c_index = self.state()["active_workspace"]
            self.expect_equal("three workspace indexes", 3, len({a_index, b_index, c_index}))

        a_expected = {
            "name": PAY_NAME,
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["invoice.rs"], "active_tab": "invoice.rs"}],
            "active_tab": "invoice.rs",
            "cursor": {"row": 20, "column": 1},
            "window_count": 1,
        }
        b_expected = {
            "name": ledger_name,
            "worktrees": [real(billing_api)],
            "panes": [{"tabs": ["ledger.rs"], "active_tab": "ledger.rs"}],
            "active_tab": "ledger.rs",
            "cursor": {"row": 12, "column": 1},
            "window_count": 1,
        }
        c_expected = {
            "name": DOCS_NAME,
            "worktrees": [real(docs_site)],
            "panes": [{"tabs": ["README.md"], "active_tab": "README.md"}],
            "active_tab": "README.md",
            "cursor": {"row": 5, "column": 1},
            "window_count": 1,
        }

        with self.step(f"make workspace B active with {PREVIOUS_WORKSPACE!r}; A and C stay inactive with their files open"):
            state = self.switch_workspace(PREVIOUS_WORKSPACE, b_index, "workspace B", b_expected)
            before = workspace_list(state)
            self.expect_equal(
                "workspaces",
                sorted(
                    [
                        {"name": PAY_NAME, "worktrees": [real(billing_api)]},
                        {"name": ledger_name, "worktrees": [real(billing_api)]},
                        {"name": DOCS_NAME, "worktrees": [real(docs_site)]},
                    ],
                    key=str,
                ),
                sorted(before, key=str),
            )

        with self.step("quit with ctrl-q and start again once, so the window's workspace list is saved"):
            self.restart_cod()
            self.wait_for_values(
                "the restored window", {"workspace_count": 3, "active_workspace": b_index, "modal": None, **b_expected}, layout, RESTORE_TIMEOUT
            )
            self.wait_for_values("the restored workspaces", {"workspaces": before}, lambda state: {"workspaces": workspace_list(state)}, SHOW_TIMEOUT)

        with self.step("kill the restored Cod with SIGKILL, no clean quit"):
            self.kill_cod()

        for delay in KILL_DELAYS:
            with self.step(f"start Cod on the same data folder and kill it with SIGKILL {delay:g} s after the launch"):
                self.start_cod_again(connect=False)
                time.sleep(delay)
                self.kill_cod()

        with self.step("start Cod normally: all three workspaces come back, B active with its tab and cursor"):
            self.start_cod_again()
            self.wait_for_values(
                "the restored window", {"workspace_count": 3, "active_workspace": b_index, "modal": None, **b_expected}, layout, RESTORE_TIMEOUT
            )
            self.wait_for_values("the restored workspaces", {"workspaces": before}, lambda state: {"workspaces": workspace_list(state)}, SHOW_TIMEOUT)
            self.wait_for_title(ledger_name)

        with self.step("switch to A and to C and check their tabs, cursors and names"):
            self.switch_workspace(PREVIOUS_WORKSPACE, a_index, "workspace A", a_expected)
            self.wait_for_title(PAY_NAME)
            self.switch_workspace(NEXT_WORKSPACE, b_index, "workspace B", b_expected)
            self.switch_workspace(NEXT_WORKSPACE, c_index, "workspace C", c_expected)
            state = self.wait_for_title(DOCS_NAME)
            self.expect_equal("workspaces", before, workspace_list(state))

    # Moving a named workspace to a new window (review new minor)

    def test_a_named_workspace_moved_to_a_new_window_keeps_its_name(self) -> None:
        """Zed's "move project to new window" moves a named workspace into a window of its own; the workspace keeps its name there and after a restart."""

        def titles() -> list[str]:
            """The titles of Cod's windows as sway shows them."""
            return sorted(window["title"] or "" for window in self.compositor_windows())

        def two_named_windows(state: dict[str, Any]) -> bool:
            shown = titles()
            return (
                state["window_count"] == 2
                and state["workspace_count"] == 1
                and len(shown) == 2
                and any(DOCS_NAME in title for title in shown)
                and any(PAY_NAME in title for title in shown)
            )

        with self.step("create the fixture repos billing-api and docs-site"):
            billing_api = self.make_repo("billing-api", BILLING_API)
            docs_site = self.make_repo("docs-site", DOCS_SITE)

        with self.step("start Cod on billing-api, keeping its session"):
            self.start_cod(billing_api, persistent=True)
            self.wait_for_worktrees([billing_api])

        with self.step(f"workspace A: open invoice.rs and rename it to {PAY_NAME!r}"):
            self.open_file("invoice.rs")
            self.rename_workspace(PAY_NAME)

        with self.step(f"workspace C: new workspace on docs-site, open README.md, rename it to {DOCS_NAME!r}"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")
            self.rename_workspace(DOCS_NAME)
            self.wait_for_title(DOCS_NAME)
            self.expect_equal("workspace count", 2, self.state()["workspace_count"])

        with self.step(f"move workspace C to a new window with {MOVE_PROJECT_TO_NEW_WINDOW!r}"):
            self.run_command(MOVE_PROJECT_TO_NEW_WINDOW)

        with self.step(f"two windows: one shows {DOCS_NAME!r}, the other {PAY_NAME!r}, one workspace each"):
            state = self.wait_for(two_named_windows, SHOW_TIMEOUT, f"two windows titled {DOCS_NAME!r} and {PAY_NAME!r}")
            self.expect_equal("workspace in the reported window", 1, len(state["workspaces"]))
            self.expect_equal(
                "the reported window's workspace",
                True,
                workspace_list(state)
                in (
                    [{"name": DOCS_NAME, "worktrees": [real(docs_site)]}],
                    [{"name": PAY_NAME, "worktrees": [real(billing_api)]}],
                ),
            )

        with self.step("quit with ctrl-q and start again: both windows come back with their names"):
            self.restart_cod()
            self.wait_for(two_named_windows, RESTORE_TIMEOUT, f"two restored windows titled {DOCS_NAME!r} and {PAY_NAME!r}")

    # Remote (SSH) workspaces on a disposable sshd on 127.0.0.1 (tests/e2e/loopback_ssh.py). Each test is skipped with the reason when the service cannot run here.

    def loopback(self, name: str) -> loopback_ssh.LoopbackSsh:
        """A started loopback sshd for this test with the remote server binary in place, or a skip with the reason (a failure with COD_E2E_REQUIRE_SSH=1, as in CI)."""
        reason = loopback_ssh.missing_reason()
        if reason is not None:
            self.loopback_unavailable(reason)
        ssh = loopback_ssh.LoopbackSsh(self.workdir / f"ssh-{name}")
        reason = ssh.start()
        if reason is not None:
            self.loopback_unavailable(reason)
        self.addCleanup(ssh.stop)
        ssh.install_remote_server()
        return ssh

    def loopback_unavailable(self, reason: str) -> None:
        if loopback_ssh.required():
            raise AssertionError(f"loopback SSH unavailable and COD_E2E_REQUIRE_SSH=1: {reason}")
        print(f"\nloopback SSH unavailable, skipping: {reason}", file=sys.stderr)
        self.skipTest(f"loopback SSH unavailable: {reason}")

    def start_cod_on_remote(self, connections: list[loopback_ssh.LoopbackSsh], url: str) -> None:
        """Starts a persistent Cod on the ssh:// url, as `cod ssh://user@host:port/path` does, with every service's key and known_hosts in ssh_connections."""
        if self.cod is not None:
            raise RuntimeError("Cod was already started in this test")
        self.cod = Cod(self.workdir, None, persistent=True)
        self.cod.extra_settings = {"ssh_connections": [{**ssh.connection_setting(), "projects": []} for ssh in connections]}
        self.cod.extra_args = [url]
        self.addCleanup(self._stop_cod)
        self.addCleanup(self._save_final_artifacts)
        self.cod.start()
        self.cod.connect(REMOTE_TIMEOUT)

    def open_remote_and_local(self, ssh: loopback_ssh.LoopbackSsh) -> tuple[Path, Path, list[dict[str, Any]]]:
        """One window: workspace 1 is the remote ledger-remote with ledger.rs open, workspace 2 the local billing-api with invoice.rs open. Returns both folders and the workspace list."""
        remote = self.make_repo("ledger-remote", LEDGER_REMOTE)
        local = self.make_repo("billing-api", BILLING_API)
        self.start_cod_on_remote([ssh], ssh.url(remote))
        self.wait_for(
            lambda state: real_paths(state["worktrees"]) == [real(remote)], REMOTE_TIMEOUT, "the remote folder ledger-remote over SSH"
        )
        self.open_file("ledger.rs")
        self.new_workspace()
        self.add_folder(local)
        self.open_file("invoice.rs")
        state = self.state()
        self.expect_equal("window count", 1, state["window_count"])
        self.expect_equal("workspace count", 2, state["workspace_count"])
        return remote, local, workspace_list(state)

    def expect_each_once(self, state: dict[str, Any], folders: list[Path]) -> None:
        listed = [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)]
        for folder in folders:
            self.expect_equal(f"workspaces showing {folder.name}", 1, listed.count([real(folder)]))

    def show_workspace_of(self, folder: Path, tab: str) -> dict[str, Any]:
        """Switches with "workspace: next workspace" until the folder's workspace is active, then waits for its tab."""
        for _ in range(self.state()["workspace_count"]):
            if real_paths(self.state()["worktrees"]) == [real(folder)]:
                break
            before = self.state()["active_workspace"]
            self.run_command(NEXT_WORKSPACE)
            self.wait_for(lambda state: state["active_workspace"] != before and state["modal"] is None, SHOW_TIMEOUT, "the next workspace")
        return self.wait_for(
            lambda state: real_paths(state["worktrees"]) == [real(folder)] and state["active_tab"] == tab,
            REMOTE_TIMEOUT,
            f"{folder.name} with {tab} in the active tab",
        )

    def cancel_remote_connection_by_keys(self) -> None:
        """Waits for the remote connection modal or its prompt (a prompt the driver cannot read has a null message), then presses escape until neither shows."""
        self.wait_for(
            lambda state: state["modal"] == "other" or state["prompt"] is not None,
            REMOTE_TIMEOUT,
            "the remote connection modal or its failure prompt",
        )
        for _ in range(3):
            state = self.state()
            if state["modal"] is None and state["prompt"] is None:
                break
            self.keys("escape")
            time.sleep(SETTLE_TIME)
        self.wait_for(lambda state: state["modal"] is None and state["prompt"] is None, SHOW_TIMEOUT, "no modal and no prompt after cancel")

    def expect_remote_flags(self, state: dict[str, Any], remote: list[Path]) -> None:
        """When the driver reports "remote" for each workspace, the workspaces showing a folder in `remote` are remote and the others are local."""
        workspaces = state["workspaces"]
        if not workspaces or not all("remote" in workspace for workspace in workspaces):
            return
        remote_paths = [[real(folder)] for folder in remote]
        for workspace in workspaces:
            paths = real_paths(workspace["worktrees"])
            if not paths:
                continue
            self.expect_equal(f"remote flag of the workspace showing {paths}", paths in remote_paths, workspace["remote"])

    def test_remote_and_local_workspaces_come_back_once_after_quit_and_start(self) -> None:
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test"):
            ssh = self.loopback("a")

        with self.step("one window: the remote ledger-remote with ledger.rs, and a local billing-api with invoice.rs"):
            remote, local, before = self.open_remote_and_local(ssh)

        with self.step("quit with ctrl-q and start again"):
            self.restart_cod(REMOTE_TIMEOUT)

        with self.step("check that one window has both workspaces, each exactly once"):
            state = self.wait_for(
                lambda state: state["workspace_count"] == 2 and sorted(workspace_list(state), key=str) == sorted(before, key=str),
                REMOTE_TIMEOUT,
                "both workspaces to come back",
            )
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_each_once(state, [remote, local])

        with self.step("check the remote workspace: its folder over SSH and ledger.rs open"):
            self.show_workspace_of(remote, "ledger.rs")

        with self.step("check the local workspace: billing-api and invoice.rs open"):
            state = self.show_workspace_of(local, "invoice.rs")
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])

    def test_remote_workspace_survives_a_cancelled_reconnection(self) -> None:
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test"):
            ssh = self.loopback("b")

        with self.step("one window: the remote ledger-remote with ledger.rs, and a local billing-api with invoice.rs"):
            remote, local, _ = self.open_remote_and_local(ssh)

        with self.step("quit with ctrl-q, stop sshd, start Cod again so the remote reconnection fails"):
            assert self.cod is not None
            self.cod.quit()
            ssh.stop()
            self.cod.folder = None
            self.cod.start()
            self.cod.connect(REMOTE_TIMEOUT)

        with self.step("cancel the remote connection by keys (escape) once its modal or prompt shows"):
            self.cancel_remote_connection_by_keys()

        with self.step("start sshd again, quit with ctrl-q and start Cod again"):
            reason = ssh.start()
            if reason is not None:
                raise AssertionError(f"the loopback sshd did not start again: {reason}")
            self.restart_cod(REMOTE_TIMEOUT)

        with self.step("check that the remote workspace came back exactly once, with ledger.rs, next to the local one"):
            state = self.wait_for(
                lambda state: [real(remote)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)],
                REMOTE_TIMEOUT,
                "the remote workspace to come back",
            )
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])
            state = self.show_workspace_of(remote, "ledger.rs")
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])

    def test_active_remote_workspace_comes_back_active_with_the_local_one_switchable(self) -> None:
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test"):
            ssh = self.loopback("active")

        with self.step("one window: the remote ledger-remote with ledger.rs, and a local billing-api with invoice.rs"):
            remote, local, before = self.open_remote_and_local(ssh)

        with self.step("make the remote workspace the active one"):
            state = self.show_workspace_of(remote, "ledger.rs")
            self.expect_remote_flags(state, [remote])

        with self.step("quit with ctrl-q and start again"):
            self.restart_cod(REMOTE_TIMEOUT)

        with self.step("check that the remote workspace is active with ledger.rs and the local one is listed once"):
            state = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)]
                and state["active_tab"] == "ledger.rs"
                and [real(local)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)],
                REMOTE_TIMEOUT,
                "the remote workspace active with ledger.rs and the local billing-api listed",
            )
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_each_once(state, [remote, local])
            self.expect_remote_flags(state, [remote])

        with self.step("switch to the local workspace: billing-api with invoice.rs"):
            state = self.show_workspace_of(local, "invoice.rs")
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal(
                "workspaces after restart",
                sorted(before, key=str),
                sorted([workspace for workspace in workspace_list(state) if workspace["worktrees"]], key=str),
            )
            self.expect_remote_flags(state, [remote])

        with self.step("switch back to the remote workspace: ledger.rs is still open"):
            self.show_workspace_of(remote, "ledger.rs")

    def test_remote_workspace_survives_a_cancel_while_the_remote_server_starts(self) -> None:
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test"):
            ssh = self.loopback("slow")

        with self.step("one window: the remote ledger-remote with ledger.rs, and a local billing-api with invoice.rs"):
            remote, local, _ = self.open_remote_and_local(ssh)

        with self.step("quit with ctrl-q, make the remote server start slowly (sshd stays up), start Cod again"):
            assert self.cod is not None
            self.cod.quit()
            ssh.install_slow_remote_server(REMOTE_SERVER_DELAY)
            self.cod.folder = None
            self.cod.start()
            self.cod.connect(REMOTE_TIMEOUT)

        with self.step("cancel the remote connection by keys (escape) while the remote server is still starting"):
            self.cancel_remote_connection_by_keys()

        with self.step("put the real remote server back, quit with ctrl-q and start Cod again"):
            ssh.install_remote_server()
            self.restart_cod(REMOTE_TIMEOUT)

        with self.step("check that the remote workspace came back exactly once, with ledger.rs, next to the local one"):
            state = self.wait_for(
                lambda state: [real(remote)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)],
                REMOTE_TIMEOUT,
                "the remote workspace to come back",
            )
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])
            state = self.show_workspace_of(remote, "ledger.rs")
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_remote_flags(state, [remote])

    def test_window_closed_while_its_remote_workspace_connects_does_not_come_back(self) -> None:
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test"):
            ssh = self.loopback("close")
            docs_site = self.make_repo("docs-site", DOCS_SITE)

        with self.step("one window: the remote ledger-remote with ledger.rs, and a local billing-api with invoice.rs"):
            remote, local, _ = self.open_remote_and_local(ssh)

        with self.step("quit with ctrl-q, make the remote server start slowly (sshd stays up), start Cod again"):
            assert self.cod is not None
            self.cod.quit()
            ssh.install_slow_remote_server(REMOTE_SERVER_DELAY)
            self.cod.folder = None
            self.cod.start()
            self.cod.connect(REMOTE_TIMEOUT)

        with self.step("close the restoring window with ctrl-shift-w while its remote workspace connects"):
            self.wait_for(
                lambda state: state["modal"] == "other" or state["prompt"] is not None,
                REMOTE_TIMEOUT,
                "the remote connection modal while the remote server starts",
            )
            self.keys("ctrl-shift-w")

        with self.step("put the real remote server back and open ledger-remote over SSH again (cod ssh://...)"):
            ssh.install_remote_server()
            assert self.cod is not None
            # On Linux Zed quits when its last window closes (bind_on_window_closed in crates/zed/src/zed.rs).
            assert self.cod.process is not None
            with contextlib.suppress(subprocess.TimeoutExpired):
                self.cod.process.wait(timeout=RESTORE_TIMEOUT)
            if self.cod.process.poll() is not None:
                # Closing the last window quit Cod: start it again on the ssh:// url.
                status = self.cod.process.returncode
                self.expect_equal("exit status after the last window closed", 0, status)
                self.cod.process = None
                if self.cod.driver is not None:
                    self.cod.driver.close()
                    self.cod.driver = None
                # The exited Cod leaves its socket file behind; connect() must not see it before the next Cod listens.
                with contextlib.suppress(FileNotFoundError):
                    self.cod.socket_path.unlink()
                self.cod.folder = None
                self.cod.extra_args = [ssh.url(remote)]
                self.cod.start()
                self.cod.connect(REMOTE_TIMEOUT)
            else:
                self.expect_equal("Cod windows after ctrl-shift-w", [], self.compositor_windows())
                env = cod_environment(os.environ, self.cod.preview_dir, self.cod.socket_path, self.cod.home, persistent=True)
                reopen = subprocess.run(
                    cod_command(self.cod.run_dir, None) + [ssh.url(remote)], cwd=self.workdir, env=env, capture_output=True, timeout=60
                )
                self.expect_equal("exit status of the second open", 0, reopen.returncode)
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)], REMOTE_TIMEOUT, "ledger-remote over SSH again"
            )
            self.open_file("ledger.rs")

        with self.step("add local work: a new workspace on docs-site with README.md"):
            self.new_workspace()
            self.add_folder(docs_site)
            self.open_file("README.md")

        with self.step("quit with ctrl-q and start again"):
            self.restart_cod(REMOTE_TIMEOUT)

        with self.step("check that one window has the reopened remote and docs-site once each, and the closed billing-api window is gone"):
            state = self.wait_for(
                lambda state: [real(remote)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)]
                and [real(docs_site)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)],
                REMOTE_TIMEOUT,
                "the reopened remote workspace and docs-site to come back",
            )
            self.stays(lambda state: state["window_count"] == 1, SETTLE_TIME, "one window after restart")
            state = self.state()
            self.expect_each_once(state, [remote, docs_site])
            listed = [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)]
            self.expect_equal("workspaces showing billing-api (closed with its window)", 0, listed.count([real(local)]))
            state = self.show_workspace_of(remote, "ledger.rs")
            self.expect_each_once(state, [remote, docs_site])
            state = self.show_workspace_of(docs_site, "README.md")
            self.expect_each_once(state, [remote, docs_site])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_remote_flags(state, [remote])

    def test_same_folder_on_another_ssh_port_does_not_take_the_first_hosts_saved_workspace(self) -> None:
        with self.step("start two loopback sshd services on two ports"):
            first = self.loopback("first")
            second = self.loopback("second")
            remote = self.make_repo("ledger-remote", LEDGER_REMOTE)

        with self.step("open ledger-remote over the first port, open ledger.rs, quit and start again"):
            self.start_cod_on_remote([first, second], first.url(remote))
            self.wait_for(lambda state: real_paths(state["worktrees"]) == [real(remote)], REMOTE_TIMEOUT, "ledger-remote over the first port")
            self.open_file("ledger.rs")
            self.restart_cod(REMOTE_TIMEOUT)
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] == "ledger.rs",
                REMOTE_TIMEOUT,
                "the first port's workspace with ledger.rs to come back",
            )

        with self.step("open the same folder over the second port from the command line (cod ssh://...)"):
            assert self.cod is not None
            env = cod_environment(os.environ, self.cod.preview_dir, self.cod.socket_path, self.cod.home, persistent=True)
            second_open = subprocess.run(
                cod_command(self.cod.run_dir, None) + [second.url(remote)], cwd=self.workdir, env=env, capture_output=True, timeout=60
            )
            self.expect_equal("exit status of the second open", 0, second_open.returncode)

        with self.step("check that the second port's workspace is a fresh one: the folder with no tab, and the first keeps ledger.rs"):
            state = self.wait_for(lambda state: state["window_count"] == 2, REMOTE_TIMEOUT, "a second window for the second port")
            fresh = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] is None,
                REMOTE_TIMEOUT,
                "the second port's window with ledger-remote and no tab (not the first port's saved layout)",
            )
            self.stays(lambda state: state["tabs"] == [], SETTLE_TIME, "the second port's window having no tab")
            self.expect_equal("window count", 2, fresh["window_count"])

    def open_from_command_line(self, url: str) -> None:
        """Opens the url in the running Cod as `cod <url>` does, and checks the command's exit status."""
        assert self.cod is not None
        env = cod_environment(os.environ, self.cod.preview_dir, self.cod.socket_path, self.cod.home, persistent=True)
        result = subprocess.run(cod_command(self.cod.run_dir, None) + [url], cwd=self.workdir, env=env, capture_output=True, timeout=60)
        self.expect_equal(f"exit status of cod {url}", 0, result.returncode)

    def test_first_ssh_host_keeps_its_saved_layout_after_the_same_folder_opens_on_another_port(self) -> None:
        # The existing-window path (projects: open remote, then the host's "Open Folder" path picker) is not driven here: the driver reports that modal only as "other", with no selected entry and no query of its path picker, so keys there would be blind. The command line open is used instead.
        with self.step("start two loopback sshd services on two ports"):
            first = self.loopback("revisit-first")
            second = self.loopback("revisit-second")
            remote = self.make_repo("ledger-remote", LEDGER_REMOTE)

        with self.step("open ledger-remote over the first port, open ledger.rs, quit and start again"):
            self.start_cod_on_remote([first, second], first.url(remote))
            self.wait_for(lambda state: real_paths(state["worktrees"]) == [real(remote)], REMOTE_TIMEOUT, "ledger-remote over the first port")
            self.open_file("ledger.rs")
            self.restart_cod(REMOTE_TIMEOUT)
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] == "ledger.rs",
                REMOTE_TIMEOUT,
                "the first port's workspace with ledger.rs to come back",
            )

        with self.step("open the same folder over the second port (cod ssh://...): a fresh workspace with no tab"):
            self.open_from_command_line(second.url(remote))
            self.wait_for(lambda state: state["window_count"] == 2, REMOTE_TIMEOUT, "a second window for the second port")
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] is None,
                REMOTE_TIMEOUT,
                "the second port's window with ledger-remote and no tab",
            )

        with self.step("open the folder over the first port again (cod ssh://...): ledger.rs is still its layout"):
            self.open_from_command_line(first.url(remote))
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] == "ledger.rs",
                REMOTE_TIMEOUT,
                "the first port's workspace with ledger.rs again",
            )

        with self.step("quit with ctrl-q and start again: the first port's workspace keeps ledger.rs"):
            self.restart_cod(REMOTE_TIMEOUT)
            state = self.wait_for(
                lambda state: [real(remote)] in [real_paths(workspace["worktrees"]) for workspace in workspace_list(state)],
                REMOTE_TIMEOUT,
                "ledger-remote to come back",
            )
            if state["active_tab"] != "ledger.rs":
                self.open_from_command_line(first.url(remote))
            self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] == "ledger.rs",
                REMOTE_TIMEOUT,
                "the first port's workspace with ledger.rs after restart",
            )


if __name__ == "__main__":
    unittest.main()
