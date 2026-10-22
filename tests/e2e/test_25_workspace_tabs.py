"""Story #25: the workspaces as quiet tabs in the title bar, each with a status dot only when something is happening, and a "+" that opens a new workspace. One test per acceptance criterion.

Fixtures are the throwaway git repo billing-api with invented content and three invented workspace names.
The workspaces are made and named with the commands of #24 from the command palette. Tabs and "+" are clicked at the positions the driver reports for the last drawn frame.
Session states come from the driver's set_entry test fixture (tests/e2e/cod_status_fixtures.py), which stands in for the scripted fake agent provider of #28 until it lands.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

from cod_e2e import COMMAND_TIMEOUT, CodTestCase, processes_with_marker, sway_socket
from cod_status_fixtures import (
    REMOVED_ITEMS,
    TAB_MAX_WIDTH,
    CodStatusFixtures,
    active_tabs,
    bounds_pixel,
    center,
    color_distance,
    drawn,
    hex_color,
    ink_columns,
    ink_runs,
    logical_pixel,
    output_scale,
    read_ppm,
    rows_center,
    screenshot_ppm,
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

# The name of a tab whose workspace has no name and no folder: the first one of these shown is
# just "New workspace"; the second and later ones are numbered "New workspace 2", "New workspace
# 3", ... in tab order, so a user can tell freshly-opened workspaces apart (#77).
UNNAMED = "New workspace"
UNNAMED_2 = "New workspace 2"
UNNAMED_3 = "New workspace 3"

# Issue #77 (follow-up of #25): the stakeholder's first-use screenshot was 1280x800, the default
# dark theme, sway scale 1. Run the width-sensitive tests below with COD_E2E_OUTPUT_MODE=1280x800
# (script/e2e sets sway's output to that size only for this run); at script/e2e's own default
# (1440x900) the title bar has even more free room, so the bug (tabs cut far under their own max
# width while the bar sits mostly empty) reproduces there too, but 1280 is the exact width the
# report measured. This reproduction's own fixture environment (headless sway, no desktop portal)
# settled on a light-looking title bar rather than the stakeholder's dark one (see AC(c) below);
# the truncation and layout bugs reproduce the same regardless.
# #77 C5: derived from COD_E2E_OUTPUT_MODE, not hardcoded, so the scale-2 tests sample the
# right pixel coordinates too.
FIRST_USE_OUTPUT_SCALE = output_scale()

# Issue #77 AC(c): colors are checked relative to each other in the same screenshot, not against
# a hardcoded theme hex. A headless sway with no desktop portal reports no system appearance, and
# this reproduction measured the fixture's own default settling on a light-looking title bar
# (#ebebec) rather than the stakeholder's dark one, so a hardcoded absolute color would be
# fragile; the actual defect (a border color unrelated to the tab's own fill, measured exactly as
# the theme's plain `border` token: One Light #c9c9ca next to a #fafafa fill) does not depend on
# which theme is active, light or dark.
# Generous: real compositing (pixman software rendering, PPM round-tripping) is not bit-exact.
COLOR_TOLERANCE = 12.0

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


def sway_ipc(*args: str) -> Any:
    """Runs swaymsg against the private sway of this run (script/e2e) and returns its JSON reply."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    sway = sway_socket(Path(runtime)) if runtime else None
    if sway is None:
        raise AssertionError(f"no sway IPC socket in XDG_RUNTIME_DIR={runtime!r}: run the end-to-end tests with script/e2e")
    reply = subprocess.run(["swaymsg", "-s", str(sway), "-r", *args], capture_output=True, text=True, timeout=COMMAND_TIMEOUT)
    if reply.returncode != 0:
        raise AssertionError(f"swaymsg {' '.join(args)} failed with status {reply.returncode}: {reply.stdout.strip()} {reply.stderr.strip()}")
    return json.loads(reply.stdout)


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

    def codctl(self, *args: str) -> subprocess.CompletedProcess[str]:
        """Runs the shipped codctl against the running Cod, the way an agent does (issue #77 C1)."""
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        env = dict(os.environ)
        for name in ("COD_WORKSPACE", "COD_SOCKET"):
            env.pop(name, None)
        env["HOME"] = str(self.cod.home)
        return subprocess.run(
            [str(Path(run_dir) / "codctl"), *args],
            env=env,
            capture_output=True,
            text=True,
            timeout=30.0,
            stdin=subprocess.DEVNULL,
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

    # Issue #77 AC(a): two default workspaces, neither tab cut, both full names drawn.

    def test_ac7_two_default_workspaces_are_not_cut(self) -> None:
        """The stakeholder's screenshot (1280x800): two "New workspace" tabs both read "New work..."
        although about 1000 px of the 1280 px title bar was empty. Run with
        COD_E2E_OUTPUT_MODE=1280x800 for the exact reported width. Also #77's numbering: a user
        must be able to tell freshly-opened workspaces apart, so the second and third unnamed,
        folderless workspaces are "New workspace 2" and "New workspace 3", not all "New
        workspace"; the title bar, the overview (#26) and the tooltip must all agree."""
        with self.step("start Cod with a fresh profile (no folder): one default workspace"):
            self.start_cod(None)
            state = self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})

        with self.step("open a second workspace with +: distinct default names, neither tab cut"):
            self.click_add_button(state)
            state = self.wait_for_tabs("the second tab", {"names": [UNNAMED, UNNAMED_2], "active": [False, True]})
            tabs = title_bar(state)["tabs"]
            for index, tab in enumerate(tabs):
                self.expect_equal(f"tab {index} truncated", False, tab["truncated"])
                self.expect_equal(f"tab {index} tooltip matches its name", tab["name"], tab["tooltip"])

        with self.step("open a third workspace with +: three distinct default names, none cut"):
            self.click_add_button(state)
            state = self.wait_for_tabs(
                "the third tab", {"names": [UNNAMED, UNNAMED_2, UNNAMED_3], "active": [False, False, True]}
            )
            tabs = title_bar(state)["tabs"]
            for index, tab in enumerate(tabs):
                self.expect_equal(f"tab {index} truncated", False, tab["truncated"])

        with self.step("the overview (#26) lists the same three distinct names"):
            self.keys("ctrl-alt-o")
            overview_state = self.wait_for(
                lambda state: state["overview"] is not None and len(state["overview"]["rows"]) > 0, SHOW_TIMEOUT, "the overview"
            )
            workspace_rows = [row["title"] for row in overview_state["overview"]["rows"] if row["kind"] == "workspace"]
            self.expect_equal("overview workspace names", sorted([UNNAMED, UNNAMED_2, UNNAMED_3]), sorted(workspace_rows))
            self.keys("escape")

        with self.step("renaming the first workspace takes it out of the numbering; the remaining two renumber"):
            self.wait_for(lambda state: state["overview"] is None, SHOW_TIMEOUT, "the overview to close")
            self.click_tab(self.state(), 0)
            self.wait_for_tabs("the first tab active", {"active": [True, False, False]})
            self.rename_workspace("Renamed")
            # Numbering is recomputed from the current members, not stored on each workspace, so
            # with one fewer unnamed workspace the remaining two are "New workspace" and "New
            # workspace 2" now, not "New workspace 2" and "New workspace 3" (#77, a stated rough
            # edge: renumbering when an earlier one leaves the unnamed set is acceptable).
            state = self.wait_for_tabs("renamed, the remaining two renumbered", {"names": ["Renamed", UNNAMED, UNNAMED_2]})

        with self.step("a new fourth workspace becomes the next unnamed rank"):
            self.click_add_button(state)
            self.wait_for_tabs(
                "the fourth tab", {"names": ["Renamed", UNNAMED, UNNAMED_2, UNNAMED_3], "active": one_hot(4, 3)}
            )

    # Issue #77 AC(b): a long name ellipsizes only as needed and stays distinguishable.

    def test_ac7_long_names_use_their_full_max_width_and_stay_distinguishable(self) -> None:
        """Two long names without an "[ID]" prefix, differing only near the end (the part ellipsis
        would cut first): if #77's layout bug squeezes both tabs far under their own 280 px max
        width, they lose that distinguishing tail and look identical. 1280 px comfortably fits two
        280 px tabs (560 px), so both should reach the max width, not something much smaller."""
        long_a = "Fix invoice rounding for every annual plan in the EU region"
        long_b = "Fix invoice rounding for every annual plan in the US region"

        with self.step("start Cod, name the first workspace, add a second with another long name"):
            self.start_cod(None)
            self.rename_workspace(long_a)
            self.new_workspace()
            self.rename_workspace(long_b)
            state = self.wait_for_tabs("two long-named tabs", {"names": [long_a, long_b], "active": [False, True]})

        with self.step("check the tooltips carry the full names"):
            tabs = title_bar(state)["tabs"]
            self.expect_equal("tooltips", [long_a, long_b], [tab["tooltip"] for tab in tabs])

        with self.step(f"check neither tab is squeezed under its own {TAB_MAX_WIDTH} px max width"):
            for index, tab in enumerate(tabs):
                width = tab["bounds"]["width"]
                if width < TAB_MAX_WIDTH - 1:
                    raise AssertionError(
                        f"expected tab {index} at its max width {TAB_MAX_WIDTH} px (two long tabs fit the "
                        f"title bar), actual {width} px; a narrower tab has less of its name showing and "
                        "both tabs can look the same"
                    )

    # Issue #77 AC(c): tab colors match chosen ghost tokens, with no extra border color.

    def test_ac7_tab_colors_match_the_ghost_tokens_with_no_extra_border(self) -> None:
        """Colors are compared to each other in the same screenshot (see the constants above): an
        inactive tab should look like the title bar itself, a hovered tab should pick up a visible
        highlight, the active tab should stand out from the bar, and no tab should carry a border
        in a color different from its own fill. Samples are taken near the top of each tab, away
        from the label's own glyphs (sampling mid-height landed on anti-aliased text pixels, not
        the background, in this reproduction)."""
        with self.step("start Cod: one default workspace, add a second (first stays inactive)"):
            self.start_cod(None)
            state = self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})
            self.click_add_button(state)
            state = self.wait_for_tabs("the second tab", {"names": [UNNAMED, UNNAMED_2], "active": [False, True]})

        shot = self.workdir / "colors.ppm"
        with self.step("screenshot the title bar, the inactive tab and the active tab"):
            screenshot_ppm(shot)
            width, height, pixels = read_ppm(shot)
            tabs = title_bar(state)["tabs"]
            add_button = title_bar(state)["add_button"]["bounds"]
            bar_y = tabs[0]["bounds"]["y"] + 2
            # Clearly right of the "+" button: the title bar's own background, nothing else drawn there.
            bar_bg = logical_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, add_button["x"] + add_button["width"] + 40, bar_y)
            inactive_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[0]["bounds"], dy=0.15)
            active_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[1]["bounds"], dy=0.15)
            # One logical pixel inside the left edge, where border_x_1 draws a border if the tab has one.
            active_edge = logical_pixel(
                width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[1]["bounds"]["x"], tabs[1]["bounds"]["y"] + 2
            )

        with self.step("check the inactive tab looks like the title bar itself"):
            if color_distance(inactive_bg, bar_bg) > COLOR_TOLERANCE:
                raise AssertionError(
                    f"expected the inactive tab close to the title bar {hex_color(bar_bg)}, actual {hex_color(inactive_bg)}"
                )

        with self.step("check the active tab stands out from the title bar (a selected look)"):
            if color_distance(active_bg, bar_bg) <= COLOR_TOLERANCE:
                raise AssertionError(
                    f"expected the active tab visibly different from the title bar {hex_color(bar_bg)}, "
                    f"actual the same {hex_color(active_bg)}"
                )

        with self.step("check the active tab has no separate border color"):
            if color_distance(active_edge, active_bg) > COLOR_TOLERANCE:
                raise AssertionError(
                    f"expected the active tab's edge the same color as its own fill {hex_color(active_bg)}, "
                    f"actual {hex_color(active_edge)} (a border in a different grey)"
                )

        with self.step("hover the inactive tab and check it now shows a visible highlight"):
            self.hover_tab(state, 0)
            screenshot_ppm(shot)
            width, height, pixels = read_ppm(shot)
            hover_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[0]["bounds"], dy=0.15)
            if color_distance(hover_bg, bar_bg) <= COLOR_TOLERANCE:
                raise AssertionError(
                    f"expected the hovered tab visibly different from the title bar {hex_color(bar_bg)}, "
                    f"actual the same {hex_color(hover_bg)}"
                )

    # Issue #77 AC(d)/F4: tab content (label, dot, close button) and "+" are vertically centered
    # within 1 px of the title bar's own center.

    def test_ac7_tab_content_is_vertically_centered(self) -> None:
        with self.step("start Cod and give the first (only) workspace a visible dot"):
            self.start_cod(None)
            state = self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})
            self.set_entry(0, "s1", "working", title="Codex: review migration")
            state = self.wait_for_tabs("the dot", {"dots": ["blue"]})

        shot = self.workdir / "align.ppm"
        with self.step("check the drawn dot, label, close icon and + icon each sit within 1 px of the tab's vertical center and of each other"):
            screenshot_ppm(shot)
            width, height, pixels = read_ppm(shot)
            scale = FIRST_USE_OUTPUT_SCALE
            bar = title_bar(state)
            tab = bar["tabs"][0]
            # The label is measured by its cap-height band (topmost ink row to the baseline), which a descender such as the "p" of "workspace" does not move; that needs the known default name.
            self.expect_equal("tab name", UNNAMED, tab["name"])
            bounds = tab["bounds"]
            tab_center = bounds["y"] + bounds["height"] / 2
            close_left = round(tab["close_bounds"]["x"] * scale)
            content = {x: rows for x, rows in ink_columns(width, height, pixels, scale, bounds).items() if x < close_left}
            runs = ink_runs(content, gap=round(3 * scale))
            if len(runs) < 2:
                raise AssertionError(f"expected a dot and then a label in the tab, actual ink runs {runs!r}")
            dot_first, dot_last = runs[0]
            dot = [content[x] for x in range(dot_first, dot_last + 1) if x in content]
            dot_top, dot_bottom = min(top for top, _ in dot), max(bottom for _, bottom in dot)
            dot_width, dot_height = (dot_last - dot_first + 1) / scale, (dot_bottom - dot_top + 1) / scale
            if dot_width > 10 or abs(dot_width - dot_height) > 2:
                raise AssertionError(f"expected the first ink in the tab to be its small round dot, actual {dot_width:.1f}x{dot_height:.1f} px")
            label = [rows for x, rows in content.items() if x > dot_last]
            cap_top = min(top for top, _ in label)
            bottoms = [bottom for _, bottom in label]
            baseline = max(set(bottoms), key=bottoms.count)
            if (baseline - cap_top + 1) / scale < 6:
                raise AssertionError(f"expected the label's cap-height band to be at least 6 px tall, actual rows {cap_top}-{baseline}")
            close_ink = list(ink_columns(width, height, pixels, scale, tab["close_bounds"]).values())
            if not close_ink:
                raise AssertionError("expected the active tab's close icon drawn")
            add_bounds = bar["add_button"]["bounds"]
            add_columns = ink_columns(width, height, pixels, scale, add_bounds)
            add_middle = (add_bounds["x"] + add_bounds["width"] / 2) * scale
            plus_first, plus_last = min(ink_runs(add_columns, gap=round(2 * scale)), key=lambda run: abs((run[0] + run[1]) / 2 - add_middle))
            plus = [add_columns[x] for x in range(plus_first, plus_last + 1) if x in add_columns]
            centers = {
                "dot": rows_center(dot_top, dot_bottom, scale),
                "label cap-height band": rows_center(cap_top, baseline, scale),
                "close icon": rows_center(min(top for top, _ in close_ink), max(bottom for _, bottom in close_ink), scale),
                "+ icon": rows_center(min(top for top, _ in plus), max(bottom for _, bottom in plus), scale),
            }
            print(f"  tab center {tab_center:.1f}, drawn centers {centers}")
            for element, element_center in centers.items():
                if abs(element_center - tab_center) > 1.0:
                    raise AssertionError(f"expected the {element} centered within 1 px of the tab's {tab_center:.1f}, actual {element_center:.1f}")
            if max(centers.values()) - min(centers.values()) > 1.0:
                raise AssertionError(f"expected the dot, label, close icon and + icon on one line within 1 px, actual centers {centers!r}")

        with self.step("check the close button and the + button centers against the title bar's own center"):
            bar_center = bounds["y"] + bounds["height"] / 2  # the title bar's content row is as tall as a tab
            close_bounds = bar["tabs"][0]["close_bounds"]
            if not drawn(close_bounds):
                raise AssertionError(f"expected a close button drawn on the active tab, actual {close_bounds!r}")
            close_center = close_bounds["y"] + close_bounds["height"] / 2
            if abs(close_center - bar_center) > 1.0:
                raise AssertionError(f"expected the close button centered within 1 px of {bar_center:.1f}, actual {close_center:.1f}")
            add_bounds = bar["add_button"]["bounds"]
            add_center = add_bounds["y"] + add_bounds["height"] / 2
            if abs(add_center - bar_center) > 1.0:
                raise AssertionError(f'expected the "+" button centered within 1 px of {bar_center:.1f}, actual {add_center:.1f}')

    # Issue #77 AC(e)/F3: a hovered or active tab shows a close control that closes that workspace.

    def test_ac7_close_button_shows_on_active_and_hover_and_closes_the_tab(self) -> None:
        with self.step("start Cod: one default workspace, add a second"):
            self.start_cod(None)
            state = self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})
            self.click_add_button(state)
            state = self.wait_for_tabs(
                "the second tab", {"names": [UNNAMED, UNNAMED_2], "active": [False, True], "workspace_count": 2}
            )

        with self.step("both loaded tabs have a close button (its space is always reserved)"):
            tabs = title_bar(state)["tabs"]
            for index, tab in enumerate(tabs):
                if not drawn(tab["close_bounds"]):
                    raise AssertionError(f"expected tab {index} to have a close button, actual {tab['close_bounds']!r}")

        shot = self.workdir / "close.ppm"
        with self.step("the active tab's close icon is visible without hovering; the inactive one is not"):
            screenshot_ppm(shot)
            width, height, pixels = read_ppm(shot)
            active_close = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[1]["close_bounds"])
            active_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[1]["bounds"], dy=0.15)
            if color_distance(active_close, active_bg) <= COLOR_TOLERANCE:
                raise AssertionError("expected the active tab's close icon visible (a color different from its own background)")
            inactive_close = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[0]["close_bounds"])
            inactive_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, tabs[0]["bounds"], dy=0.15)
            if color_distance(inactive_close, inactive_bg) > COLOR_TOLERANCE:
                raise AssertionError("expected the inactive tab's close icon not visible before hovering it")

        with self.step("hover the inactive tab: its close icon becomes visible, its own width does not change"):
            width_before = tabs[0]["bounds"]["width"]
            self.hover_tab(state, 0)
            state = self.state()
            hovered_bounds = title_bar(state)["tabs"][0]["bounds"]
            self.expect_equal("hovered tab width unchanged", width_before, hovered_bounds["width"])
            screenshot_ppm(shot)
            width, height, pixels = read_ppm(shot)
            hovered_close = bounds_pixel(
                width, height, pixels, FIRST_USE_OUTPUT_SCALE, title_bar(state)["tabs"][0]["close_bounds"]
            )
            hovered_bg = bounds_pixel(width, height, pixels, FIRST_USE_OUTPUT_SCALE, hovered_bounds, dy=0.15)
            if color_distance(hovered_close, hovered_bg) <= COLOR_TOLERANCE:
                raise AssertionError("expected the hovered tab's close icon visible")

        with self.step("click the hovered tab's close button: it closes, the other workspace stays active"):
            # #77 C5: asserted by the surviving workspace's own id, not just its name or
            # position, now that the two have distinct names (the numbering) too.
            surviving_id = state["workspaces"][1]["id"]
            self.click_close_button(state, 0)
            state = self.wait_for_tabs(
                "one workspace left, the previously active one still active",
                {"names": [UNNAMED], "active": [True], "workspace_count": 1},
            )
            self.expect_equal("the surviving workspace is the one that was active, by id", surviving_id, state["workspaces"][0]["id"])

    def test_ac7_a_held_close_press_cannot_migrate_to_another_workspace(self) -> None:
        """#77 C1: both the tab and its close button are keyed by the workspace's own identity,
        not its index, so a held press cannot end up acting on whichever workspace now sits at
        that position. Press B's close button, remove A through codctl before releasing, then
        release: C (checked by id) must never be the one that closes, whatever happens to B."""
        with self.step("start Cod: three workspaces A, B, C, C active"):
            self.start_cod(None)
            self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})
            self.rename_workspace("A")
            self.new_workspace()
            self.rename_workspace("B")
            self.new_workspace()
            self.rename_workspace("C")
            state = self.wait_for_tabs("three named tabs", {"names": ["A", "B", "C"], "active": [False, False, True]})

        ids = {workspace["name"]: workspace["id"] for workspace in state["workspaces"]}
        c_id = ids["C"]

        with self.step("press B's close button, remove A through codctl before releasing"):
            self.mouse_down_on_close_button(state, 1)
            result = self.codctl("workspace", "close", "--workspace", "A", "--force", "--json")
            if result.returncode != 0:
                raise AssertionError(f"codctl workspace close A failed: exit {result.returncode}, {result.stdout!r} {result.stderr!r}")
            self.mouse_up_at(*center(title_bar(state)["tabs"][1]["close_bounds"]))

        with self.step("C is never the one that closed, whether or not B also did"):
            state = self.wait_for(lambda state: state["workspace_count"] <= 2, SHOW_TIMEOUT, "A gone (and maybe B)")
            remaining_ids = [workspace["id"] for workspace in state["workspaces"]]
            if c_id not in remaining_ids:
                raise AssertionError(f"expected C (id {c_id}) to survive, actual surviving workspace ids {remaining_ids}")

    def test_ac7_close_button_shows_the_unsaved_confirmation_and_keep_keeps_it(self) -> None:
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api, add a second default workspace"):
            self.start_cod(billing_api)
            state = self.wait_for_tabs("the first tab", {"names": ["billing-api"], "active": [True]})
            self.click_add_button(state)
            state = self.wait_for_tabs("the second tab", {"names": ["billing-api", UNNAMED], "active": [False, True]})

        with self.step("dirty a buffer in the first (now inactive) workspace"):
            self.click_tab(state, 0)
            self.wait_for_tabs("the first tab active", {"active": [True, False]})
            self.keys("ctrl-p")
            self.wait_for(lambda state: state["modal"] == "file_finder", SHOW_TIMEOUT, "the file finder to open")
            self.wait_for(lambda state: state["file_finder_ready"], SHOW_TIMEOUT, "the file finder to be drawn with the focus")
            self.type("main.rs")
            self.keys("enter")
            self.wait_for(
                lambda state: state["modal"] is None and state["active_tab"] == "main.rs", SHOW_TIMEOUT, "main.rs to open"
            )
            self.type("// draft ")
            self.wait_for(lambda state: state["dirty_tabs"] == ["main.rs"], SHOW_TIMEOUT, "main.rs to have unsaved changes")

        with self.step("switch to the second (clean) workspace, hover the first tab, click its close button"):
            self.click_tab(self.state(), 1)
            state = self.wait_for_tabs("the second tab active", {"active": [False, True]})
            self.hover_tab(state, 0)
            state = self.state()
            self.click_close_button(state, 0)
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt to save main.rs")

        with self.step("cancel the prompt (escape): the workspace is kept, both tabs remain"):
            # Cod shows one workspace's window at a time, so closing a background tab's prompt
            # shows it first (as the state just above already found: it is the active one, not
            # the second, once the prompt is up); canceling just means neither tab is lost.
            self.keys("escape")
            self.wait_for(lambda state: state["prompt"] is None, SHOW_TIMEOUT, "the prompt to close")
            state = self.wait_for_tabs(
                "both workspaces still open", {"names": ["billing-api", UNNAMED], "workspace_count": 2}
            )

        with self.step("the dirtied buffer is still dirty: nothing was saved or discarded"):
            self.expect_equal("main.rs still dirty", ["main.rs"], state["dirty_tabs"])

    # Issue #77 C4: natural width when there is room must not break correct shrinking when the
    # title bar is genuinely full. The test makes its own narrow window: it sets the output of
    # this run's sway to 640x600 logical pixels before Cod starts and restores the output's own
    # mode afterwards, so it runs in the same default script/e2e batch as the other tests.

    def narrow_the_output(self, width: int, height: int) -> None:
        """Sets the sway output to `width` x `height` logical pixels until the test ends."""
        output = next((output for output in sway_ipc("-t", "get_outputs") if output.get("active")), None)
        if output is None:
            raise AssertionError("expected an active sway output")
        scale = output.get("scale") or 1.0
        mode = output["current_mode"]
        original = f"{mode['width']}x{mode['height']}"
        sway_ipc("output", output["name"], "resolution", f"{round(width * scale)}x{round(height * scale)}")
        self.addCleanup(sway_ipc, "output", output["name"], "resolution", original)

    def cod_window_width(self) -> float:
        """The logical width sway gave Cod's window."""
        assert self.cod is not None
        pids = set(processes_with_marker(self.cod.marker))
        widths: list[float] = []

        def walk(node: dict[str, Any]) -> None:
            if node.get("pid") in pids and node.get("type") in ("con", "floating_con"):
                widths.append(node["rect"]["width"])
            for child in [*node.get("nodes", []), *node.get("floating_nodes", [])]:
                walk(child)

        walk(sway_ipc("-t", "get_tree"))
        if len(widths) != 1:
            raise AssertionError(f"expected one Cod window in sway's tree, actual widths {widths}")
        return float(widths[0])

    def test_ac7_narrow_window_many_short_tabs_stay_inside_the_bar(self) -> None:
        """Six default workspaces, each able to grow up to COD_TAB_MAX_WIDTH, in a 640 px title
        bar: every tab, its close button and "+" must stay inside it, and a name that does not
        fit is cut (truncated), not left overflowing past the window edge or blank."""
        with self.step("narrow this run's output to 640x600 for this test only"):
            self.narrow_the_output(640, 600)

        with self.step("start Cod with a fresh profile: its window is 640 px wide"):
            self.start_cod(None)
            state = self.wait_for_tabs("the first tab", {"names": [UNNAMED], "active": [True]})
            bar_width = self.cod_window_width()
            self.expect_equal("Cod's window width", 640.0, bar_width)

        with self.step('open five more default workspaces with "+", which stays inside the window for every click'):
            for expected_count in range(2, 7):
                add = title_bar(state)["add_button"]["bounds"]
                if drawn(add) and add["x"] + add["width"] > bar_width + 0.5:
                    raise AssertionError(
                        f'expected the "+" button inside the {bar_width} px window before opening workspace '
                        f'{expected_count}, actual right edge {add["x"] + add["width"]}; tab right edges: '
                        f'{[tab["bounds"]["x"] + tab["bounds"]["width"] for tab in title_bar(state)["tabs"]]}'
                    )
                self.click_add_button(state)
                state = self.wait_for(
                    lambda state, expected_count=expected_count: state["workspace_count"] == expected_count
                    and len(title_bar(state)["tabs"]) == expected_count,
                    SHOW_TIMEOUT,
                    f"{expected_count} workspaces and tabs",
                )

        with self.step("every tab, its close button and + stay inside the 640 px title bar"):
            bar = title_bar(state)
            tabs = bar["tabs"]
            self.expect_equal("six tabs drawn", 6, len(tabs))
            for index, tab in enumerate(tabs):
                if not drawn(tab["bounds"]) or tab["bounds"]["width"] <= 0:
                    raise AssertionError(f"expected tab {index} drawn with a positive width, actual {tab['bounds']!r}")
                right = tab["bounds"]["x"] + tab["bounds"]["width"]
                if right > bar_width + 0.5:
                    raise AssertionError(f"tab {index} ({tab['name']!r}) right edge {right} is past the {bar_width} px title bar")
                close = tab["close_bounds"]
                if close is not None:
                    close_right = close["x"] + close["width"]
                    if close_right > bar_width + 0.5:
                        raise AssertionError(f"tab {index}'s close button right edge {close_right} is past the title bar")
            add = bar["add_button"]["bounds"]
            add_right = add["x"] + add["width"]
            if add_right > bar_width + 0.5:
                raise AssertionError(f'expected the "+" button inside the {bar_width} px title bar, actual right edge {add_right}')

        with self.step("at least one name that does not fit is marked cut"):
            if not any(tab["truncated"] for tab in tabs):
                raise AssertionError("expected at least one of six tabs in a 640 px bar to be cut")


