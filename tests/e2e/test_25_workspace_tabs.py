"""Story #25: the workspaces as quiet tabs in the title bar, each with a status dot only when something is happening, and a "+" that opens a new workspace. One test per acceptance criterion.

Fixtures are the throwaway git repo billing-api with invented content and three invented workspace names.
The workspaces are made and named with the commands of #24 from the command palette. Tabs and "+" are clicked at the positions the driver reports for the last drawn frame.
Session states come from the driver's set_entry test fixture (tests/e2e/cod_status_fixtures.py), which stands in for the scripted fake agent provider of #28 until it lands.
"""

from __future__ import annotations

import os
import time
import unittest
from collections.abc import Callable
from typing import Any

from cod_e2e import CodTestCase
from cod_status_fixtures import (
    REMOVED_ITEMS,
    TAB_MAX_WIDTH,
    CodStatusFixtures,
    active_tabs,
    drawn,
    tab_dots,
    tab_names,
    title_bar,
)

NEW_WORKSPACE = "workspace: new workspace"
RENAME_WORKSPACE = "workspace: rename workspace"
NEXT_WORKSPACE = "workspace: next workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding on legal-entity invoices"
DOC_NAME = "[DOC-208] (docs-site) Move the API section"
OPS_NAME = "[OPS-77] (infra) Upgrade Postgres to 17"
NAMES = [PAY_NAME, DOC_NAME, OPS_NAME]

# The name of a tab whose workspace has no name and no folder.
UNNAMED = "New workspace"

# How long a workspace command or a click may take to show.
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# #25 AC4: when the last session leaves a state, the dot changes within one second.
DOT_TIMEOUT = 1.0
# How long a state that must not change is watched.
SETTLE_TIME = 1.5
# Review #25/#26: how long a workspace other than the one shown after a restart may take to
# load lazily the first time it is shown (about 1-3 s in CI); watched long enough that an
# older load's completion switching back to it would be caught.
PENDING_LOAD_SETTLE = 6.0

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# billing-api\n\nAn invented service that issues invoices.\n",
    "src/main.rs": 'fn main() {\n    println!("billing-api");\n}\n',
}


def real(path: Any) -> str:
    return os.path.realpath(path)


def tabs_view(state: dict[str, Any]) -> dict[str, Any]:
    """What the title bar tabs show: names, which one is active, dots, and the active workspace of the window."""
    return {
        "names": tab_names(state),
        "active": active_tabs(state),
        "dots": tab_dots(state),
        "active_workspace": state["active_workspace"],
        "workspace_count": state["workspace_count"],
        "modal": state["modal"],
    }


def one_hot(count: int, index: int) -> list[bool]:
    return [position == index for position in range(count)]


