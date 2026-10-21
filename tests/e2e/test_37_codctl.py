"""Story #37: the codctl command server and the workspace commands. One test per acceptance criterion (AC4 has one per command).

Each test starts Cod with a temp data folder and runs the shipped codctl binary as a separate process, the way an agent does through Bash. It checks what codctl prints and its exit code, and checks the window through the driver state (workspaces, names, folders, the active workspace, prompts).
Fixtures are the throwaway git repos ~/work/billing-api and ~/work/billing-web (plus ~/work/docs-site) in the test's temp HOME, with invented content.
"""

from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import time
import unittest
from pathlib import Path
from typing import Any

from cod_e2e import CodTestCase, make_repo
from cod_status_fixtures import CodStatusFixtures
from fake_acp_agent import fake_agent_settings

PAY_NAME = "[PAY-1234] (billing-api) Fix invoice rounding"
WEB_NAME = "[PAY-1234] (billing-web) Show rounded totals"

# How long a codctl command may take to finish, and a change to show in the window.
COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
# How long Cod may take to restore its last session after a restart.
RESTORE_TIMEOUT = 45.0
# How long a change that must not happen has to stay away before the test believes it.
SETTLE_TIME = 1.5

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n",
}
BILLING_WEB = {
    "package.json": '{\n  "name": "billing-web",\n  "version": "0.1.0"\n}\n',
    "src/invoices.ts": "export function rounded(cents: number): number {\n  return Math.round(cents / 100) * 100;\n}\n",
}
DOCS_SITE = {
    "README.md": "# docs-site\n\nInvented documentation of the billing services.\n",
}

COMMANDS = ["new", "list", "focus", "rename", "close", "add-folder"]


def real(path: str | Path) -> str:
    return os.path.realpath(path)


def count_threads_named(pid: int, name: str) -> int:
    """How many threads of the process `pid` are named `name` (RC review A6): Cod names the
    codctl server's per-client thread "codctl-client", so a disconnected `session wait` that
    still has its thread blocked in `answer.recv()` shows up here even though nothing in the
    driver state reflects it."""
    task_dir = Path(f"/proc/{pid}/task")
    count = 0
    for entry in task_dir.iterdir():
        try:
            comm = (entry / "comm").read_text().strip()
        except OSError:
            continue
        if comm == name:
            count += 1
    return count