class FirstUseWelcomeTest(CodStatusFixtures, CodTestCase):
    """Issue #77 AC(f): a fresh profile's first workspace shows Cod's own welcome text, never "Zed"."""

    def test_ac7_fresh_profile_shows_cod_welcome_text_not_zed(self) -> None:
        with self.step("start Cod with a fresh profile and no folder"):
            self.start_cod(None)
            state = self.wait_for(
                lambda state: state["welcome"] is not None or state["active_tab"] == "Onboarding",
                SHOW_TIMEOUT,
                "the welcome page, or (an even more first-use state this reproduction found) Zed's own first-run onboarding page",
            )

        if state["welcome"] is None:
            with self.step("check the onboarding page's own header names Cod, never Zed"):
                onboarding_header = state["onboarding_header"]
                if onboarding_header is None or "Zed" in onboarding_header:
                    raise AssertionError(f'expected the onboarding header to never say "Zed", actual {onboarding_header!r}')

            with self.step("close the onboarding page, then show the welcome page"):
                self.keys("ctrl-w")
                self.run_command("zed: show welcome")
                state = self.wait_for(lambda state: state["welcome"] is not None, SHOW_TIMEOUT, "the welcome page")

        with self.step("check the welcome greeting names Cod, never Zed"):
            welcome = state["welcome"]
            if "Zed" in welcome:
                raise AssertionError(f'expected the welcome greeting to never say "Zed", actual {welcome!r}')

    # Issue #77 V1: a fresh profile must show one window throughout, not two.

    def test_ac7_fresh_profile_stays_in_one_window(self) -> None:
        """"zed: show welcome" (run right after closing onboarding, the normal first-use path)
        opened a second, mostly-empty window: with_active_or_new_workspace's cx.active_window()
        did not see the one already-open MultiWorkspace window (the platform had not reported it
        "active" yet) and fell back to opening a new one instead of reusing it."""
        with self.step("start Cod with a fresh profile and no folder"):
            self.start_cod(None)
            state = self.wait_for(
                lambda state: state["welcome"] is not None or state["active_tab"] == "Onboarding",
                SHOW_TIMEOUT,
                "the welcome page or the onboarding page",
            )
            self.expect_equal("one window at the start", 1, len(self.compositor_windows()))

        if state["welcome"] is None:
            with self.step("close onboarding: still one window"):
                self.keys("ctrl-w")
                self.expect_equal("one window after closing onboarding", 1, len(self.compositor_windows()))

            with self.step('"zed: show welcome": still one window'):
                self.run_command("zed: show welcome")
                self.wait_for(lambda state: state["welcome"] is not None, SHOW_TIMEOUT, "the welcome page")
                windows = self.compositor_windows()
                if len(windows) != 1:
                    raise AssertionError(
                        f"expected one Cod window after showing welcome, actual {len(windows)}: {[w['title'] for w in windows]}"
                    )

        with self.step('"+": still one window'):
            self.click_add_button(self.state())
            self.wait_for(lambda state: state["workspace_count"] == 2, SHOW_TIMEOUT, "the second workspace")
            self.expect_equal("one window after +", 1, len(self.compositor_windows()))


if __name__ == "__main__":
    unittest.main()
