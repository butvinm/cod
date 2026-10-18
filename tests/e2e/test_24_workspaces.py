"""Story #24: create, name and switch workspaces in one window, kept across restarts. One test per acceptance criterion, keys only.

Fixtures are the throwaway git repos billing-api, billing-web and docs-site with invented content.
The workspace commands are run from the command palette (ctrl-shift-p) by their exact names; run_command() only presses enter when the palette highlights exactly that command for the typed query.
Every check of what a workspace shows waits for the complete expected view (folders, panes, split, tabs, cursor, unsaved text, drawn tabs, project panel) and names each value that differs.
The test_stock_* tests check the stock Zed steps the criteria use (Zed's path picker, the project panel, the save prompts and their answers by keys), so they pass before the story is built.
test_stock_zed_asks_to_save_when_a_window_closes records the prompt stock Zed shows when a window with an unsaved file closes; AC6 expects the same prompt.
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
TOGGLE_PROJECT_PANEL_FOCUS = "project panel: toggle focus"
CLOSE_ALL_DOCKS = "workspace: close all docks"
NEW_WINDOW = "workspace: new window"
NEW_FILE = "workspace: new file"
CLOSE_WINDOW = "workspace: close window"

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding"
DOCS_NAME = "Docs: release notes"

# How long a workspace command, a file open or a restore may take to show.
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# How long a cancelled change must stay away before the test believes it.
SETTLE_TIME = 1.5

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


if __name__ == "__main__":
    unittest.main()
