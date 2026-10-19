"""Story #26: the workspace overview. One shortcut opens a floating picker with every workspace and its folders, sessions and pages, most urgent first; typing filters, the arrows move, Enter goes there, Escape goes back. One test per acceptance criterion or pair of criteria.

Fixtures are the throwaway git repo billing-api with invented content and three invented workspaces, made and named with the commands of #24 from the command palette in the order PAY-1234, DOC-208, OPS-77, so the window order differs from the order of urgency.
Session and page states come from the driver's set_entry test fixture (tests/e2e/cod_status_fixtures.py), which stands in for the scripted fake agent provider of #28 until it lands.
"""

from __future__ import annotations

import unittest
from typing import Any

from cod_e2e import CodTestCase
from cod_status_fixtures import CodStatusFixtures

NEW_WORKSPACE = "workspace: new workspace"
RENAME_WORKSPACE = "workspace: rename workspace"
NEXT_WORKSPACE = "workspace: next workspace"
PREVIOUS_WORKSPACE = "workspace: previous workspace"

# The overview shortcut on Linux (#26 AC1, D25).
OVERVIEW = "ctrl-alt-o"
PLACEHOLDER = "Go to session..."

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding"
DOC_NAME = "[DOC-208] (docs-site) Move the API section"
OPS_NAME = "[OPS-77] (infra) Upgrade Postgres to 17"

# Window order: PAY-1234 (0), DOC-208 (1), OPS-77 (2).
PAY, DOC, OPS = 0, 1, 2

MR_PAGE = "gitlab.example.com/billing/-/merge_requests/42"

# The rows of the fixtures in the order of urgency (#26 AC2, AC3): kind, title, second line (None for the workspace row of PAY-1234, whose second line is its folder path), dot.
EXPECTED_ROWS = [
    ("workspace", PAY_NAME, None, "yellow"),
    ("folder", "billing-api", None, None),
    ("session", "Claude: fix invoice rounding", "[PAY-1234] - waiting for you", "yellow"),
    ("session", "Codex: review migration", "[PAY-1234] - working", "blue"),
    ("session", "Claude: add tax tests", "[PAY-1234] - done, 6 tests added", "green"),
    ("page", "GitLab MR !42", f"[PAY-1234] - {MR_PAGE}", None),
    ("workspace", OPS_NAME, "", "red"),
    ("session", "Codex: plan Postgres 17 upgrade", "[OPS-77] - failed", "red"),
    ("workspace", DOC_NAME, "", None),
]
OPS_ROW = 6
DOC_ROW = 8

# How long a command, a key or a click may take to show.
SHOW_TIMEOUT = 15.0

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# billing-api\n\nAn invented service that issues invoices.\n",
    "src/main.rs": 'fn main() {\n    println!("billing-api");\n}\n',
}


def overview(state: dict[str, Any]) -> dict[str, Any]:
    found = state.get("overview")
    if not isinstance(found, dict):
        raise AssertionError(f"expected the overview to be open, actual {found!r}")
    return found


def titles(state: dict[str, Any]) -> list[str]:
    return [row["title"] for row in overview(state)["rows"]]


def is_open(state: dict[str, Any]) -> bool:
    return isinstance(state.get("overview"), dict)


