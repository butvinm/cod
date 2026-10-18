"""Beta trial for story #24, not for merge: one user works on two tickets in the same repo, keys only.

Persona: Rowan Achterberg, a backend developer. They keep two tickets of the invented checkout-api repo open side by side as two Cod workspaces on the same folder, rename them in the "[TICKET] (repo) Title" pattern, leave an unsaved edit in one, switch back and forth, quit and restart, and finally close the second workspace through its save prompt (Cancel first, then Save).
The helpers of test_24_workspaces are reused as plain functions; observations a user would notice (window title, prompt text) are logged as "OBSERVE:" lines, and extra screenshots go to <artifacts>/<test id>-snaps/.
"""

from __future__ import annotations

import subprocess
import time
import unittest
from typing import Any

import test_24_workspaces as w24
from cod_e2e import DRIVER_START_TIMEOUT, CodTestCase, artifact_name, artifacts_root, log

TAX_NAME = "[SHOP-210] (checkout-api) Fix tax rounding"
REFUND_NAME = "[SHOP-233] (checkout-api) Refund export"

TAX_RS = (
    "//! Tax of the invented checkout-api service.\n"
    "\n"
    "pub struct TaxRate {\n"
    "    pub region: String,\n"
    "    pub basis_points: u32,\n"
    "}\n"
    "\n"
    "impl TaxRate {\n"
    "    pub fn tax_milli_cents(&self, net_cents: i64) -> i64 {\n"
    "        net_cents * i64::from(self.basis_points) / 10\n"
    "    }\n"
    "}\n"
    "\n"
    "/// Rounds a value in thousandths of a cent to whole cents.\n"
    "pub fn round_cents(value_milli: i64) -> i64 {\n"
    "    (value_milli + 500) / 1000\n"
    "}\n"
    "pub fn total_with_tax(net_cents: i64, rate: &TaxRate) -> i64 {\n"
    "    net_cents + round_cents(rate.tax_milli_cents(net_cents))\n"
    "}\n"
    "\n"
    + w24.numbered_lines("// tax note", 14)
)

REFUND_RS = (
    "//! Refunds of the invented checkout-api service.\n"
    "\n"
    "pub struct Refund {\n"
    "    pub order: u32,\n"
    "    pub amount_cents: i64,\n"
    "    pub reason: String,\n"
    "}\n"
    "\n"
    "pub fn export_csv(refunds: &[Refund]) -> String {\n"
    "    let mut out = String::from(\"order,amount_cents,reason\\n\");\n"
    "    for refund in refunds {\n"
    "        out.push_str(&format!(\"{},{},{}\\n\", refund.order, refund.amount_cents, refund.reason));\n"
    "    }\n"
    "    out\n"
    "}\n"
    "\n"
    + w24.numbered_lines("// refund note", 14)
)

CHECKOUT_API = {
    "Cargo.toml": '[package]\nname = "checkout-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "README.md": "# checkout-api\n\nAn invented shop checkout service.\n",
    "src/main.rs": "mod refund;\nmod tax;\n\nfn main() {\n    println!(\"checkout-api\");\n}\n",
    "src/tax.rs": TAX_RS,
    "src/refund.rs": REFUND_RS,
}

TAX_DRAFT = "// beta draft "
REFUND_DRAFT = "Draft "


