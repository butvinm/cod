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
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from cod_e2e import CodTestCase
from fake_acp_agent import fake_agent_settings
from page_server import PageServer

COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
LOAD_TIMEOUT = 30.0
RESTORE_TIMEOUT = 45.0
SETTLE_TIME = 2.0

AGENT_ID = "claude-acp"
SESSION_ID = "claude-session-pay-1234"

INVOICE = "".join(f"// line {number}: invented invoice rule {number}\n" for number in range(1, 30))
BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": INVOICE,
}


def ask_html(session_id: str = SESSION_ID, open_file_path: str = "src/invoice.rs:18") -> str:
    """The story's own fixture: a text field and `Send` button, an `open-file` link, a
    `focus-session` button, a button with an action outside the allowlist, and an inline script
    that would change the heading if it ran. Every interactive element is an absolutely
    positioned, full box of its own, at a known position, so a test can click it precisely.
    """
    # The driver exposes the page rectangle, not DOM element rectangles.
    # A flow root keeps the h1's margin inside the body, so its positioned boxes
    # use the page origin assumed by click_in_page instead of a collapsed margin.
    box = "position:absolute;left:{left}px;top:{top}px;width:220px;height:30px;"
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>"
        '<body style="margin:0;position:relative;display:flow-root">'
        '<h1 id="heading">Ask</h1>'
        f'<input id="answer" type="text" autofocus style="{box.format(left=0, top=0)}">'
        f'<button data-cod="submit" data-cod-value-from="#answer" '
        f'style="{box.format(left=230, top=0)}">Send</button>'
        f'<a data-cod="open-file" data-cod-path="{open_file_path}" '
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


def large_ask_html() -> str:
    """RC review new defect #1: well over 6 KiB of invented filler, so the page's own `data:`
    address is longer than the engine's 8192-byte diagnostic cap and every reported address
    comes back truncated, starting with the very first load. Submitting must still answer
    `--wait`.
    """
    box = "position:absolute;left:{left}px;top:{top}px;width:220px;height:30px;"
    filler = "invented filler text for RC review new defect number one. " * 200
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>"
        f"<!-- {filler} -->"
        '<body style="margin:0;position:relative;display:flow-root">'
        '<h1 id="heading">Ask (large page)</h1>'
        f'<input id="answer" type="text" autofocus value="half-even" style="{box.format(left=0, top=0)}">'
        f'<button data-cod="submit" data-cod-value-from="#answer" '
        f'style="{box.format(left=230, top=0)}">Send</button>'
        "</body></html>"
    )


def long_value_ask_html(length: int) -> str:
    """RC review B5: a field prefilled with `length` ASCII characters, so a real mouse click on
    Send submits a value this long through the bridge's console-line transport.
    """
    box = "position:absolute;left:{left}px;top:{top}px;width:220px;height:30px;"
    value = "v" * length
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>"
        '<body style="margin:0;position:relative;display:flow-root">'
        '<h1 id="heading">Ask (long value)</h1>'
        f'<input id="answer" type="text" value="{value}" style="{box.format(left=0, top=0)}">'
        f'<button data-cod="submit" data-cod-value-from="#answer" '
        f'style="{box.format(left=230, top=0)}">Send</button>'
        "</body></html>"
    )


def head_in_comment_onerror_html() -> str:
    """RC review B1: `<head>` only ever appears inside an HTML comment, which used to capture
    Cod's CSP and bootstrap injection point; the onerror handler then tries to forge a submit.
    """
    return (
        "<!-- <head> -->"
        "<body>"
        "<img src=\"data:,\" onerror=\"window.cod.call('submit',{value:'forged'})\">"
        "</body>"
    )


def subframe_bridge_forge_html() -> str:
    """RC review B2: an agent page (no --js) with a same-origin `srcdoc` iframe that logs a
    `cod:bridge:` console message of its own. Console messages carry no frame identity, so
    without `frame-src 'none'` this could forge a submit from inside the subframe.
    """
    inner = (
        "<script>console.log(\"cod:bridge:\" + JSON.stringify("
        "{name:\"submit\",args:{value:\"forged-from-iframe\"}}));</script>"
    )
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>"
        f"<iframe srcdoc='{inner}'></iframe>"
        "</body></html>"
    )