class WorkspaceOverviewTest(CodStatusFixtures, CodTestCase):
    # Actions

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

    def start_with_fixtures(self) -> None:
        """Starts Cod on billing-api, makes and names the three workspaces and sets their sessions and pages. OPS-77 (the last made) is active."""
        with self.step("create the fixture repo billing-api"):
            billing_api = self.make_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-api and name its workspace PAY-1234"):
            self.start_cod(billing_api)
            self.wait_for(lambda state: state["workspace_count"] == 1 and state["modal"] is None, SHOW_TIMEOUT, "the first workspace")
            self.rename_workspace(PAY_NAME)

        with self.step("open and name DOC-208, then OPS-77"):
            self.new_workspace()
            self.rename_workspace(DOC_NAME)
            self.new_workspace()
            self.rename_workspace(OPS_NAME)

        with self.step("set the sessions and the page of PAY-1234 and the failed session of OPS-77"):
            self.set_entry(PAY, "pay-tests", "done", title="Claude: add tax tests", context="done, 6 tests added")
            self.set_entry(PAY, "pay-review", "working", title="Codex: review migration", context="working")
            self.set_entry(PAY, "pay-rounding", "needs_input", title="Claude: fix invoice rounding", context="waiting for you")
            self.set_entry(PAY, "pay-mr", "idle", title="GitLab MR !42", kind="page", context=MR_PAGE)
            self.set_entry(OPS, "ops-plan", "error", title="Codex: plan Postgres 17 upgrade", context="failed")
            self.wait_for(
                lambda state: [workspace["status"] for workspace in state["workspaces"]] == ["needs_input", "idle", "error"],
                SHOW_TIMEOUT,
                "the workspace statuses",
            )

    def open_overview(self) -> dict[str, Any]:
        self.keys(OVERVIEW)
        return self.wait_for(
            lambda state: is_open(state) and overview(state)["focused"] and len(overview(state)["rows"]) > 0,
            SHOW_TIMEOUT,
            "the overview to open with the focus",
        )

    def wait_for_selected(self, index: int) -> dict[str, Any]:
        return self.wait_for(
            lambda state: is_open(state) and overview(state)["selected_index"] == index,
            SHOW_TIMEOUT,
            f"row {index} to be selected",
        )

    # AC1

    def test_ac1_the_shortcut_opens_the_overview_and_ctrl_p_still_opens_the_file_finder(self) -> None:
        self.start_with_fixtures()

        with self.step("press ctrl-alt-o: the overview opens with its placeholder, an empty query and the first row selected"):
            state = self.open_overview()
            found = overview(state)
            self.expect_equal("placeholder", PLACEHOLDER, found["placeholder"])
            self.expect_equal("query", "", found["query"])
            self.expect_equal("selected index", 0, found["selected_index"])
            self.expect_equal("first row selected", True, found["rows"][0]["selected"])
            self.expect_equal("modal", "other", state["modal"])

        with self.step("press escape, then ctrl-p: Zed's file finder opens"):
            self.keys("escape")
            self.wait_for(lambda state: state["overview"] is None and state["modal"] is None, SHOW_TIMEOUT, "the overview to close")
            self.keys("ctrl-p")
            state = self.wait_for(lambda state: state["modal"] == "file_finder", SHOW_TIMEOUT, "the file finder to open")
            self.expect_equal("overview", None, state["overview"])
            self.keys("escape")
            self.wait_for(lambda state: state["modal"] is None, SHOW_TIMEOUT, "the file finder to close")

    # AC2, AC3

    def test_ac2_ac3_rows_show_two_lines_and_dots_in_order_of_urgency(self) -> None:
        self.start_with_fixtures()

        with self.step("open the overview"):
            state = self.open_overview()
            rows = overview(state)["rows"]

        with self.step("check the rows: workspaces by urgency (PAY-1234, OPS-77, DOC-208), each with its folders, sessions by urgency and pages under it"):
            self.expect_equal(
                "rows (kind, title, dot)",
                [(kind, title, dot) for kind, title, _, dot in EXPECTED_ROWS],
                [(row["kind"], row["title"], row["dot"]) for row in rows],
            )

        with self.step("check the second lines: the folder path for a workspace, [ID] - context below it"):
            for index, (_, title, detail, _) in enumerate(EXPECTED_ROWS):
                if detail is not None:
                    self.expect_equal(f"second line of {title!r}", detail, rows[index]["detail"])
            workspace_path = rows[0]["detail"]
            if not workspace_path.endswith("/billing-api"):
                raise AssertionError(f"expected the PAY-1234 row to show the path of billing-api, actual {workspace_path!r}")
            self.expect_equal("second line of the billing-api folder", f"[PAY-1234] - {workspace_path}", rows[1]["detail"])

    # AC4

    def test_ac4_typing_filters_and_keeps_matches_under_their_workspace(self) -> None:
        self.start_with_fixtures()

        with self.step("open the overview and type postgres: only OPS-77 and its session stay"):
            self.open_overview()
            self.type("postgres")
            state = self.wait_for(lambda state: is_open(state) and overview(state)["query"] == "postgres", SHOW_TIMEOUT, "the query")
            self.expect_equal("rows", [OPS_NAME, "Codex: plan Postgres 17 upgrade"], titles(state))
            self.expect_equal("first row selected", 0, overview(state)["selected_index"])

        with self.step("type tax tests instead: the session stays under PAY-1234"):
            self.keys(" ".join(["backspace"] * len("postgres")))
            self.type("tax tests")
            state = self.wait_for(lambda state: is_open(state) and overview(state)["query"] == "tax tests", SHOW_TIMEOUT, "the query")
            self.expect_equal("rows", [PAY_NAME, "Claude: add tax tests"], titles(state))

        with self.step("backspace widens the filter again: every row is back"):
            self.keys(" ".join(["backspace"] * len("tax tests")))
            state = self.wait_for(lambda state: is_open(state) and overview(state)["query"] == "", SHOW_TIMEOUT, "an empty query")
            self.expect_equal("rows", [title for _, title, _, _ in EXPECTED_ROWS], titles(state))

    # AC5

    def test_ac5_up_and_down_move_one_row_left_and_right_jump_between_workspaces(self) -> None:
        self.start_with_fixtures()

        with self.step("open the overview"):
            self.open_overview()

        with self.step("down and up move one row"):
            self.keys("down")
            self.wait_for_selected(1)
            self.keys("down")
            self.wait_for_selected(2)
            self.keys("up")
            self.wait_for_selected(1)

        with self.step("right jumps to the next workspace row, OPS-77, then DOC-208"):
            self.keys("right")
            state = self.wait_for_selected(OPS_ROW)
            self.expect_equal("selected row", OPS_NAME, overview(state)["rows"][OPS_ROW]["title"])
            self.keys("right")
            state = self.wait_for_selected(DOC_ROW)
            self.expect_equal("selected row", DOC_NAME, overview(state)["rows"][DOC_ROW]["title"])

        with self.step("left jumps back to OPS-77, then PAY-1234"):
            self.keys("left")
            self.wait_for_selected(OPS_ROW)
            self.keys("left")
            self.wait_for_selected(0)

    # AC6

    def test_ac6_enter_switches_to_the_workspace_of_the_row(self) -> None:
        self.start_with_fixtures()

        with self.step("select the OPS-77 session and press enter: the overview closes and OPS-77 stays active"):
            self.run_command(PREVIOUS_WORKSPACE)
            self.wait_for(lambda state: state["active_workspace"] == DOC and state["modal"] is None, SHOW_TIMEOUT, "DOC-208 to be active")
            self.open_overview()
            self.type("postgres")
            self.wait_for(lambda state: is_open(state) and len(overview(state)["rows"]) == 2, SHOW_TIMEOUT, "the filtered rows")
            self.keys("down")
            self.wait_for_selected(1)
            self.keys("enter")
            self.wait_for(
                lambda state: state["overview"] is None and state["modal"] is None and state["active_workspace"] == OPS,
                SHOW_TIMEOUT,
                "OPS-77 to be active",
            )

        with self.step("select the DOC-208 workspace row and press enter: DOC-208 becomes active"):
            self.open_overview()
            self.keys("right right")
            self.wait_for_selected(DOC_ROW)
            self.keys("enter")
            self.wait_for(
                lambda state: state["overview"] is None and state["modal"] is None and state["active_workspace"] == DOC,
                SHOW_TIMEOUT,
                "DOC-208 to be active",
            )

        with self.step("select the billing-api folder and press enter: PAY-1234 becomes active and the project panel focuses billing-api"):
            self.open_overview()
            self.keys("down")
            self.wait_for_selected(1)
            self.keys("enter")
            self.wait_for(
                lambda state: state["overview"] is None
                and state["active_workspace"] == PAY
                and (state["project_panel"] or {}).get("focused") is True
                and (state["project_panel"] or {}).get("selected") == "billing-api",
                SHOW_TIMEOUT,
                "the billing-api root to be focused in the project panel of PAY-1234",
            )

    # AC7

    def test_ac7_escape_or_a_click_outside_closes_and_gives_the_focus_back(self) -> None:
        self.start_with_fixtures()

        with self.step("show PAY-1234 and open README.md there with the file finder: the editor has the focus"):
            self.run_command(NEXT_WORKSPACE)
            self.wait_for(lambda state: state["active_workspace"] == PAY and state["modal"] is None, SHOW_TIMEOUT, "PAY-1234 to be active")
            self.keys("ctrl-p")
            self.wait_for(lambda state: state["file_finder_ready"], SHOW_TIMEOUT, "the file finder to take typed keys")
            self.type("README")
            self.keys("enter")
            before = self.wait_for(
                lambda state: state["modal"] is None and (state["focus"] or {}).get("kind") == "item" and (state["focus"] or {}).get("tab") == "README.md",
                SHOW_TIMEOUT,
                "README.md to be open with the focus",
            )

        with self.step("open the overview and press escape: it closes, the focus is back and no workspace switched"):
            self.open_overview()
            self.keys("right")
            self.wait_for_selected(OPS_ROW)
            self.keys("escape")
            state = self.wait_for(lambda state: state["overview"] is None and state["modal"] is None, SHOW_TIMEOUT, "the overview to close")
            self.expect_equal("focus", before["focus"], state["focus"])
            self.expect_equal("active workspace", before["active_workspace"], state["active_workspace"])

        with self.step("open the overview and click outside it: it closes, the focus is back and no workspace switched"):
            self.open_overview()
            self.keys("down")
            self.wait_for_selected(1)
            # Below the title bar and left of the picker, which is centered near the top of the window.
            self.click_at(10, 60)
            state = self.wait_for(lambda state: state["overview"] is None and state["modal"] is None, SHOW_TIMEOUT, "the overview to close")
            self.expect_equal("focus", before["focus"], state["focus"])
            self.expect_equal("active workspace", before["active_workspace"], state["active_workspace"])


if __name__ == "__main__":
    unittest.main()
