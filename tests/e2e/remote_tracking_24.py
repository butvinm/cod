"""Remote SSH windows with Cod workspaces: lifecycle cases that are not in the 0.0.1 minimum (story #24, PM scope comment of 2026-10-19).

These tests are not a #24 gate. The name of this file does not match test_*.py, so `script/e2e` without names never runs it; CI runs it in its own step, "Remote multi-workspace tracking (not a #24 gate)", and keeps its artifacts apart.
Run it with `script/e2e remote_tracking_24`. The basic SSH tests that gate #24 stay in test_24_workspaces.py.
They use the loopback sshd of tests/e2e/loopback_ssh.py, as the basic SSH tests do.
With lazy restore a remote workspace that is not shown connects only when it is shown, so the restart tests switch to it before they expect its connection modal.
"""

from __future__ import annotations

import contextlib
import os
import subprocess
import unittest
from typing import Any

import test_24_workspaces as workspaces
from cod_e2e import cod_command, cod_environment
from test_24_workspaces import (
    DOCS_SITE,
    LEDGER_REMOTE,
    NEXT_WORKSPACE,
    REMOTE_SERVER_DELAY,
    REMOTE_TIMEOUT,
    RESTORE_TIMEOUT,
    SETTLE_TIME,
    SHOW_TIMEOUT,
    real,
    real_paths,
    workspace_list,
)


class RemoteWorkspacesTrackingTest(workspaces.WorkspacesTest):
    """Uses the helpers of WorkspacesTest; load_tests below runs only the tests defined here, never the inherited ones."""

    def show_restoring_remote_workspace(self) -> dict[str, Any]:
        """After a restart with the local workspace active: waits for both workspaces, then shows the remote one with "workspace: next workspace", so its connection starts."""
        state = self.wait_for(lambda state: state["workspace_count"] == 2, REMOTE_TIMEOUT, "both workspaces to come back")
        before = state["active_workspace"]
        self.run_command(NEXT_WORKSPACE)
        return self.wait_for(lambda state: state["active_workspace"] != before, SHOW_TIMEOUT, "the remote workspace to be shown")

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

        with self.step("show the remote workspace with next workspace, so its reconnection starts"):
            self.show_restoring_remote_workspace()

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
            self.expect_remote_flags(state, [remote])

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

        with self.step("show the remote workspace with next workspace, so its connection starts"):
            self.show_restoring_remote_workspace()

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

        with self.step("show the remote workspace with next workspace, so its connection starts"):
            self.show_restoring_remote_workspace()

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
            self.expect_remote_flags(fresh, [remote])

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
            state = self.wait_for(
                lambda state: real_paths(state["worktrees"]) == [real(remote)] and state["active_tab"] == "ledger.rs",
                REMOTE_TIMEOUT,
                "the first port's workspace with ledger.rs after restart",
            )
            self.expect_remote_flags(state, [remote])


def load_tests(loader: unittest.TestLoader, tests: unittest.TestSuite, pattern: str | None) -> unittest.TestSuite:
    """Only the tests defined in RemoteWorkspacesTrackingTest, never the ones it inherits from WorkspacesTest. -k still filters them."""
    own = set(vars(RemoteWorkspacesTrackingTest))
    names = [name for name in loader.getTestCaseNames(RemoteWorkspacesTrackingTest) if name in own]
    return unittest.TestSuite(RemoteWorkspacesTrackingTest(name) for name in names)


if __name__ == "__main__":
    unittest.main()