class BetaWorkspacesTest(CodTestCase):
    open_file = w24.WorkspacesTest.open_file
    go_to_line = w24.WorkspacesTest.go_to_line
    add_folder = w24.WorkspacesTest.add_folder
    new_workspace = w24.WorkspacesTest.new_workspace
    wait_for_rename_field = w24.WorkspacesTest.wait_for_rename_field
    rename_workspace = w24.WorkspacesTest.rename_workspace
    switch_workspace = w24.WorkspacesTest.switch_workspace
    wait_for_title = w24.WorkspacesTest.wait_for_title
    stays = w24.WorkspacesTest.stays
    wait_for_worktrees = w24.WorkspacesTest.wait_for_worktrees

    def snap(self, name: str) -> None:
        """Saves a screenshot of what the user sees now; observation only, never fails the test."""
        target = artifacts_root() / (artifact_name(self.id()) + "-snaps")
        target.mkdir(parents=True, exist_ok=True)
        try:
            subprocess.run(["grim", str(target / f"{name}.png")], capture_output=True, text=True, timeout=20)
        except (OSError, subprocess.TimeoutExpired) as error:
            log(f"  snap {name} failed: {error}")

    def observe(self, what: str) -> dict[str, Any]:
        state = self.state()
        names = [workspace["name"] for workspace in state["workspaces"]]
        log(
            f"OBSERVE [{what}]: title={state['window_title']!r} active={state['active_workspace']} names={names} "
            f"tabs={state['tabs']} cursor={state['cursor']} dirty={state['dirty_tabs']} prompt={state['prompt']} rename={state['rename_input']}"
        )
        return state

    def expect_title_shows_only(self, state: dict[str, Any], shown: str, hidden: str) -> None:
        title = state["window_title"] or ""
        if shown not in title:
            raise AssertionError(f"expected the window title to show {shown!r}, actual {title!r}")
        if hidden in title:
            raise AssertionError(f"expected the window title not to show {hidden!r}, actual {title!r}")

    def expect_drawn(self, state: dict[str, Any], shown: str, hidden: str) -> None:
        # The title can change a frame before the new tabs are drawn (run 39139367154), so wait for the drawing.
        self.wait_for(
            lambda state: shown in state["drawn_tabs"] and hidden not in state["drawn_tabs"],
            w24.SHOW_TIMEOUT,
            f"the window to draw {shown!r} and not {hidden!r}",
        )

    def test_two_tickets_in_one_repo(self) -> None:
        with self.step("create the invented repo checkout-api"):
            repo = self.make_repo("checkout-api", CHECKOUT_API)
            tax_path = repo / "src" / "tax.rs"
            refund_path = repo / "src" / "refund.rs"
            tax_edited = w24.insert_at_line(TAX_RS, 18, TAX_DRAFT)
            refund_edited = w24.insert_at_line(REFUND_RS, 9, REFUND_DRAFT)
            folder = [w24.real(repo)]

        with self.step("start Cod on checkout-api, keeping its session"):
            self.start_cod(repo, persistent=True)
            self.wait_for_worktrees([repo])

        with self.step("check the starting state: one window, one unnamed workspace, no tab"):
            start = self.observe("start")
            self.expect_equal("window count", 1, start["window_count"])
            self.expect_equal("workspace count", 1, start["workspace_count"])
            self.expect_equal("workspace name", None, w24.active(start)["name"])
            self.expect_equal("tabs", [], start["tabs"])
            self.snap("01-start")

        with self.step("first ticket: open main.rs, then tax.rs, go to line 18"):
            self.open_file("main.rs")
            self.open_file("tax.rs")
            self.go_to_line(18)

        with self.step("first ticket: type an unsaved edit at line 18"):
            self.type(TAX_DRAFT)
            self.wait_for_values(
                "the edited tax.rs",
                {"dirty_tabs": ["tax.rs"], "cursor": {"row": 18, "column": 1 + len(TAX_DRAFT)}, "unsaved_texts": [{"tab": "tax.rs", "text": tax_edited}]},
                w24.layout,
                10,
            )
            self.expect_equal("tax.rs on disk", TAX_RS, tax_path.read_text())

        with self.step(f"first ticket: run {w24.RENAME_WORKSPACE!r}, type {TAX_NAME!r}, look at the field, press enter"):
            self.run_command(w24.RENAME_WORKSPACE)
            state = self.wait_for_rename_field("the rename field to show")
            log(f"OBSERVE [rename field of an unnamed workspace]: proposed text {state['rename_input']['text']!r}")
            self.type(TAX_NAME)
            self.wait_for_values("the rename field", {"text": TAX_NAME}, lambda state: state["rename_input"] or {}, w24.SHOW_TIMEOUT)
            self.snap("02-rename-field-long-name")
            self.keys("enter")
            self.wait_for_values("the named first workspace", {"name": TAX_NAME, "rename_input": None, "modal": None}, w24.layout, w24.SHOW_TIMEOUT)
            state = self.wait_for_title(TAX_NAME)
            self.observe("first ticket named")
            self.snap("03-title-first-ticket")
            a_index = state["active_workspace"]

        with self.step("first ticket: open the rename field again, type something, press escape: the name stays"):
            self.run_command(w24.RENAME_WORKSPACE)
            state = self.wait_for_rename_field("the rename field to show again")
            self.expect_equal("proposed text of the rename field", TAX_NAME, state["rename_input"]["text"])
            self.type("oops")
            self.keys("escape")
            self.wait_for_values("the cancelled rename", {"rename_input": None, "modal": None, "name": TAX_NAME}, w24.layout, w24.SHOW_TIMEOUT)
            self.stays(lambda state: w24.active(state)["name"] == TAX_NAME and "oops" not in (state["window_title"] or ""), w24.SETTLE_TIME, "the old name")

        with self.step("check the edit survived the rename: tax.rs still unsaved at line 18, cursor where it was"):
            self.wait_for_values(
                "the first ticket after renaming",
                {"active_tab": "tax.rs", "dirty_tabs": ["tax.rs"], "cursor": {"row": 18, "column": 1 + len(TAX_DRAFT)}},
                w24.layout,
                w24.SHOW_TIMEOUT,
            )

        with self.step(f"second ticket: run {w24.NEW_WORKSPACE!r}"):
            self.new_workspace()
            self.observe("new workspace")
            self.snap("04-new-workspace")

        with self.step(f"second ticket: add the SAME folder checkout-api with {w24.ADD_FOLDER!r}"):
            self.add_folder(repo)

        with self.step("second ticket: open refund.rs and go to line 9"):
            self.open_file("refund.rs")
            self.go_to_line(9)

        with self.step(f"second ticket: rename it to {REFUND_NAME!r}"):
            self.rename_workspace(REFUND_NAME)
            state = self.wait_for_title(REFUND_NAME)
            self.observe("second ticket named")
            self.snap("05-title-second-ticket")
            b_index = state["active_workspace"]
            if b_index == a_index:
                raise AssertionError(f"expected the second ticket at another index than the first ({a_index}), actual {b_index}")
            self.expect_title_shows_only(state, REFUND_NAME, TAX_NAME)

        a_expected = {
            "name": TAX_NAME,
            "worktrees": folder,
            "panes": [{"tabs": ["main.rs", "tax.rs"], "active_tab": "tax.rs"}],
            "active_tab": "tax.rs",
            "cursor": {"row": 18, "column": 1 + len(TAX_DRAFT)},
            "dirty_tabs": ["tax.rs"],
            "unsaved_texts": [{"tab": "tax.rs", "text": tax_edited}],
            "window_count": 1,
        }
        b_expected = {
            "name": REFUND_NAME,
            "worktrees": folder,
            "panes": [{"tabs": ["refund.rs"], "active_tab": "refund.rs"}],
            "active_tab": "refund.rs",
            "cursor": {"row": 9, "column": 1},
            "dirty_tabs": [],
            "unsaved_texts": [],
            "window_count": 1,
        }

        with self.step(f"switch to the first ticket with {w24.PREVIOUS_WORKSPACE!r}: its tabs, cursor and unsaved edit are there"):
            state = self.switch_workspace(w24.PREVIOUS_WORKSPACE, a_index, "the first ticket", a_expected)
            state = self.wait_for_title(TAX_NAME)
            self.expect_title_shows_only(state, TAX_NAME, REFUND_NAME)
            self.expect_drawn(state, "tax.rs", "refund.rs")
            self.observe("back on first ticket")
            self.snap("06-switched-to-first")

        with self.step(f"switch to the second ticket with {w24.NEXT_WORKSPACE!r}: refund.rs at line 9"):
            state = self.switch_workspace(w24.NEXT_WORKSPACE, b_index, "the second ticket", b_expected)
            state = self.wait_for_title(REFUND_NAME)
            self.expect_drawn(state, "refund.rs", "tax.rs")

        with self.step(f"press {w24.NEXT_WORKSPACE!r} once more from the last workspace and see where it goes"):
            self.run_command(w24.NEXT_WORKSPACE)
            time.sleep(w24.SETTLE_TIME)
            state = self.observe("next from the last workspace")
            log(f"OBSERVE [next from last]: active workspace {state['active_workspace']} (first ticket is {a_index}, second is {b_index})")

        with self.step("make the first ticket active again before quitting"):
            if self.state()["active_workspace"] != a_index:
                self.switch_workspace(w24.PREVIOUS_WORKSPACE, a_index, "the first ticket", a_expected)
            self.wait_for_values("the first ticket", a_expected, w24.layout, w24.SHOW_TIMEOUT)
            self.expect_equal("tax.rs on disk", TAX_RS, tax_path.read_text())

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that both tickets came back and the first one, with its unsaved edit, is active again"):
            state = self.wait_for_values(
                "the restored first ticket",
                {"workspace_count": 2, "active_workspace": a_index, "modal": None, **a_expected},
                w24.layout,
                w24.RESTORE_TIMEOUT,
            )
            self.wait_for_values(
                "the restored workspaces",
                {"workspaces": sorted([{"name": TAX_NAME, "worktrees": folder}, {"name": REFUND_NAME, "worktrees": folder}], key=str)},
                lambda state: {"workspaces": sorted(w24.workspace_list(state), key=str)},
                w24.SHOW_TIMEOUT,
            )
            state = self.wait_for_title(TAX_NAME)
            self.observe("after restart")
            self.snap("07-after-restart")

        with self.step(f"after the restart, switch to the second ticket with {w24.NEXT_WORKSPACE!r}: refund.rs at line 9"):
            started = time.monotonic()
            state = self.switch_workspace(w24.NEXT_WORKSPACE, b_index, "the restored second ticket", b_expected)
            log(f"OBSERVE [first switch after restart]: the second ticket showed its layout {time.monotonic() - started:.2f} s after the palette command started")
            self.wait_for_title(REFUND_NAME)
            self.observe("second ticket after restart")
            self.snap("07b-second-after-restart")

        with self.step("second ticket: type an unsaved edit in refund.rs"):
            self.type(REFUND_DRAFT)
            self.wait_for_values(
                "the edited refund.rs",
                {"dirty_tabs": ["refund.rs"], "unsaved_texts": [{"tab": "refund.rs", "text": refund_edited}]},
                w24.layout,
                10,
            )

        with self.step(f"close the second ticket with {w24.CLOSE_WORKSPACE!r}: Cod asks to save refund.rs only"):
            self.run_command(w24.CLOSE_WORKSPACE)
            state = self.wait_for(lambda state: state["prompt"] is not None, w24.SHOW_TIMEOUT, "a prompt to save refund.rs")
            self.observe("close prompt")
            self.snap("08-close-prompt")
            prompt = state["prompt"]
            self.expect_equal("prompt answers", w24.SAVE_ANSWERS, prompt["answers"])
            if "refund.rs" not in prompt["message"] or "tax.rs" in prompt["message"]:
                raise AssertionError(f"expected a prompt about refund.rs only, actual {prompt!r}")
            self.expect_equal("workspace count while asking", 2, state["workspace_count"])

        with self.step("answer with escape (Cancel): the second ticket stays with its unsaved edit"):
            self.keys("escape")
            self.wait_for_values(
                "the second ticket after Cancel",
                {"prompt": None, "workspace_count": 2, "active_workspace": b_index, "name": REFUND_NAME, "dirty_tabs": ["refund.rs"], "unsaved_texts": [{"tab": "refund.rs", "text": refund_edited}]},
                w24.layout,
                w24.SHOW_TIMEOUT,
            )
            self.stays(lambda state: state["workspace_count"] == 2 and state["prompt"] is None, w24.SETTLE_TIME, "two workspaces after Cancel")
            self.expect_equal("refund.rs on disk", REFUND_RS, refund_path.read_text())
            self.observe("after cancel")

        with self.step(f"close the second ticket again with {w24.CLOSE_WORKSPACE!r} and answer with enter (Save)"):
            self.run_command(w24.CLOSE_WORKSPACE)
            state = self.wait_for(lambda state: state["prompt"] is not None, w24.SHOW_TIMEOUT, "the save prompt again")
            self.expect_equal("highlighted answer", 0, state["prompt"]["highlighted"])
            self.keys("enter")

        with self.step("check that refund.rs was saved, the second ticket is gone and the first ticket is active with its unsaved edit"):
            state = self.wait_for_values(
                "the first ticket after the close",
                {"prompt": None, "workspace_count": 1, "modal": None, **a_expected},
                w24.layout,
                w24.SHOW_TIMEOUT,
            )
            deadline = time.monotonic() + 10
            while refund_path.read_text() != refund_edited and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("refund.rs on disk", refund_edited, refund_path.read_text())
            self.expect_equal("tax.rs on disk", TAX_RS, tax_path.read_text())
            state = self.wait_for_title(TAX_NAME)
            self.observe("after close and save")
            self.snap("09-after-close")

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()

        with self.step("check that only the first ticket comes back, still named, with its unsaved edit, and the second stays gone"):

            def restored(state: dict[str, Any]) -> bool:
                names = [workspace["name"] for workspace in state["workspaces"]]
                if REFUND_NAME in names:
                    raise AssertionError(f"expected the closed second ticket to stay closed, actual workspaces {names}")
                return all(w24.layout(state)[key] == value for key, value in a_expected.items())

            self.wait_for(restored, w24.RESTORE_TIMEOUT, "the first ticket to be restored")
            state = self.stays(lambda state: state["workspace_count"] == 1 and REFUND_NAME not in str(state["workspaces"]), w24.SETTLE_TIME, "exactly the first ticket")
            self.wait_for_title(TAX_NAME)
            self.observe("final restart")
            self.snap("10-final-restart")

    def test_path_launch_then_plain_start_keeps_each_workspace_once(self) -> None:
        """After a session with two named workspaces, Rowan starts Cod once with the repo path (cod checkout-api), quits, and starts it plainly: every workspace shows once."""
        with self.step("create the invented repo checkout-api"):
            repo = self.make_repo("checkout-api", CHECKOUT_API)

        with self.step("start Cod on checkout-api, keeping its session"):
            self.start_cod(repo, persistent=True)
            self.wait_for_worktrees([repo])

        with self.step(f"name the first workspace {TAX_NAME!r} with tax.rs open"):
            self.open_file("tax.rs")
            self.rename_workspace(TAX_NAME)
            self.wait_for_title(TAX_NAME)

        with self.step(f"second workspace on the same folder, refund.rs open, named {REFUND_NAME!r}"):
            self.new_workspace()
            self.add_folder(repo)
            self.open_file("refund.rs")
            self.rename_workspace(REFUND_NAME)
            self.wait_for_title(REFUND_NAME)
            before = self.observe("before the path launch")
            self.expect_equal("names before", sorted([TAX_NAME, REFUND_NAME]), sorted(workspace["name"] for workspace in before["workspaces"]))

        with self.step("quit Cod with ctrl-q and start it with the folder argument checkout-api"):
            assert self.cod is not None
            self.cod.quit()
            self.cod.folder = repo
            self.cod.start()
            self.cod.connect(DRIVER_START_TIMEOUT)
            self.wait_for(lambda state: state["active_workspace"] is not None and repo.name in str(state["worktrees"]), w24.RESTORE_TIMEOUT, "a workspace with checkout-api")
            time.sleep(w24.SETTLE_TIME * 2)
            self.observe("after the path launch")
            self.snap("p1-after-path-launch")

        with self.step("quit Cod with ctrl-q and start it plainly"):
            self.restart_cod()

        with self.step("check that every workspace appears exactly once, in one window, and both named tickets are there"):
            deadline = time.monotonic() + w24.RESTORE_TIMEOUT
            while time.monotonic() < deadline:
                names = [workspace["name"] for workspace in self.state()["workspaces"]]
                if TAX_NAME in names and REFUND_NAME in names:
                    break
                time.sleep(0.2)
            time.sleep(w24.SETTLE_TIME * 2)
            state = self.observe("after the plain start")
            self.snap("p2-after-plain-start")
            entries = w24.workspace_list(state)
            names = [entry["name"] for entry in entries]
            log(f"OBSERVE [path launch then plain start]: windows={state['window_count']} workspaces={entries}")
            duplicates = [entry for index, entry in enumerate(entries) if entry in entries[:index]]
            if duplicates:
                raise AssertionError(f"expected every workspace once, actual duplicates {duplicates} in {entries}")
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal(f"times {TAX_NAME!r} shows", 1, names.count(TAX_NAME))
            self.expect_equal(f"times {REFUND_NAME!r} shows", 1, names.count(REFUND_NAME))


if __name__ == "__main__":
    unittest.main()