class Codctl:
    """Runs the shipped codctl binary as a separate process with the test's HOME."""

    def __init__(self, binary: Path, home: Path) -> None:
        self.binary = binary
        self.home = home

    def environment(self, extra: dict[str, str] | None = None) -> dict[str, str]:
        env = dict(os.environ)
        for name in ("COD_WORKSPACE", "COD_SOCKET"):
            env.pop(name, None)
        env["HOME"] = str(self.home)
        env.update(extra or {})
        return env

    def run(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.binary), *args],
            env=self.environment(env),
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT,
            stdin=subprocess.DEVNULL,
        )

    def start(self, *args: str) -> subprocess.Popen[str]:
        """Starts a command that waits for the user (close with a save prompt)."""
        return subprocess.Popen(
            [str(self.binary), *args],
            env=self.environment(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            stdin=subprocess.DEVNULL,
        )


class CodctlTest(CodStatusFixtures, CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.home = self.workdir / "home"
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        self.run_dir = Path(run_dir)
        self.codctl = Codctl(self.run_dir / "codctl", self.home)

    # Fixtures and helpers

    def make_work_repo(self, name: str, files: dict[str, str]) -> Path:
        """Creates ~/work/<name> in the test's HOME as a real git repo."""
        return make_repo(self.home / "work", name, files)

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
        lines = result.stderr.splitlines()
        self.expect_equal(f"number of stderr lines of {result.args[1:]} ({result.stderr!r})", 1, len(lines))

    def list_json(self) -> list[dict[str, Any]]:
        result = self.codctl.run("workspace", "list", "--json")
        self.expect_result(result, 0)
        listed = json.loads(result.stdout)
        if not isinstance(listed, list):
            raise AssertionError(f"workspace list --json printed {result.stdout!r}, not a JSON array")
        return listed

    def wait_for_codctl(self) -> list[dict[str, Any]]:
        """Waits until codctl reaches Cod and Cod lists its first workspace."""
        deadline = time.monotonic() + COMMAND_TIMEOUT
        last = None
        while True:
            last = self.codctl.run("workspace", "list", "--json")
            if last.returncode == 0 and json.loads(last.stdout):
                return json.loads(last.stdout)
            if time.monotonic() > deadline:
                raise AssertionError(f"codctl did not reach Cod: exit {last.returncode}, stdout {last.stdout!r}, stderr {last.stderr!r}")
            time.sleep(0.2)

    def new_workspace(self, *args: str) -> int:
        result = self.codctl.run("workspace", "new", *args)
        self.expect_result(result, 0)
        if not re.fullmatch(r"\d+\n", result.stdout):
            raise AssertionError(f"workspace new printed {result.stdout!r}, not one workspace ID")
        return int(result.stdout)

    def window_view(self, state: dict[str, Any]) -> dict[str, Any]:
        """What the window shows of its workspaces: names, folders and the active one."""
        return {
            "names": [workspace["name"] for workspace in state["workspaces"]],
            "folders": [[real(path) for path in workspace["worktrees"]] for workspace in state["workspaces"]],
            "active": state["active_workspace"],
        }

    def wait_for_window(self, expected: dict[str, Any], timeout: float = SHOW_TIMEOUT) -> dict[str, Any]:
        return self.wait_for_values("window", expected, self.window_view, timeout)

    def expect_window_stays(self, expected: dict[str, Any]) -> None:
        """Checks that the window keeps showing `expected` for a while."""
        time.sleep(SETTLE_TIME)
        self.expect_equal("window", expected, self.window_view(self.state()))

    def start_on(self, repo: Path) -> int:
        """Starts Cod on the repo, waits for codctl and returns the ID of the first workspace."""
        self.start_cod(repo)
        self.wait_for(
            lambda state: [real(path) for path in state["worktrees"]] == [real(repo)],
            SHOW_TIMEOUT,
            f"the {repo.name} folder to open",
        )
        listed = self.wait_for_codctl()
        self.expect_equal("workspaces listed at the start", 1, len(listed))
        return int(listed[0]["id"])

    # AC1

    def test_ac1_socket_help_and_not_running(self) -> None:
        with self.step("check that codctl ships next to cod in the run folder (laid out like the preview archive)"):
            binary = self.run_dir / "codctl"
            self.expect_equal("codctl is an executable file next to cod", True, binary.is_file() and os.access(binary, os.X_OK))
            self.expect_equal("cod is in the same folder", True, (self.run_dir / "cod").is_file())

        with self.step("run codctl --help and check that it lists the commands"):
            result = self.codctl.run("--help")
            self.expect_result(result, 0, stderr="")
            for command in COMMANDS:
                self.expect_equal(f"--help lists workspace {command}", True, f"workspace {command}" in result.stdout)

        with self.step("create the fixture repo billing-api"):
            api = self.make_work_repo("billing-api", BILLING_API)

        with self.step("check that every command says Cod is not running, with exit code 1, before Cod starts"):
            for args in (
                ["workspace", "new", "--folder", str(api)],
                ["workspace", "list"],
                ["workspace", "list", "--json"],
                ["workspace", "focus", "1"],
                ["workspace", "rename", "1", PAY_NAME],
                ["workspace", "close", "1"],
                ["workspace", "add-folder", "1", str(api)],
            ):
                self.expect_result(self.codctl.run(*args), 1, stdout="", stderr="Cod is not running\n")

        with self.step("start Cod on billing-api and wait until codctl reaches it"):
            self.start_on(api)

        with self.step("check that the socket is in XDG_RUNTIME_DIR and only this user can open it"):
            runtime = os.environ.get("XDG_RUNTIME_DIR")
            if not runtime:
                raise AssertionError("XDG_RUNTIME_DIR is not set: run the end-to-end tests with script/e2e")
            folder = Path(runtime) / "cod"
            socket_path = folder / "codctl.sock"
            folder_stat = os.lstat(folder)
            socket_stat = os.lstat(socket_path)
            self.expect_equal("the socket is a socket", True, stat.S_ISSOCK(socket_stat.st_mode))
            self.expect_equal("the socket folder is a real folder", True, stat.S_ISDIR(folder_stat.st_mode))
            self.expect_equal("owner of the socket folder", os.getuid(), folder_stat.st_uid)
            self.expect_equal("owner of the socket", os.getuid(), socket_stat.st_uid)
            self.expect_equal("group and other access to the socket folder", "0", oct(folder_stat.st_mode & 0o077)[2:])
            self.expect_equal("group and other access to the socket", "0", oct(socket_stat.st_mode & 0o077)[2:])

    def test_a6_a_disconnected_session_wait_does_not_leak_a_server_thread(self) -> None:
        """RC review A6: a client that disconnects during `session wait` (no --timeout) must
        not leave the server's client thread blocked in answer.recv() forever, nor its
        foreground poll task running for nobody. A session that never reaches needs-input (the
        fake agent's script has no turns) makes the wait block exactly like a real, slow agent
        would, until the client goes away."""
        with self.step("start Cod with a fake claude agent and open a session that stays idle"):
            api = self.make_work_repo("billing-api", BILLING_API)
            script = self.workdir / "a6-agent.json"
            script.write_text(json.dumps({"session_ids": ["a6-session"], "turns": []}))
            agent_servers = {"claude": fake_agent_settings(script, log=self.workdir / "a6-agent.log")}
            self.start_cod(api, extra_settings={"agent_servers": agent_servers})
            self.wait_for(
                lambda state: real(api) in [real(path) for path in state["worktrees"]],
                SHOW_TIMEOUT,
                "the billing-api folder to open",
            )
            # The agent panel can take a moment to become ready after the window first shows;
            # retry instead of failing on that unrelated timing, as wait_for_codctl does for the
            # socket itself.
            deadline = time.monotonic() + COMMAND_TIMEOUT
            new_result = self.codctl.run("session", "new", "--agent", "claude")
            while new_result.returncode != 0 and time.monotonic() < deadline:
                time.sleep(0.2)
                new_result = self.codctl.run("session", "new", "--agent", "claude")
            self.expect_result(new_result, 0, stderr="")
            session_id = new_result.stdout.strip()
            assert session_id, f"codctl session new --agent claude printed nothing: {new_result!r}"
            assert self.cod is not None and self.cod.process is not None
            pid = self.cod.process.pid

        with self.step("baseline count of the server's own per-client threads"):
            baseline = count_threads_named(pid, "codctl-client")

        with self.step("start and disconnect several session wait calls that never resolve on their own"):
            for _ in range(5):
                waiting = self.codctl.start("session", "wait", session_id, "--until", "needs-input")
                time.sleep(0.3)
                waiting.kill()
                waiting.wait(timeout=COMMAND_TIMEOUT)

        with self.step("the disconnected waits do not leave their server threads blocked in answer.recv() forever"):
            self.wait_for(
                lambda _: count_threads_named(pid, "codctl-client") <= baseline,
                SHOW_TIMEOUT,
                "the disconnected session wait threads to exit instead of staying blocked",
            )

    # AC2

    def test_ac2_new_workspace_with_name_and_folder(self) -> None:
        with self.step("create the fixture repos billing-web and billing-api"):
            web = self.make_work_repo("billing-web", BILLING_WEB)
            api = self.make_work_repo("billing-api", BILLING_API)

        with self.step("start Cod on billing-web"):
            first = self.start_on(web)

        with self.step("check the starting window: one workspace with billing-web, active"):
            self.wait_for_window({"names": [None], "folders": [[real(web)]], "active": 0})

        with self.step(f"run codctl workspace new --name {PAY_NAME!r} --folder ~/work/billing-api"):
            new_id = self.new_workspace("--name", PAY_NAME, "--folder", str(api))
            self.expect_equal("the new ID differs from the first workspace's", True, new_id != first)

        with self.step("check that the window shows the new workspace with its name and folder, and billing-web keeps the focus"):
            self.wait_for_window({"names": [None, PAY_NAME], "folders": [[real(web)], [real(api)]], "active": 0})
            self.expect_window_stays({"names": [None, PAY_NAME], "folders": [[real(web)], [real(api)]], "active": 0})

        with self.step("check that workspace list knows the new workspace by the printed ID"):
            listed = self.list_json()
            self.expect_equal("listed IDs", [first, new_id], [workspace["id"] for workspace in listed])

        with self.step("run codctl workspace new --focus with no folder"):
            focused_id = self.new_workspace("--focus")

        with self.step("check that the third workspace is shown and active"):
            self.wait_for_window({"names": [None, PAY_NAME, None], "folders": [[real(web)], [real(api)], []], "active": 2})
            self.expect_equal("active listed ID", [focused_id], [workspace["id"] for workspace in self.list_json() if workspace["active"]])

    def test_a2_new_on_an_already_open_folder_does_not_duplicate_its_id(self) -> None:
        """RC review A2: `workspace new --folder X` while X is already open used to restore X's
        saved database row into a second, independent workspace entity, so both ended up sharing
        one id and persistence. It must instead find the one already open on that folder."""
        with self.step("create the fixture repos billing-web and billing-api, and start on billing-web"):
            web = self.make_work_repo("billing-web", BILLING_WEB)
            api = self.make_work_repo("billing-api", BILLING_API)
            first = self.start_on(web)

        with self.step("open billing-api as a new workspace"):
            api_id = self.new_workspace("--folder", str(api))
            self.wait_for_window({"names": [None, None], "folders": [[real(web)], [real(api)]], "active": 0})

        with self.step("workspace new --folder billing-api again returns the same id, not a third one"):
            again_id = self.new_workspace("--folder", str(api))
            self.expect_equal("the second call returns billing-api's existing id", api_id, again_id)

        with self.step("the window still shows exactly two workspaces, and list has no duplicate id"):
            self.expect_window_stays({"names": [None, None], "folders": [[real(web)], [real(api)]], "active": 0})
            listed_ids = [workspace["id"] for workspace in self.list_json()]
            self.expect_equal("still exactly two workspaces, no duplicate id", sorted([first, api_id]), sorted(listed_ids))

    def test_a2_concurrent_new_on_a_pending_saved_folder_returns_one_id(self) -> None:
        """RC review A2 (race): two concurrent `workspace new --folder X` calls for a folder set
        that is saved but not loaded yet must not both pass the live-workspace check and restore
        the same database row into two independent workspace entities."""
        with self.step("create the fixture repos billing-api and billing-web, and save a session with both"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)
            self.start_cod(api, persistent=True)
            self.wait_for_codctl()
            web_id = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step("restart Cod so billing-web is saved but not loaded yet"):
            self.restart_cod()
            self.wait_for(lambda state: state["workspace_count"] == 2, RESTORE_TIMEOUT, "both workspaces to come back")
            before_ids = sorted(workspace["id"] for workspace in self.list_json())
            pending = next(workspace for workspace in self.list_json() if workspace["id"] == web_id)
            self.expect_equal("billing-web is pending, not loaded, right after the restart", False, pending["loaded"])

        with self.step("fire two concurrent workspace new --folder billing-web calls"):
            first = self.codctl.start("workspace", "new", "--folder", str(web))
            second = self.codctl.start("workspace", "new", "--folder", str(web))
            first_out, first_err = first.communicate(timeout=COMMAND_TIMEOUT)
            second_out, second_err = second.communicate(timeout=COMMAND_TIMEOUT)
            self.expect_equal(f"first call exit code (stderr {first_err!r})", 0, first.returncode)
            self.expect_equal(f"second call exit code (stderr {second_err!r})", 0, second.returncode)
            first_id = int(first_out.strip())
            second_id = int(second_out.strip())
            self.expect_equal("first concurrent call returns billing-web's existing id", web_id, first_id)
            self.expect_equal("second concurrent call returns the same id as the first, not a new one", first_id, second_id)

        with self.step("still exactly two workspaces, no duplicate id, after both calls"):
            listed_ids = sorted(workspace["id"] for workspace in self.list_json())
            self.expect_equal("still exactly two workspaces total, no duplicate id", before_ids, listed_ids)

    # AC3

    def test_ac3_list_text_and_json(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api and add a workspace for billing-web"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step("run codctl workspace list and check one line per workspace with ID, status and name"):
            result = self.codctl.run("workspace", "list")
            self.expect_result(result, 0, stderr="")
            lines = result.stdout.splitlines()
            self.expect_equal("number of lines", 2, len(lines))
            columns = [line.split("\t") for line in lines]
            self.expect_equal("IDs", [str(first), str(second)], [column[0] for column in columns])
            self.expect_equal("statuses", ["idle", "idle"], [column[1] for column in columns])
            self.expect_equal("names of unnamed workspaces", ["-", "-"], [column[2] for column in columns])

        with self.step("run codctl workspace list --json and check id, name, status, folders and active"):
            listed = self.list_json()
            view = [
                {
                    "id": workspace["id"],
                    "name": workspace["name"],
                    "status": workspace["status"],
                    "folders": [real(path) for path in workspace["folders"]],
                    "active": workspace["active"],
                }
                for workspace in listed
            ]
            self.expect_equal(
                "listed workspaces",
                [
                    {"id": first, "name": None, "status": "idle", "folders": [real(api)], "active": True},
                    {"id": second, "name": None, "status": "idle", "folders": [real(web)], "active": False},
                ],
                view,
            )

    def test_ac3_list_includes_workspaces_not_loaded_yet(self) -> None:
        with self.step("create the fixture repos billing-api, billing-web and docs-site"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)
            docs = self.make_work_repo("docs-site", DOCS_SITE)

        with self.step("start Cod on billing-api, keeping its session, and add workspaces for billing-web and docs-site"):
            self.start_cod(api, persistent=True)
            self.wait_for_codctl()
            self.new_workspace("--folder", str(web))
            self.new_workspace("--folder", str(docs))
            self.wait_for_window({"names": [None, None, None], "folders": [[real(api)], [real(web)], [real(docs)]], "active": 0})
            before = [(workspace["id"], [real(path) for path in workspace["folders"]]) for workspace in self.list_json()]

        with self.step("quit Cod with ctrl-q and start it again"):
            self.restart_cod()
            self.wait_for(lambda state: state["workspace_count"] == 3, RESTORE_TIMEOUT, "the three workspaces to come back")

        with self.step("check that workspace list --json lists all three, loaded or not, with their IDs and folders"):
            listed = self.list_json()
            after = [(workspace["id"], [real(path) for path in workspace["folders"]]) for workspace in listed]
            self.expect_equal("listed workspaces after the restart", before, after)
            self.expect_equal("every entry tells whether it is loaded", True, all(isinstance(workspace.get("loaded"), bool) for workspace in listed))

    def test_a8_list_reports_the_workspaces_real_status_not_always_idle(self) -> None:
        """RC review A8: `workspace list --json` used to print the placeholder "idle"
        unconditionally, never the workspace's real status, even while an entry of its own
        needed input or had failed."""
        with self.step("create the fixture repos billing-api and billing-web, and start on both"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step("both workspaces start idle"):
            self.expect_equal(
                "statuses before any entry",
                {first: "idle", second: "idle"},
                {workspace["id"]: workspace["status"] for workspace in self.list_json()},
            )

        with self.step("give the first workspace a needs-input entry and the second a working one"):
            self.set_entry(0, "demo-session", "needs_input")
            self.set_entry(1, "demo-session", "working")

        with self.step("workspace list --json now reports each workspace's real status"):
            self.wait_for(
                lambda _: {workspace["id"]: workspace["status"] for workspace in self.list_json()}
                == {first: "needs_input", second: "working"},
                SHOW_TIMEOUT,
                "workspace list --json to report the real statuses",
            )

    # AC4

    def test_ac4_focus(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api and add a workspace for billing-web"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step(f"run codctl workspace focus {second} and check that the window shows billing-web"):
            self.expect_result(self.codctl.run("workspace", "focus", str(second)), 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 1})
            self.wait_for(lambda state: state["workspaces"][1]["drawn"] and not state["workspaces"][0]["drawn"], SHOW_TIMEOUT, "billing-web to be drawn")

        with self.step(f"run codctl workspace focus {first} and check that the window shows billing-api again"):
            self.expect_result(self.codctl.run("workspace", "focus", str(first)), 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

    def test_a4_cli_focus_does_not_lose_to_a_still_loading_pending_tabs_completion(self) -> None:
        """RC review A4: `workspace focus` must bump the window's activation intent exactly as a
        tab click does. Start loading pending billing-web, then `workspace focus` billing-api
        while it is still shown; billing-web's own completion must not switch away from
        billing-api afterwards."""
        with self.step("save a session with billing-api active and billing-web added, then restart so billing-web is pending"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)
            self.start_cod(api, persistent=True)
            self.wait_for_codctl()
            api_id = self.list_json()[0]["id"]
            web_id = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})
            self.restart_cod()
            self.wait_for(lambda state: state["workspace_count"] == 2, RESTORE_TIMEOUT, "both workspaces to come back")
            pending = next(workspace for workspace in self.list_json() if workspace["id"] == web_id)
            self.expect_equal("billing-web is pending right after the restart", False, pending["loaded"])

        with self.step("start loading billing-web without waiting for it, then focus billing-api before it finishes"):
            loading = self.codctl.start("workspace", "focus", str(web_id))
            self.addCleanup(lambda: loading.poll() is None and loading.kill())
            focus_result = self.codctl.run("workspace", "focus", str(api_id))
            self.expect_result(focus_result, 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step("billing-api stays shown even once billing-web finishes loading in the background"):
            self.expect_window_stays({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})
            loading_out, loading_err = loading.communicate(timeout=COMMAND_TIMEOUT)
            self.expect_equal(
                f"the background focus on billing-web itself still succeeded (stderr {loading_err!r})",
                0,
                loading.returncode,
            )

    def test_a4_new_folder_focus_does_not_lose_to_a_still_loading_pending_tabs_completion(self) -> None:
        """RC review A4 (final batch): `workspace new --folder <existing> --focus` must bump the
        window's activation intent exactly as `workspace focus` and a tab click do, not call
        `activate` directly. Make billing-web live, start loading pending docs-site in the
        background, then `workspace new --folder <billing-web> --focus` while docs-site is still
        loading; its completion must not switch away from billing-web afterwards."""
        with self.step("save a session with billing-api active, billing-web and docs-site added, then restart so both are pending"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)
            docs = self.make_work_repo("docs-site", DOCS_SITE)
            self.start_cod(api, persistent=True)
            self.wait_for_codctl()
            web_id = self.new_workspace("--folder", str(web))
            docs_id = self.new_workspace("--folder", str(docs))
            self.wait_for_window({"names": [None, None, None], "folders": [[real(api)], [real(web)], [real(docs)]], "active": 0})
            self.restart_cod()
            self.wait_for(lambda state: state["workspace_count"] == 3, RESTORE_TIMEOUT, "all three workspaces to come back")

        with self.step("load billing-web fully, leaving docs-site pending"):
            self.expect_result(self.codctl.run("workspace", "focus", str(web_id)), 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None, None], "folders": [[real(api)], [real(web)], [real(docs)]], "active": 1})
            pending = next(workspace for workspace in self.list_json() if workspace["id"] == docs_id)
            self.expect_equal("docs-site is still pending", False, pending["loaded"])

        with self.step("start loading docs-site without waiting, then refocus billing-web with workspace new --folder --focus"):
            loading = self.codctl.start("workspace", "focus", str(docs_id))
            self.addCleanup(lambda: loading.poll() is None and loading.kill())
            refocus_result = self.codctl.run("workspace", "new", "--folder", str(web), "--focus")
            self.expect_result(refocus_result, 0, stdout=f"{web_id}\n", stderr="")
            self.wait_for_window({"names": [None, None, None], "folders": [[real(api)], [real(web)], [real(docs)]], "active": 1})

        with self.step("billing-web stays shown even once docs-site finishes loading in the background"):
            self.expect_window_stays({"names": [None, None, None], "folders": [[real(api)], [real(web)], [real(docs)]], "active": 1})
            loading_out, loading_err = loading.communicate(timeout=COMMAND_TIMEOUT)
            self.expect_equal(
                f"the background focus on docs-site itself still succeeded (stderr {loading_err!r})",
                0,
                loading.returncode,
            )

    def test_ac4_rename(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api and add a workspace for billing-web"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step(f"run codctl workspace rename {first} {PAY_NAME!r} and check the name and the window title"):
            self.expect_result(self.codctl.run("workspace", "rename", str(first), PAY_NAME), 0, stdout="", stderr="")
            state = self.wait_for_window({"names": [PAY_NAME, None], "folders": [[real(api)], [real(web)]], "active": 0})
            self.wait_for(lambda state: PAY_NAME in (state.get("window_title") or ""), SHOW_TIMEOUT, "the window title to show the name")

        with self.step(f"run codctl workspace rename {second} by ID while it is not active, then rename it again by its name"):
            self.expect_result(self.codctl.run("workspace", "rename", str(second), WEB_NAME), 0, stdout="", stderr="")
            self.wait_for_window({"names": [PAY_NAME, WEB_NAME], "folders": [[real(api)], [real(web)]], "active": 0})
            self.expect_result(self.codctl.run("workspace", "rename", WEB_NAME, "billing-web"), 0, stdout="", stderr="")
            self.wait_for_window({"names": [PAY_NAME, "billing-web"], "folders": [[real(api)], [real(web)]], "active": 0})
            self.expect_equal("listed names", [PAY_NAME, "billing-web"], [workspace["name"] for workspace in self.list_json()])

    def test_ac4_add_folder(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api"):
            first = self.start_on(api)
            self.wait_for_window({"names": [None], "folders": [[real(api)]], "active": 0})

        with self.step(f"run codctl workspace add-folder {first} ~/work/billing-web"):
            self.expect_result(self.codctl.run("workspace", "add-folder", str(first), str(web)), 0, stdout="", stderr="")

        with self.step("check that the workspace shows both folders, as after workspace: add folder to project"):
            self.wait_for_window({"names": [None], "folders": [[real(api), real(web)]], "active": 0})
            self.wait_for(
                lambda state: state["project_panel"] is not None and sorted(state["project_panel"]["roots"]) == ["billing-api", "billing-web"],
                SHOW_TIMEOUT,
                "the project panel to list both folders",
            )
            listed = self.list_json()
            self.expect_equal("listed folders", [[real(api), real(web)]], [[real(path) for path in workspace["folders"]] for workspace in listed])

    # AC5

    def open_and_edit(self, query: str, tab: str) -> None:
        """Opens a file with the file finder and types one character into it, so it has unsaved changes."""
        self.keys("ctrl-p")
        self.wait_for(lambda state: state["modal"] == "file_finder" and state["file_finder_ready"], SHOW_TIMEOUT, "the file finder to open")
        self.type(query)
        self.wait_for(lambda state: state["modal"] == "file_finder", SHOW_TIMEOUT, "the file finder to keep the query")
        self.keys("enter")
        self.wait_for(lambda state: state["active_tab"] == tab and state["modal"] is None, SHOW_TIMEOUT, f"{tab} to open")
        self.type("x")
        self.wait_for(lambda state: state["dirty_tabs"] == [tab], SHOW_TIMEOUT, f"{tab} to have unsaved changes")

    def finish(self, process: subprocess.Popen[str]) -> tuple[int, str, str]:
        stdout, stderr = process.communicate(timeout=COMMAND_TIMEOUT)
        return process.returncode, stdout, stderr

    def test_ac5_close_asks_about_unsaved_files(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api, add billing-web and give it an unsaved change in invoices.ts"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web), "--focus")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 1})
            self.open_and_edit("invoices", "invoices.ts")

        with self.step(f"run codctl workspace close {second} and check that Cod asks about the unsaved file"):
            process = self.codctl.start("workspace", "close", str(second))
            state = self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt about the unsaved file")
            self.expect_equal("Cancel is an answer", True, "Cancel" in (state["prompt"]["answers"] or []))
            self.expect_equal("codctl still waits", None, process.poll())

        with self.step("answer the prompt with escape (Cancel) and check that codctl exits with 2 and the workspace stays"):
            self.keys("escape")
            code, stdout, stderr = self.finish(process)
            self.expect_equal(f"exit code (stdout {stdout!r}, stderr {stderr!r})", 2, code)
            self.wait_for(lambda state: state["prompt"] is None, SHOW_TIMEOUT, "the prompt to close")
            self.expect_window_stays({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 1})
            self.expect_equal("unsaved tabs", ["invoices.ts"], self.state()["dirty_tabs"])

        with self.step(f"run codctl workspace close {second} again and answer Save with enter"):
            process = self.codctl.start("workspace", "close", str(second))
            self.wait_for(lambda state: state["prompt"] is not None, SHOW_TIMEOUT, "a prompt about the unsaved file")
            self.keys("enter")
            code, stdout, stderr = self.finish(process)
            self.expect_equal(f"exit code (stdout {stdout!r}, stderr {stderr!r})", 0, code)

        with self.step("check that the workspace closed, billing-api is shown and the change was saved"):
            self.wait_for_window({"names": [None], "folders": [[real(api)]], "active": 0})
            self.expect_equal("listed IDs", [first], [workspace["id"] for workspace in self.list_json()])
            self.expect_equal("saved invoices.ts starts with the typed x", True, (web / "src" / "invoices.ts").read_text().startswith("x"))

    def test_ac5_close_force_drops_unsaved_changes(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api, add billing-web and give it an unsaved change in invoices.ts"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web), "--focus")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 1})
            self.open_and_edit("invoices", "invoices.ts")

        with self.step(f"run codctl workspace close {second} --force and check that it closes without a prompt"):
            self.expect_result(self.codctl.run("workspace", "close", str(second), "--force"), 0, stdout="", stderr="")
            state = self.wait_for_window({"names": [None], "folders": [[real(api)]], "active": 0})
            self.expect_equal("prompt", None, state["prompt"])
            self.expect_equal("listed IDs", [first], [workspace["id"] for workspace in self.list_json()])
            self.expect_equal("invoices.ts on disk", BILLING_WEB["src/invoices.ts"], (web / "src" / "invoices.ts").read_text())

    # AC6

    def test_ac6_unknown_workspace_or_bad_argument_changes_nothing(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api and add a workspace for billing-web"):
            first = self.start_on(api)
            self.new_workspace("--folder", str(web))
            start = {"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0}
            self.wait_for_window(start)
            listed_before = self.list_json()

        unknown = "987654321"
        missing = str(self.home / "work" / "no-such-folder")
        for args in (
            ["workspace", "focus", unknown],
            ["workspace", "rename", unknown, PAY_NAME],
            ["workspace", "close", unknown],
            ["workspace", "close", unknown, "--force"],
            ["workspace", "add-folder", unknown, str(web)],
            ["workspace", "focus", "no workspace has this name"],
            ["workspace", "add-folder", str(first), missing],
            ["workspace", "new", "--folder", missing],
            ["workspace", "new", "--bogus"],
            ["workspace", "rename", str(first), ""],
            ["workspace", "frobnicate"],
            ["workspace", "focus"],
        ):
            with self.step(f"run codctl {' '.join(args)!r} and check a one-line error with exit code 1"):
                self.expect_one_line_error(self.codctl.run(*args))

        with self.step("check that nothing changed in the window"):
            self.expect_window_stays(start)
            self.expect_equal("listed workspaces", listed_before, self.list_json())

    # AC7

    def test_ac7_cod_workspace_in_terminals_and_as_default(self) -> None:
        with self.step("create the fixture repos billing-api and billing-web"):
            api = self.make_work_repo("billing-api", BILLING_API)
            web = self.make_work_repo("billing-web", BILLING_WEB)

        with self.step("start Cod on billing-api and add a workspace for billing-web"):
            first = self.start_on(api)
            second = self.new_workspace("--folder", str(web))
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 0})

        with self.step("check that codctl without an ID acts on the workspace in COD_WORKSPACE"):
            self.expect_result(self.codctl.run("workspace", "focus", env={"COD_WORKSPACE": str(second)}), 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web)]], "active": 1})
            self.expect_result(self.codctl.run("workspace", "add-folder", str(api), env={"COD_WORKSPACE": str(second)}), 0, stdout="", stderr="")
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web), real(api)]], "active": 1})

        with self.step(f"focus workspace {first}, open a terminal there and write COD_WORKSPACE to a file"):
            self.expect_result(self.codctl.run("workspace", "focus", str(first)), 0)
            self.wait_for_window({"names": [None, None], "folders": [[real(api)], [real(web), real(api)]], "active": 0})
            out = self.workdir / "cod-workspace.txt"
            self.run_command("terminal panel: toggle focus")
            self.wait_for(
                lambda state: state["focus"] is not None and state["focus"]["kind"] in ("item", "in_item", "other"),
                SHOW_TIMEOUT,
                "the terminal to take the focus",
            )
            time.sleep(SETTLE_TIME)
            self.type(f"printf '%s' \"$COD_WORKSPACE\" > {out}\n")
            deadline = time.monotonic() + SHOW_TIMEOUT
            while not out.exists() and time.monotonic() < deadline:
                time.sleep(0.2)

        with self.step("check that the terminal's COD_WORKSPACE holds the ID of its workspace"):
            self.expect_equal("COD_WORKSPACE in the terminal", str(first), out.read_text() if out.exists() else None)


if __name__ == "__main__":
    unittest.main()
