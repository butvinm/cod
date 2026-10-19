"""Story #24: the basic set of remote (SSH) workspace tests, on a disposable sshd on 127.0.0.1 (tests/e2e/loopback_ssh.py).

These tests need the remote server binary, which takes long to build, so they are not in the required E2E job.
The name of this file does not match test_*.py, so `script/e2e` without names never runs it; the optional workflow "E2E remote (optional)" (.github/workflows/e2e-remote.yml) runs it with `script/e2e remote_24`.
The remote multi-workspace lifecycle cases are tracked apart in remote_tracking_24.py, which builds on the helpers here.
Each test is skipped with the reason when the service cannot run here (a failure with COD_E2E_REQUIRE_SSH=1, as in the remote workflow).
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
import unittest
from pathlib import Path
from typing import Any

import loopback_ssh
from cod_e2e import Cod, cod_command, cod_environment
from test_24_workspaces import (
    BILLING_API,
    LEDGER_REMOTE,
    NEXT_WORKSPACE,
    REMOTE_TIMEOUT,
    SETTLE_TIME,
    SHOW_TIMEOUT,
    WorkspacesTest,
    layout,
    real,
    real_paths,
    workspace_list,
)


class RemoteWorkspacesTest(WorkspacesTest):
    """Uses the helpers of WorkspacesTest; load_tests below runs only the tests defined here, never the inherited ones."""

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
        self.expect_remote_flags(state, [remote])
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
        """By the driver's "remote" field of each workspace: the workspaces showing a folder in `remote` are remote and the others are local."""
        workspaces = state["workspaces"]
        self.expect_equal("workspaces without a remote field in the driver state", [], [w for w in workspaces if "remote" not in w])
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
            self.expect_remote_flags(state, [remote])

        with self.step("check the remote workspace: its folder over SSH and ledger.rs open"):
            self.show_workspace_of(remote, "ledger.rs")

        with self.step("check the local workspace: billing-api and invoice.rs open"):
            state = self.show_workspace_of(local, "invoice.rs")
            self.expect_each_once(state, [remote, local])
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_remote_flags(state, [remote])

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

    def open_from_command_line(self, url: str) -> None:
        """Opens the url in the running Cod as `cod <url>` does, and checks the command's exit status."""
        assert self.cod is not None
        env = cod_environment(os.environ, self.cod.preview_dir, self.cod.socket_path, self.cod.home, persistent=True)
        result = subprocess.run(cod_command(self.cod.run_dir, None) + [url], cwd=self.workdir, env=env, capture_output=True, timeout=60)
        self.expect_equal(f"exit status of cod {url}", 0, result.returncode)

    def test_open_an_ssh_project_at_start(self) -> None:
        """The basic SSH ability Cod inherits from Zed, as Cod ships today: `cod ssh://user@host:port/path` on a fresh start (no saved remote state) opens the remote folder, and a file there opens, takes an edit and saves to the remote host.

        Opening an ssh:// url in a Cod that already runs is not tested here: the Linux preview has no command-line hand-off yet (issue "Opening a path while Cod runs does nothing"). The second-port tests in remote_tracking_24.py cover that path.
        """
        with self.step("start a loopback sshd on 127.0.0.1 with a key made for this test, and make the repo"):
            ssh = self.loopback("open")
            remote = self.make_repo("ledger-remote", LEDGER_REMOTE)
            ledger = remote / "ledger.rs"
            ledger_before = ledger.read_text()

        with self.step("start Cod on ledger-remote over SSH (cod ssh://...), with the loopback host in ssh_connections"):
            self.start_cod_on_remote([ssh], ssh.url(remote))

        with self.step("check that the window shows ledger-remote over SSH, with no modal and no file open"):
            state = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["modal"] is None,
                REMOTE_TIMEOUT,
                "the window with ledger-remote over SSH",
            )
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("workspace count", 1, state["workspace_count"])
            self.expect_equal("tabs", [], state["tabs"])
            self.expect_remote_flags(state, [remote])

        with self.step("open ledger.rs with the file finder and type an edit"):
            self.open_file("ledger.rs")
            self.type("// checked ")
            self.wait_for_values(
                "the edited ledger.rs",
                {"dirty_tabs": ["ledger.rs"], "unsaved_texts": [{"tab": "ledger.rs", "text": "// checked " + ledger_before}]},
                layout,
                SHOW_TIMEOUT,
            )

        with self.step("save with ctrl-s and check the file on the remote host"):
            self.keys("ctrl-s")
            state = self.wait_for(
                lambda state: state["dirty_tabs"] == [] and state["active_tab"] == "ledger.rs", REMOTE_TIMEOUT, "ledger.rs to be saved"
            )
            self.expect_equal("unsaved texts", [], state["unsaved_texts"])
            deadline = time.monotonic() + 10
            while ledger.read_text() != "// checked " + ledger_before and time.monotonic() < deadline:
                time.sleep(0.1)
            self.expect_equal("ledger.rs on the remote host", "// checked " + ledger_before, ledger.read_text())
            state = self.state()
            self.expect_equal("window count", 1, state["window_count"])
            self.expect_equal("folders of the SSH window", [real(remote)], real_paths(state["worktrees"]))
            self.expect_remote_flags(state, [remote])


def load_tests(loader: unittest.TestLoader, tests: unittest.TestSuite, pattern: str | None) -> unittest.TestSuite:
    """Only the tests defined in RemoteWorkspacesTest, never the ones it inherits from WorkspacesTest. -k still filters them."""
    own = set(vars(RemoteWorkspacesTest))
    names = [name for name in loader.getTestCaseNames(RemoteWorkspacesTest) if name in own]
    return unittest.TestSuite(RemoteWorkspacesTest(name) for name in names)


if __name__ == "__main__":
    unittest.main()
