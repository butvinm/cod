"""Story #43: agent-made HTML pages with a small, safe Cod action bridge.

Each test starts Cod on the throwaway git repo ~/work/billing-api (invented content), writes
ask.html into a temp folder (the story's own fixture: a text field and a Send button, an
open-file link, a focus-session button, an unknown action and an inline script that would change
the heading if it ran) and opens it with the real codctl binary, as an agent does through Bash.

ask.html's own interactive elements are laid out as absolutely positioned, full-size boxes (the
same trick page_server.py already uses for its links), so a test can compute a real mouse click
point for an element from the page's own drawn bounds (driver state pages.open[].bounds, #43)
without depending on font metrics or text layout.
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

from cod_e2e import CodTestCase
from fake_acp_agent import fake_agent_settings
from page_server import PageServer

COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
LOAD_TIMEOUT = 30.0
SETTLE_TIME = 2.0

AGENT_ID = "claude-acp"
SESSION_ID = "claude-session-pay-1234"

INVOICE = "".join(f"// line {number}: invented invoice rule {number}\n" for number in range(1, 30))
BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": INVOICE,
}


def ask_html(session_id: str = SESSION_ID) -> str:
    """The story's own fixture: a text field and `Send` button, an `open-file` link, a
    `focus-session` button, a button with an action outside the allowlist, and an inline script
    that would change the heading if it ran. Every interactive element is an absolutely
    positioned, full box of its own, at a known position, so a test can click it precisely.
    """
    box = "position:absolute;left:{left}px;top:{top}px;width:220px;height:30px;"
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>"
        '<body style="margin:0;position:relative">'
        '<h1 id="heading">Ask</h1>'
        f'<input id="answer" type="text" autofocus style="{box.format(left=0, top=0)}">'
        f'<button data-cod="submit" data-cod-value-from="#answer" '
        f'style="{box.format(left=230, top=0)}">Send</button>'
        f'<a data-cod="open-file" data-cod-path="src/invoice.rs:18" '
        f'style="{box.format(left=0, top=40)}display:block;">Open invoice.rs</a>'
        f'<button data-cod="focus-session" data-cod-session="{session_id}" '
        f'style="{box.format(left=0, top=80)}">Focus session</button>'
        f'<button data-cod="erase-everything" '
        f'style="{box.format(left=0, top=120)}">Not in the allowlist</button>'
        "<script>"
        "document.getElementById('heading').textContent='changed';"
        "if (window.cod) { window.cod.call('submit', {value: 'script-ran'}); }"
        "</script>"
        "</body></html>"
    )


def pages(state: dict[str, Any]) -> list[dict[str, Any]]:
    return state["pages"]["open"]


def page_by_id(state: dict[str, Any], page_id: str) -> dict[str, Any] | None:
    return next((page for page in pages(state) if page["id"] == page_id), None)


def chat(state: dict[str, Any]) -> dict[str, Any]:
    return state.get("chat") or {}


class AgentPagesTest(CodTestCase):
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

    def codctl_env(self) -> dict[str, str]:
        env = dict(os.environ)
        for name in ("COD_WORKSPACE", "COD_SOCKET"):
            env.pop(name, None)
        env["HOME"] = str(self.home)
        return env

    def codctl(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.binary), *args],
            env=self.codctl_env(),
            cwd=str(cwd) if cwd else None,
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT,
            stdin=subprocess.DEVNULL,
        )

    def codctl_popen(self, *args: str, cwd: Path | None = None) -> subprocess.Popen[str]:
        """Starts codctl without waiting for it, for `--wait`: the test drives the UI while it blocks."""
        return subprocess.Popen(
            [str(self.binary), *args],
            env=self.codctl_env(),
            cwd=str(cwd) if cwd else None,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
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

    def start_on_billing_api(self) -> Path:
        repo = self.make_repo("billing-api", BILLING_API)
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

    def write_ask_html(self, session_id: str = SESSION_ID) -> Path:
        folder = self.workdir / "agent-pages"
        folder.mkdir(exist_ok=True)
        path = folder / "ask.html"
        path.write_text(ask_html(session_id))
        return path

    def open_agent_page(self, repo: Path, *extra: str) -> str:
        """`codctl open --html ask.html [extra...]`, which must print the page id and exit 0 at once (#43 AC1)."""
        html_path = self.write_ask_html()
        result = self.codctl("open", "--html", str(html_path), *extra, cwd=repo)
        self.expect_result(result, 0, stderr="")
        if not re.fullmatch(r"page-\d+\n", result.stdout):
            raise AssertionError(f"open --html printed {result.stdout!r}, not a page id")
        page_id = result.stdout.strip()
        self.wait_for(
            lambda state: (page_by_id(state, page_id) or {}).get("state") == "loaded",
            LOAD_TIMEOUT,
            "the agent page to load",
        )
        return page_id

    def click_in_page(self, page_id: str, local_x: float, local_y: float) -> None:
        """A real mouse click at a point inside the page's own content, from its drawn bounds."""
        state = self.state()
        page = page_by_id(state, page_id)
        assert page is not None, f"page {page_id} is not open"
        bounds = page["bounds"]
        self.driver.request("click", x=bounds["left"] + local_x, y=bounds["top"] + local_y, ctrl=False)

    def output_log(self) -> str:
        assert self.cod is not None
        return self.cod.output_path.read_text(errors="replace") if self.cod.output_path.exists() else ""

    # AC1

    def test_ac1_opening_local_html_makes_an_agent_page(self) -> None:
        with self.step("start Cod on billing-api and open ask.html with codctl open --html"):
            repo = self.start_on_billing_api()
            page_id = self.open_agent_page(repo)

        with self.step("check that the page is agent-made, and a plain file:// page would not be"):
            page = page_by_id(self.state(), page_id)
            assert page is not None
            self.expect_equal("page kind", "agent", page["kind"])

        with self.step("open a plain URL as a page and check it is not agent-made"):
            url = self.server.url("/wiki/Home")
            other_id = self.codctl("open", url, cwd=repo).stdout.strip()
            state = self.wait_for(
                lambda state: (page_by_id(state, other_id) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the plain page to load",
            )
            other = page_by_id(state, other_id)
            assert other is not None
            self.expect_equal("plain page kind", "external", other["kind"])

    # AC2

    def test_ac2_wait_submits_by_keyboard_mouse_or_tab_close(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open --html --wait, type half-even, submit with Tab and Enter, and check the printed value"):
            html_path = self.write_ask_html()
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            page_id = None
            try:
                deadline = time.monotonic() + LOAD_TIMEOUT
                while page_id is None:
                    state = self.state()
                    candidates = [page["id"] for page in pages(state) if page.get("state") == "loaded"]
                    if candidates:
                        page_id = candidates[-1]
                    elif time.monotonic() > deadline:
                        raise AssertionError("codctl open --html --wait never showed a loaded page")
                    else:
                        time.sleep(0.1)
                # The inline script did not run (no --js): nothing auto-submitted yet.
                time.sleep(SETTLE_TIME)
                self.expect_equal("codctl open --html --wait exited already", None, process.poll())
                self.wait_for(
                    lambda state: (page_by_id(state, page_id) or {}).get("focused") is True,
                    SHOW_TIMEOUT,
                    "the agent page to have the focus",
                )
                self.type("half-even")
                self.keys("tab")
                self.keys("enter")
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            self.expect_equal("codctl open --html --wait exit code", 0, process.returncode)
            self.expect_equal("codctl open --html --wait stdout", "half-even\n", stdout)
            self.expect_equal("codctl open --html --wait stderr", "", stderr)
            self.wait_for(lambda state: page_by_id(state, page_id) is None, SHOW_TIMEOUT, "the page tab to close")

        with self.step("open --html --wait again, submit by a real mouse click, and check the printed value"):
            html_path = self.write_ask_html()
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                deadline = time.monotonic() + LOAD_TIMEOUT
                page_id = None
                while page_id is None:
                    state = self.state()
                    candidates = [page["id"] for page in pages(state) if page.get("state") == "loaded"]
                    if candidates:
                        page_id = candidates[-1]
                    elif time.monotonic() > deadline:
                        raise AssertionError("codctl open --html --wait never showed a loaded page")
                    else:
                        time.sleep(0.1)
                self.wait_for(
                    lambda state: (page_by_id(state, page_id) or {}).get("focused") is True,
                    SHOW_TIMEOUT,
                    "the agent page to have the focus",
                )
                self.type("half-even")
                # The Send button is the absolutely positioned box at left 230, top 0.
                self.click_in_page(page_id, 230 + 50, 15)
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            self.expect_equal("codctl open --html --wait exit code (mouse click)", 0, process.returncode)
            self.expect_equal("codctl open --html --wait stdout (mouse click)", "half-even\n", stdout)

        with self.step("open --html --wait a third time and close the tab with Ctrl-W: exit code 2"):
            html_path = self.write_ask_html()
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                deadline = time.monotonic() + LOAD_TIMEOUT
                page_id = None
                while page_id is None:
                    state = self.state()
                    candidates = [page["id"] for page in pages(state) if page.get("state") == "loaded"]
                    if candidates:
                        page_id = candidates[-1]
                    elif time.monotonic() > deadline:
                        raise AssertionError("codctl open --html --wait never showed a loaded page")
                    else:
                        time.sleep(0.1)
                self.wait_for(
                    lambda state: (page_by_id(state, page_id) or {}).get("focused") is True,
                    SHOW_TIMEOUT,
                    "the agent page to have the focus",
                )
                self.keys("ctrl-w")
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            self.expect_equal("codctl open --html --wait exit code (closed)", 2, process.returncode)
            self.expect_equal("codctl open --html --wait stdout (closed)", "", stdout)

        with self.step("a missing html file fails with exit code 1"):
            missing = self.workdir / "no-such-file.html"
            result = self.codctl("open", "--html", str(missing), cwd=repo)
            self.expect_equal("exit code of a missing --html file", 1, result.returncode)
            self.expect_equal("stdout of a missing --html file", "", result.stdout)

    # AC3

    def test_ac3_open_file_focus_session_and_refuse_others(self) -> None:
        with self.step("create the agent script and start Cod on billing-api"):
            repo = self.make_repo("billing-api", BILLING_API)
            script = self.workdir / "agent-script.json"
            script.write_text(json.dumps({"session_ids": [SESSION_ID], "turns": []}))
            agent_servers = {AGENT_ID: fake_agent_settings(script, log=self.workdir / "agent.log")}
            self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
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

        with self.step("start a chat with the fake agent, whose fixed session id the page's focus-session button names"):
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, SHOW_TIMEOUT, "the agent panel to load")
            self.run_command("agent: toggle focus")
            self.run_command("agent: toggle new thread menu")
            self.keys("end up enter")
            self.wait_for(lambda state: chat(state).get("session_id") == SESSION_ID, SHOW_TIMEOUT, "the fake agent's chat")

        with self.step("open ask.html and activate its open-file link: src/invoice.rs opens at line 18"):
            page_id = self.open_agent_page(repo)
            # The open-file link is the absolutely positioned box at left 0, top 40.
            self.click_in_page(page_id, 50, 55)
            self.wait_for_values(
                "editor",
                {"active_tab": "invoice.rs", "cursor": {"row": 18, "column": 1}},
                lambda state: {"active_tab": state["active_tab"], "cursor": state["cursor"]},
                SHOW_TIMEOUT,
            )

        with self.step("activate focus-session: the agent panel and that chat are focused again"):
            # Opening invoice.rs just above already moved the keyboard focus to the editor.
            self.expect_equal("agent panel not focused before focus-session", False, self.state().get("agent_panel_focused") is True)
            self.click_in_page(page_id, 50, 95)
            self.wait_for(
                lambda state: state.get("agent_panel_focused") is True and chat(state).get("session_id") == SESSION_ID,
                SHOW_TIMEOUT,
                "focus-session to focus the chat",
            )

        with self.step("activate the button whose action is not in the allowlist: nothing happens, and it is logged as refused"):
            before = self.state()
            self.click_in_page(page_id, 50, 135)
            time.sleep(SETTLE_TIME)
            after = self.state()
            self.expect_equal("active tab unchanged", before["active_tab"], after["active_tab"])
            self.expect_equal("agent panel focus unchanged", before.get("agent_panel_focused"), after.get("agent_panel_focused"))
            log_text = self.output_log()
            if "refused the action" not in log_text or "erase-everything" not in log_text:
                raise AssertionError(f"expected the refused action logged; last 4000 chars of the log: {log_text[-4000:]!r}")

    # AC4

    def test_ac4_inline_scripts_need_js(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("without --js, the inline script does not run: it never auto-submits"):
            html_path = self.write_ask_html()
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                time.sleep(SETTLE_TIME)
                self.expect_equal("codctl open --html --wait exited on its own (no --js)", None, process.poll())
            finally:
                process.kill()
                process.communicate()

        with self.step("with --js, the inline script runs and calls the allowlist through window.cod"):
            html_path = self.write_ask_html()
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", "--js", cwd=repo)
            try:
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            self.expect_equal("codctl open --html --wait --js exit code", 0, process.returncode)
            self.expect_equal("codctl open --html --wait --js stdout", "script-ran\n", stdout)

    # AC5

    def test_ac5_external_pages_get_no_bridge(self) -> None:
        with self.step("start Cod on billing-api and open the look-alike external ask page"):
            repo = self.start_on_billing_api()
            url = self.server.url("/agent-like/ask")
            page_id = self.codctl("open", url, cwd=repo).stdout.strip()
            self.wait_for(
                lambda state: (page_by_id(state, page_id) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the external page to load",
            )

        with self.step("check the page is not agent-made, then click its open-file link: nothing happens"):
            page = page_by_id(self.state(), page_id)
            assert page is not None
            self.expect_equal("external page kind", "external", page["kind"])
            before = self.state()
            # The link fills the whole page (page_server.py), so any point inside it is on target.
            self.click_in_page(page_id, 20, 20)
            time.sleep(SETTLE_TIME)
            after = self.state()
            self.expect_equal("active tab unchanged", before["active_tab"], after["active_tab"])
            self.expect_equal("tabs unchanged", before["tabs"], after["tabs"])


if __name__ == "__main__":
    unittest.main()