class WorkspaceTabsTest(CodStatusFixtures, CodTestCase):
    # Actions

    def wait_for_tabs(self, what: str, expected: dict[str, Any], timeout: float = SHOW_TIMEOUT) -> dict[str, Any]:
        return self.wait_for_values(what, expected, tabs_view, timeout)

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

    def rename_workspace(self, name: str) -> None:
        """Names the active workspace with "workspace: rename workspace" (#24)."""
        self.run_command(RENAME_WORKSPACE)
        self.wait_for(lambda state: state["rename_input"] is not None, SHOW_TIMEOUT, "the rename field to show")
        self.type(name)
        self.wait_for(
            lambda state: (state["rename_input"] or {}).get("text") == name, SHOW_TIMEOUT, "the rename field to hold the name"
        )
        self.keys("enter")
        self.wait_for(
            lambda state: state["rename_input"] is None
            and state["modal"] is None
            and state["workspaces"][state["active_workspace"]]["name"] == name,
            SHOW_TIMEOUT,
            f"the workspace to be named {name!r}",
        )

    def new_workspace(self) -> None:
        before = self.state()
        self.run_command(NEW_WORKSPACE)
        self.wait_for(
            lambda state: state["workspace_count"] == before["workspace_count"] + 1
            and state["active_workspace"] == before["workspace_count"]
            and state["modal"] is None,
            SHOW_TIMEOUT,
            "the new workspace to be active at the end",
        )

    def start_with_three_named_workspaces(self, persistent: bool = False) -> None:
        """Starts Cod on billing-api, checks the first tab, then names it and makes and names two more workspaces. OPS-77 (the last) is active."""
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api, persistent=persistent)
            self.wait_for(lambda state: [real(path) for path in state["worktrees"]] == [real(billing_api)], SHOW_TIMEOUT, "billing-api")

        with self.step("check the starting state: one unnamed workspace, one active tab named after its folder"):
            self.wait_for_tabs(
                "the first tab",
                {"names": ["billing-api"], "active": [True], "dots": [None], "active_workspace": 0, "workspace_count": 1},
            )

        with self.step("name the first workspace PAY-1234"):
            self.rename_workspace(PAY_NAME)

        with self.step("open a second workspace: its tab is active and named New workspace"):
            self.new_workspace()
            self.wait_for_tabs("the tabs after the second workspace", {"names": [PAY_NAME, UNNAMED], "active": [False, True]})

        with self.step("name the second workspace DOC-208"):
            self.rename_workspace(DOC_NAME)

        with self.step("open and name the third workspace OPS-77"):
            self.new_workspace()
            self.rename_workspace(OPS_NAME)

        with self.step("check the three tabs in creation order, OPS-77 active, no dots"):
            self.wait_for_tabs(
                "the three tabs",
                {"names": NAMES, "active": [False, False, True], "dots": [None, None, None], "active_workspace": 2, "workspace_count": 3},
            )

    # AC1

    def test_ac1_one_tab_per_workspace_in_creation_order_then_plus(self) -> None:
        self.start_with_three_named_workspaces()

        with self.step("check each tab: full name as tooltip, drawn in one row in order, then +"):
            state = self.state()
            bar = title_bar(state)
            tabs = bar["tabs"]
            self.expect_equal("tooltips", NAMES, [tab["tooltip"] for tab in tabs])
            for index, tab in enumerate(tabs):
                if not drawn(tab["bounds"]):
                    raise AssertionError(f"expected tab {index} to be drawn, actual bounds {tab['bounds']!r}")
            tops = {round(tab["bounds"]["y"]) for tab in tabs}
            self.expect_equal("number of rows of tabs", 1, len(tops))
            lefts = [tab["bounds"]["x"] for tab in tabs]
            self.expect_equal("tabs left to right in creation order", sorted(lefts), lefts)
            add = bar["add_button"]["bounds"]
            if not drawn(add):
                raise AssertionError(f"expected the + button to be drawn, actual bounds {add!r}")
            last = tabs[-1]["bounds"]
            if add["x"] < last["x"] + last["width"] - 1:
                raise AssertionError(f"expected + right of the last tab {last!r}, actual {add!r}")

        with self.step("check that a name longer than the maximum width is cut and no tab is wider"):
            self.expect_equal("PAY-1234 tab cut", True, tabs[0]["truncated"])
            for index, tab in enumerate(tabs):
                if tab["bounds"]["width"] > TAB_MAX_WIDTH + 0.5:
                    raise AssertionError(f"expected tab {index} at most {TAB_MAX_WIDTH} px wide, actual {tab['bounds']['width']}")

        with self.step("check that one line of text fits each tab: every tab is as high as the title bar row"):
            heights = {round(tab["bounds"]["height"]) for tab in tabs}
            self.expect_equal("number of tab heights", 1, len(heights))

    # AC2

    def test_ac2_title_bar_holds_only_tabs_and_plus(self) -> None:
        with self.step("create the fixture repo billing-api (a git repo, so stock Zed would show its branch)"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api"):
            self.start_cod(billing_api)
            state = self.wait_for_tabs("the first tab", {"names": ["billing-api"], "active": [True]})

        with self.step("check that the project name, branch picker, sign-in button and user menu are not in the title bar"):
            items = title_bar(state)["items"]
            shown = [item for item in REMOVED_ITEMS if item in items]
            self.expect_equal("removed Zed items in the title bar", [], shown)
            if not drawn(title_bar(state)["add_button"]["bounds"]):
                raise AssertionError("expected the + button to be drawn")

    # AC3

    def test_ac3_idle_workspaces_have_no_dot(self) -> None:
        self.start_with_three_named_workspaces()

        with self.step("give DOC-208 two idle sessions and OPS-77 one idle page"):
            self.set_entry(1, "doc-session-1", "idle", title="Claude: outline the API section")
            self.set_entry(1, "doc-session-2", "idle", title="Codex: check links")
            self.set_entry(2, "ops-page-1", "idle", title="Postgres 17 release notes", kind="page", context="postgres.example.org/docs/17")

        with self.step("check that no tab shows a dot and every workspace is idle"):
            self.stays(
                lambda state: tab_dots(state) == [None, None, None]
                and [workspace["status"] for workspace in state["workspaces"]] == ["idle", "idle", "idle"],
                SETTLE_TIME,
                "no dot on idle workspaces",
            )

    # AC4

    def test_ac4_the_dot_shows_the_most_urgent_state(self) -> None:
        self.start_with_three_named_workspaces()

        with self.step("a done session on PAY-1234: green"):
            self.set_entry(0, "pay-tests", "done", title="Claude: add tax tests", context="done, 6 tests added")
            self.wait_for_tabs("dots", {"dots": ["green", None, None]}, DOT_TIMEOUT)

        with self.step("add a working session: blue, pulsing"):
            self.set_entry(0, "pay-review", "working", title="Codex: review migration")
            state = self.wait_for_tabs("dots", {"dots": ["blue", None, None]}, DOT_TIMEOUT)
            self.expect_equal("PAY-1234 dot pulsing", True, title_bar(state)["tabs"][0]["pulsing"])

        with self.step("add a failed session: red"):
            self.set_entry(0, "pay-plan", "error", title="Codex: plan the rollout")
            self.wait_for_tabs("dots", {"dots": ["red", None, None]}, DOT_TIMEOUT)

        with self.step("add a session waiting for input: yellow"):
            self.set_entry(0, "pay-rounding", "needs_input", title="Claude: fix invoice rounding", context="waiting for you")
            state = self.wait_for_tabs("dots", {"dots": ["yellow", None, None]}, DOT_TIMEOUT)
            self.expect_equal("PAY-1234 status", "needs_input", state["workspaces"][0]["status"])
            self.expect_equal("PAY-1234 dot pulsing", False, title_bar(state)["tabs"][0]["pulsing"])

        with self.step("the waiting session goes idle: red within one second"):
            self.set_entry(0, "pay-rounding", "idle", title="Claude: fix invoice rounding")
            self.wait_for_tabs("dots", {"dots": ["red", None, None]}, DOT_TIMEOUT)

        with self.step("the failed session goes idle: blue within one second"):
            self.set_entry(0, "pay-plan", "idle", title="Codex: plan the rollout")
            self.wait_for_tabs("dots", {"dots": ["blue", None, None]}, DOT_TIMEOUT)

        with self.step("the working session goes idle: green within one second, and the green dot stays"):
            self.set_entry(0, "pay-review", "idle", title="Codex: review migration")
            self.wait_for_tabs("dots", {"dots": ["green", None, None]}, DOT_TIMEOUT)
            self.stays(lambda state: tab_dots(state) == ["green", None, None], SETTLE_TIME, "the green dot of the done session")

        with self.step("the done session goes idle: no dot within one second"):
            self.set_entry(0, "pay-tests", "idle", title="Claude: add tax tests")
            self.wait_for_tabs("dots", {"dots": [None, None, None]}, DOT_TIMEOUT)

    def test_ac4_ac5_a_workspace_not_loaded_after_a_restart_shows_its_dot_and_opens_on_click(self) -> None:
        self.start_with_three_named_workspaces(persistent=True)

        with self.step("quit with ctrl-q and start again"):
            self.restart_cod()

        with self.step("check that the three tabs came back with OPS-77 active and PAY-1234 not drawn"):
            state = self.wait_for_tabs(
                "the restored tabs", {"names": NAMES, "active": [False, False, True], "active_workspace": 2}, RESTORE_TIMEOUT
            )
            self.expect_equal("PAY-1234 drawn", False, state["workspaces"][0]["drawn"])

        with self.step("a session of PAY-1234 waits for input: its tab shows yellow"):
            self.set_entry(0, "pay-rounding", "needs_input", title="Claude: fix invoice rounding")
            state = self.wait_for_tabs("dots", {"dots": ["yellow", None, None], "active_workspace": 2}, DOT_TIMEOUT)

        with self.step("click the PAY-1234 tab: it loads and becomes active"):
            self.click_tab(state, 0)
            self.wait_for_tabs("the clicked tab", {"active": [True, False, False], "active_workspace": 0, "dots": ["yellow", None, None]})

    # AC5

    def test_ac5_newest_click_wins_over_a_still_loading_tabs_completion(self) -> None:
        """Review finding: clicking a tab that is still loading, then clicking an already loaded
        tab before the first one finishes, must leave the second tab active. The first
        workspace keeps loading in the background, but its completion must not switch the
        window back to it: the newest click decides."""
        self.start_with_three_named_workspaces(persistent=True)

        with self.step("quit with ctrl-q and start again: OPS-77 stays active, PAY-1234 and DOC-208 are pending"):
            self.restart_cod()
            state = self.wait_for_tabs(
                "the restored tabs", {"names": NAMES, "active": [False, False, True], "active_workspace": 2}, RESTORE_TIMEOUT
            )
            self.expect_equal("PAY-1234 not loaded yet", False, state["workspaces"][0]["drawn"])

        with self.step("click PAY-1234 (still loading), then click OPS-77 (already loaded) before it finishes"):
            self.click_tab(state, 0)
            self.click_tab(state, 2)
            self.wait_for_tabs(
                "OPS-77 active right after the second click", {"active": [False, False, True], "active_workspace": 2}
            )

        with self.step("OPS-77 stays active for as long as PAY-1234 may still be loading in the background"):
            self.stays(
                lambda state: state["active_workspace"] == 2 and active_tabs(state) == [False, False, True],
                PENDING_LOAD_SETTLE,
                "OPS-77 active while PAY-1234 finishes loading in the background",
            )

        with self.step("PAY-1234 did load in the background: clicking it now shows it"):
            self.click_tab(self.state(), 0)
            self.wait_for_tabs("PAY-1234 after its own click", {"active": [True, False, False], "active_workspace": 0})

    def test_ac5_clicking_a_tab_shows_it_and_plus_opens_a_new_workspace(self) -> None:
        self.start_with_three_named_workspaces()

        with self.step("click the PAY-1234 tab"):
            self.click_tab(self.state(), 0)
            self.wait_for_tabs("the clicked tab", {"active": [True, False, False], "active_workspace": 0})

        with self.step("click the DOC-208 tab"):
            self.click_tab(self.state(), 1)
            self.wait_for_tabs("the clicked tab", {"active": [False, True, False], "active_workspace": 1})

        with self.step("click +: a new workspace opens at the end and its tab is active"):
            self.click_add_button(self.state())
            self.wait_for_tabs(
                "the new tab",
                {"names": [*NAMES, UNNAMED], "active": one_hot(4, 3), "active_workspace": 3, "workspace_count": 4},
            )

    # AC6

    def test_ac6_next_and_previous_workspace_move_the_active_tab_and_wrap(self) -> None:
        self.start_with_three_named_workspaces()

        with self.step("next workspace from the last tab wraps to the first"):
            self.run_command(NEXT_WORKSPACE)
            self.wait_for_tabs("next", {"active": one_hot(3, 0), "active_workspace": 0, "modal": None})

        with self.step("next workspace moves right"):
            self.run_command(NEXT_WORKSPACE)
            self.wait_for_tabs("next", {"active": one_hot(3, 1), "active_workspace": 1, "modal": None})

        with self.step("previous workspace moves left"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for_tabs("previous", {"active": one_hot(3, 0), "active_workspace": 0, "modal": None})

        with self.step("previous workspace from the first tab wraps to the last"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for_tabs("previous", {"active": one_hot(3, 2), "active_workspace": 2, "modal": None})


if __name__ == "__main__":
    unittest.main()