def forged_onerror_html() -> str:
    """RC review B1: an inline event handler, not a `<script>` element, that tries to forge a
    submit through the bridge. Stripping `<script>` blocks alone does not stop it; only blocking
    page-authored JavaScript itself (CSP or an engine-level switch) does.
    """
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>"
        "<img src=\"data:,\" onerror=\"window.cod.call('submit',{value:'forged'})\">"
        "</body></html>"
    )


def navigate_away_html(target_url: str) -> str:
    """RC review B2: a page whose whole body is a plain link (no `data-cod`) to another
    document. Clicking it is an ordinary same-tab navigation, the kind any agent-made page might
    legitimately contain.
    """
    return (
        "<!doctype html><html><head><meta charset=\"utf-8\"></head>"
        '<body style="margin:0">'
        f'<a id="away" href="{target_url}" '
        'style="display:block;position:absolute;top:0;left:0;width:100%;height:100%;">'
        "leave"
        "</a>"
        "</body></html>"
    )


class ForgedBridgeServer:
    """An external page (RC review B2) that answers its own load by emitting a bridge-shaped
    console message, to check that a document Cod never built cannot forge a submit just by
    being shown in a tab that used to be an agent page.
    """

    def __init__(self) -> None:
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802 - the http.server name
                body = (
                    "<!doctype html><html><body>"
                    "<script>console.log('cod:bridge:'+JSON.stringify("
                    "{name:'submit',args:{value:'forged'}}));</script>"
                    "</body></html>"
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, format: str, *args: object) -> None:  # noqa: A002
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/forged"

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()


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

    def start_on_billing_api(self, persistent: bool = False) -> Path:
        repo = self.make_repo("billing-api", BILLING_API)
        self.start_cod(repo, persistent=persistent)
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

    def write_ask_html(self, session_id: str = SESSION_ID, open_file_path: str = "src/invoice.rs:18") -> Path:
        folder = self.workdir / "agent-pages"
        folder.mkdir(exist_ok=True)
        path = folder / "ask.html"
        path.write_text(ask_html(session_id, open_file_path))
        return path

    def write_html(self, name: str, content: str) -> Path:
        folder = self.workdir / "agent-pages"
        folder.mkdir(exist_ok=True)
        path = folder / name
        path.write_text(content)
        return path

    def open_agent_page(self, repo: Path, *extra: str, open_file_path: str = "src/invoice.rs:18") -> str:
        """`codctl open --html ask.html [extra...]`, which must print the page id and exit 0 at once (#43 AC1)."""
        html_path = self.write_ask_html(open_file_path=open_file_path)
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
        # Without a terminal, Zed's logger writes to the data folder instead of stdout.
        paths = (self.cod.preview_dir / "logs" / "Zed.log", self.cod.output_path)
        return "\n".join(path.read_text(errors="replace") for path in paths if path.exists())

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

        with self.step("open ask.html and focus the page before testing its actions"):
            page_id = self.open_agent_page(repo)
            self.wait_for(
                lambda state: (page_by_id(state, page_id) or {}).get("focused") is True,
                SHOW_TIMEOUT,
                "the agent page to have the focus",
            )

        with self.step("activate the button whose action is not in the allowlist: nothing happens, and it is logged as refused"):
            # Focus is already in the page: the click's normal focus change must
            # not be confused with an effect of the refused bridge action.
            before = self.state()
            self.click_in_page(page_id, 50, 135)
            time.sleep(SETTLE_TIME)
            after = self.state()
            self.expect_equal("active tab unchanged", before["active_tab"], after["active_tab"])
            self.expect_equal("tabs unchanged", before["tabs"], after["tabs"])
            self.expect_equal("chat unchanged", chat(before).get("session_id"), chat(after).get("session_id"))
            self.expect_equal("agent panel focus unchanged", before.get("agent_panel_focused"), after.get("agent_panel_focused"))
            log_text = self.output_log()
            if "refused the action" not in log_text or "erase-everything" not in log_text:
                raise AssertionError(f"expected the refused action logged; last 4000 chars of the log: {log_text[-4000:]!r}")

        with self.step("activate the open-file link: src/invoice.rs opens at line 18"):
            # The open-file link is the absolutely positioned box at left 0, top 40.
            self.click_in_page(page_id, 50, 55)
            self.wait_for_values(
                "editor",
                {"active_tab": "invoice.rs", "cursor": {"row": 18, "column": 1}},
                lambda state: {"active_tab": state["active_tab"], "cursor": state["cursor"]},
                SHOW_TIMEOUT,
            )

        with self.step("return to the page tab from the keyboard after opening invoice.rs"):
            # The file opens in the active pane (its own doc comment), so the page and the file
            # are always two items of the same pane here: "pane: activate previous item" is
            # enough, real keyboard-only navigation that needs no modal. The global tab switcher
            # would also work in a real session, but its own confirm relies on a blur-driven
            # dismiss that stock Zed gates on the platform's window-active flag (same in plain
            # Zed 1.23.2, see crates/picker/src/picker.rs on_input_editor_event), which the
            # headless e2e compositor never raises (no input devices to assign it): the switcher
            # would then leave a dismissed-but-still-registered modal behind that swallows the
            # next click, a harness artifact rather than anything a real keyboard or mouse user
            # hits.
            self.run_command("pane: activate previous item")
            self.wait_for(
                lambda state: (page_by_id(state, page_id) or {}).get("active") is True
                and (page_by_id(state, page_id) or {}).get("focused") is True
                and (page_by_id(state, page_id) or {}).get("hidden") is False,
                SHOW_TIMEOUT,
                "the original page tab to be active, visible and focused again",
            )

        with self.step("activate focus-session: the agent panel and that chat are focused again"):
            self.expect_equal("agent panel not focused before focus-session", False, self.state().get("agent_panel_focused") is True)
            self.click_in_page(page_id, 50, 95)
            self.wait_for(
                lambda state: state.get("agent_panel_focused") is True and chat(state).get("session_id") == SESSION_ID,
                SHOW_TIMEOUT,
                "focus-session to focus the chat",
            )

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

    # RC review B1 (security): an inline event handler must not run without --js.

    def test_b1_inline_event_handlers_do_not_run_without_js(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open a page whose onerror handler tries to forge a submit, without --js"):
            html_path = self.write_html("forged.html", forged_onerror_html())
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                time.sleep(SETTLE_TIME)
                self.expect_equal(
                    "codctl open --html --wait exited on its own (forged onerror submit, no --js)",
                    None,
                    process.poll(),
                )
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    # RC review B2 (security): a same-tab navigation away from the agent document must not keep
    # bridge privileges.

    def test_b2_bridge_messages_after_navigating_away_are_refused(self) -> None:
        with self.step("start Cod on billing-api and open a page that links to an outside page"):
            repo = self.start_on_billing_api()
            forged = ForgedBridgeServer()
            self.addCleanup(forged.close)
            html_path = self.write_html("navigate-away.html", navigate_away_html(forged.url()))
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                page_id = None
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

                with self.step("follow the same-tab link to the outside page, which tries to forge a submit"):
                    # The link fills the whole page.
                    self.click_in_page(page_id, 20, 20)
                    time.sleep(SETTLE_TIME)

                with self.step("the forged submit from the outside page never answers --wait"):
                    self.expect_equal(
                        "codctl open --html --wait exited on its own (forged submit after navigating away)",
                        None,
                        process.poll(),
                    )
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    # RC review new defect #1 (regression): a page over about 6 KiB must not trip the
    # navigated-away latch on its own first load, just because the engine truncates the address
    # it reports.

    def test_rc1_a_large_agent_page_still_answers_wait(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open --html --wait on a page over 10 KiB, and submit by a real mouse click"):
            html_path = self.write_html("large.html", large_ask_html())
            fixture_size = len(html_path.read_text())
            if fixture_size <= 10 * 1024:
                raise AssertionError(f"the large-page fixture is only {fixture_size} bytes, not over 10 KiB")
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                page_id = None
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

                # The Send button sits at left 230, top 0 (large_ask_html's own box layout).
                self.click_in_page(page_id, 240, 10)
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()

            self.expect_equal("codctl open --html --wait exit code (large page)", 0, process.returncode)
            self.expect_equal("codctl open --html --wait stdout (large page)", "half-even\n", stdout)
            self.expect_equal("codctl open --html --wait stderr (large page)", "", stderr)

    # RC review B5 (data loss): a bridge submission well over the old 4096-byte diagnostic cap
    # must still answer --wait with the whole value, not lose it to silent truncation.

    def test_b5_a_submit_well_over_4kib_still_answers_wait_with_the_full_value(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open --html --wait on a page whose field holds 20000 characters, and submit by a real click"):
            length = 20_000
            html_path = self.write_html("long-value.html", long_value_ask_html(length))
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                page_id = None
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

                # The Send button sits at left 230, top 0 (long_value_ask_html's own box layout).
                self.click_in_page(page_id, 240, 10)
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()

            self.expect_equal("codctl open --html --wait exit code (long value)", 0, process.returncode)
            self.expect_equal(
                "codctl open --html --wait stdout (long value)", "v" * length + "\n", stdout
            )
            self.expect_equal("codctl open --html --wait stderr (long value)", "", stderr)

    # RC review B5 (data loss, defensive): a bridge submission beyond even the new, much larger
    # cap must still give codctl a clear, bounded outcome instead of leaving --wait blocked
    # forever on a value it will never receive.

    def test_b5_a_submit_beyond_the_bridge_cap_closes_instead_of_hanging_forever(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open --html --wait on a page whose field holds 200000 characters, and submit by a real click"):
            html_path = self.write_html("oversized-value.html", long_value_ask_html(200_000))
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                page_id = None
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

                self.click_in_page(page_id, 240, 10)
                stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()

            self.expect_equal(
                "codctl open --html --wait exit code (value beyond the bridge cap, closed instead of hanging)",
                2,
                process.returncode,
            )
            self.expect_equal(
                "codctl open --html --wait stdout (value beyond the bridge cap)", "", stdout
            )

    # RC review B3 (data loss): an agent page whose document is over about 6 KiB must restore
    # intact after a restart, not corrupted by the engine's address-diagnostic truncation.

    def test_b3_a_large_agent_page_restores_intact_after_a_restart(self) -> None:
        with self.step("start Cod with a saved session and open a page over 10 KiB"):
            repo = self.start_on_billing_api(persistent=True)
            html_path = self.write_html("large-restart.html", large_ask_html())
            result = self.codctl("open", "--html", str(html_path), cwd=repo)
            self.expect_result(result, 0, stderr="")
            page_id = result.stdout.strip()
            self.wait_for(
                lambda state: (page_by_id(state, page_id) or {}).get("state") == "loaded",
                LOAD_TIMEOUT,
                "the large agent page to load",
            )

        with self.step("restart Cod"):
            self.restart_cod()

        with self.step("the restored page loads again, intact, and its bridge still works"):
            state = self.wait_for(
                lambda state: len(pages(state)) == 1 and pages(state)[0].get("state") == "loaded",
                RESTORE_TIMEOUT,
                "the large agent page to come back and load",
            )
            restored_id = pages(state)[0]["id"]
            # The Send button sits at left 230, top 0 (large_ask_html's own box layout); if the
            # restored document were truncated mid-markup, this button would not be there to
            # click, and the page would never close.
            self.click_in_page(restored_id, 240, 10)
            self.wait_for(
                lambda state: page_by_id(state, restored_id) is None,
                SHOW_TIMEOUT,
                "the restored page to close once its intact bridge submits",
            )

    # RC review B1 (security): a `<head>` tag hidden inside an HTML comment must not capture
    # Cod's CSP and bootstrap injection point.

    def test_rc_b1_head_inside_a_comment_does_not_bypass_the_csp(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open a page whose only <head> is inside a comment, without --js"):
            html_path = self.write_html("head-in-comment.html", head_in_comment_onerror_html())
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                time.sleep(SETTLE_TIME)
                self.expect_equal(
                    "codctl open --html --wait exited on its own (head in comment, forged onerror, no --js)",
                    None,
                    process.poll(),
                )
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    # RC review B2 (security): a subframe must not be able to forge a bridge call, even without
    # --js.

    def test_rc_b2_an_iframe_cannot_forge_a_bridge_submit_without_js(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open a page with a same-origin srcdoc iframe that logs a forged cod:bridge message"):
            html_path = self.write_html("subframe-forge.html", subframe_bridge_forge_html())
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", cwd=repo)
            try:
                time.sleep(SETTLE_TIME)
                self.expect_equal(
                    "codctl open --html --wait exited on its own (forged submit from an iframe, no --js)",
                    None,
                    process.poll(),
                )
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    # RC review B2 (security), for --js pages: without --js, frame-src 'none' keeps a page from
    # embedding any subframe at all. With --js there is no such CSP, so an iframe can load and
    # run its own script; the engine's console source is what must tell it apart from the main
    # document's bootstrap.

    def test_rc_b2_js_an_iframe_cannot_forge_a_bridge_submit_with_js(self) -> None:
        with self.step("start Cod on billing-api"):
            repo = self.start_on_billing_api()

        with self.step("open --js a page with a same-origin srcdoc iframe that logs a forged cod:bridge message"):
            html_path = self.write_html("subframe-forge-js.html", subframe_bridge_forge_html())
            process = self.codctl_popen("open", "--html", str(html_path), "--wait", "--js", cwd=repo)
            try:
                time.sleep(SETTLE_TIME)
                self.expect_equal(
                    "codctl open --html --wait exited on its own (forged submit from an iframe, with --js)",
                    None,
                    process.poll(),
                )
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate()

    # RC review B4 (stability): an oversized --html document is refused, not sent to the helper.

    def test_b4_oversized_html_is_refused_without_breaking_other_pages(self) -> None:
        with self.step("start Cod on billing-api and open a normal agent page first"):
            repo = self.start_on_billing_api()
            healthy_id = self.open_agent_page(repo)

        with self.step("a too-large --html document is refused with a clear error, not a crash"):
            filler = "x" * (900 * 1024)
            big_path = self.write_html("too-big.html", f"<!doctype html><body><!-- {filler} --></body>")
            result = self.codctl("open", "--html", str(big_path), cwd=repo)
            self.expect_equal("exit code of an oversized --html document", 1, result.returncode)
            self.expect_equal("stdout of an oversized --html document", "", result.stdout)
            if "large" not in result.stderr:
                raise AssertionError(f"expected a clear size error, got stderr {result.stderr!r}")

        with self.step("the earlier page tab is unaffected"):
            self.wait_for(
                lambda state: (page_by_id(state, healthy_id) or {}).get("state") == "loaded",
                SHOW_TIMEOUT,
                "the earlier agent page to still be loaded",
            )

    # RC review B6 (security): a symlink inside the workspace that points outside it must not let
    # an agent page open the file it points to.

    def test_b6_symlink_escape_is_refused(self) -> None:
        with self.step("create a repo with a symlink pointing at a secret file outside it"):
            repo = self.make_repo("billing-api", BILLING_API)
            secret = self.workdir / "outside-secret.txt"
            secret.write_text("outside secret content, never meant to be opened from a page\n")
            (repo / "outside-link.txt").symlink_to(secret)
            self.start_cod(repo)
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

        with self.step("open ask.html whose open-file action points at the symlink, and focus it"):
            page_id = self.open_agent_page(repo, open_file_path="outside-link.txt")
            self.wait_for(
                lambda state: (page_by_id(state, page_id) or {}).get("focused") is True,
                SHOW_TIMEOUT,
                "the agent page to have the focus",
            )

        with self.step("activate the open-file link: the outside file is refused, not opened"):
            before = self.state()
            # The open-file link is the absolutely positioned box at left 0, top 40.
            self.click_in_page(page_id, 50, 55)
            time.sleep(SETTLE_TIME)
            after = self.state()
            self.expect_equal("active tab unchanged", before["active_tab"], after["active_tab"])
            self.expect_equal("tabs unchanged", before["tabs"], after["tabs"])
            log_text = self.output_log()
            if "refused" not in log_text or "outside-link.txt" not in log_text:
                raise AssertionError(f"expected the symlink escape refused and logged; last 4000 chars: {log_text[-4000:]!r}")


if __name__ == "__main__":
    unittest.main()
