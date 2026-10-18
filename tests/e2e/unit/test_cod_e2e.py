"""Unit tests of cod_e2e that need no running Cod: messages, the wire format, the client, fixture repos and the start command."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

import cod_e2e


class MessagesTest(unittest.TestCase):
    def test_failure_message_names_expected_and_actual(self) -> None:
        self.assertEqual(
            cod_e2e.failure_message("active tab", "ledger.rs", "invoice.rs"),
            "expected active tab 'ledger.rs', actual 'invoice.rs'",
        )

    def test_failure_message_shows_a_missing_value(self) -> None:
        self.assertEqual(cod_e2e.failure_message("active tab", "invoice.rs", None), "expected active tab 'invoice.rs', actual None")

    def test_expect_equal(self) -> None:
        cod_e2e.expect_equal("active tab", "invoice.rs", "invoice.rs")
        with self.assertRaises(AssertionError) as caught:
            cod_e2e.expect_equal("active tab", "ledger.rs", "invoice.rs")
        self.assertEqual(str(caught.exception), "expected active tab 'ledger.rs', actual 'invoice.rs'")

    def test_step_logs_the_failed_step_and_reraises(self) -> None:
        collected = []
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(AssertionError):
            with cod_e2e.step("press enter", lambda description, error: collected.append((description, str(error)))):
                raise AssertionError("expected active tab 'ledger.rs', actual 'invoice.rs'")
        self.assertEqual(
            stderr.getvalue().splitlines(),
            [
                "STEP: press enter",
                "FAILED STEP: press enter",
                "  AssertionError: expected active tab 'ledger.rs', actual 'invoice.rs'",
            ],
        )
        self.assertEqual(collected, [("press enter", "expected active tab 'ledger.rs', actual 'invoice.rs'")])

    def test_step_without_failure_logs_only_the_step(self) -> None:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with cod_e2e.step("press ctrl-p", lambda description, error: self.fail("no failure expected")):
                pass
        self.assertEqual(stderr.getvalue(), "STEP: press ctrl-p\n")

    def test_artifact_name(self) -> None:
        self.assertEqual(
            cod_e2e.artifact_name("test_22_open_file.OpenFileTest.test_open"), "test_22_open_file.OpenFileTest.test_open"
        )
        self.assertEqual(cod_e2e.artifact_name("a b/c"), "a_b_c")


class WireFormatTest(unittest.TestCase):
    def test_requests_are_one_json_line(self) -> None:
        self.assertEqual(cod_e2e.encode_request("keys", keys="ctrl-p"), b'{"cmd": "keys", "keys": "ctrl-p"}\n')
        self.assertEqual(json.loads(cod_e2e.encode_request("type", text="in\nvoice")), {"cmd": "type", "text": "in\nvoice"})
        self.assertEqual(cod_e2e.encode_request("type", text="in\nvoice").count(b"\n"), 1)
        self.assertEqual(cod_e2e.encode_request("state"), b'{"cmd": "state"}\n')

    def test_empty_keys_and_text_are_refused(self) -> None:
        for fields in ({"keys": ""}, {"keys": "  "}, {}):
            with self.assertRaises(ValueError):
                cod_e2e.encode_request("keys", **fields)
        with self.assertRaises(ValueError):
            cod_e2e.encode_request("type", text="")

    def test_replies(self) -> None:
        self.assertEqual(cod_e2e.decode_reply(b'{"ok": true}\n'), {"ok": True})
        for line, message in (
            (b"", "closed"),
            (b"not json\n", "invalid JSON"),
            (b"[1]\n", "not an object"),
            (b'{"ok": false, "error": "unknown keystroke ctrl-?"}\n', "unknown keystroke ctrl-?"),
        ):
            with self.assertRaises(cod_e2e.DriverError) as caught:
                cod_e2e.decode_reply(line)
            self.assertIn(message, str(caught.exception))


class FakeDriver:
    """A Unix socket server that answers like Cod's driver and records the requests."""

    def __init__(self, path: Path) -> None:
        self.requests: list[dict] = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(str(path))
        self.server.listen(1)
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self) -> None:
        connection, _ = self.server.accept()
        with connection, connection.makefile("rwb") as stream:
            for line in stream:
                request = json.loads(line)
                self.requests.append(request)
                if request["cmd"] == "state":
                    reply = {"ok": True, "state": {"active_tab": None, "tabs": [], "worktrees": ["/tmp/billing-api"], "modal": None}}
                elif request["cmd"] in ("keys", "type"):
                    reply = {"ok": True}
                else:
                    reply = {"ok": False, "error": f"unknown command {request['cmd']}"}
                stream.write((json.dumps(reply) + "\n").encode())
                stream.flush()

    def close(self) -> None:
        self.server.close()


class DriverClientTest(unittest.TestCase):
    def test_client_speaks_the_protocol(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cod-e2e-unit-") as folder:
            path = Path(folder) / "cod.sock"
            fake = FakeDriver(path)
            self.addCleanup(fake.close)
            driver = cod_e2e.Driver(path, timeout=5)
            self.addCleanup(driver.close)
            driver.keys("ctrl-p")
            driver.type("invoice")
            state = driver.state()
            with self.assertRaises(cod_e2e.DriverError) as caught:
                driver.request("click")
            self.assertEqual(state["worktrees"], ["/tmp/billing-api"])
            self.assertIn("unknown command click", str(caught.exception))
            self.assertEqual(
                fake.requests,
                [{"cmd": "keys", "keys": "ctrl-p"}, {"cmd": "type", "text": "invoice"}, {"cmd": "state"}, {"cmd": "click"}],
            )


class FixtureRepoTest(unittest.TestCase):
    def test_make_repo_commits_the_files_with_an_invented_author(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cod-e2e-unit-") as folder:
            repo = cod_e2e.make_repo(Path(folder), "billing-api", {"src/invoice.rs": "fn main() {}\n", "README.md": "# Billing\n"})
            self.assertEqual((repo / "src" / "invoice.rs").read_text(), "fn main() {}\n")

            def git(*args: str) -> str:
                env = cod_e2e.git_environment(os.environ)
                return subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True, text=True).stdout

            self.assertEqual(git("status", "--porcelain"), "")
            self.assertEqual(git("ls-files").split(), ["README.md", "src/invoice.rs"])
            self.assertEqual(git("log", "--format=%an <%ae>").strip(), "Cod Fixture <fixture@example.invalid>")

    def test_make_repo_refuses_paths_outside_the_repo(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cod-e2e-unit-") as folder:
            with self.assertRaises(ValueError):
                cod_e2e.make_repo(Path(folder), "billing-api", {"../escape.rs": ""})


class StartCommandTest(unittest.TestCase):
    def test_cod_starts_through_cod_wayland_with_no_start_page(self) -> None:
        self.assertEqual(
            cod_e2e.cod_command(Path("/run/cod"), Path("/tmp/billing-api")),
            ["dbus-run-session", "--", "/run/cod/cod-wayland.sh", "", "/tmp/billing-api"],
        )

    def test_cod_starts_without_a_folder_to_restore_its_session(self) -> None:
        self.assertEqual(
            cod_e2e.cod_command(Path("/run/cod"), None),
            ["dbus-run-session", "--", "/run/cod/cod-wayland.sh", ""],
        )

    def test_a_persistent_start_keeps_zed_state(self) -> None:
        base = {"PATH": "/usr/bin", "ZED_STATELESS": "1"}
        env = cod_e2e.cod_environment(base, Path("/tmp/data"), Path("/tmp/driver/cod.sock"), Path("/tmp/home"), persistent=True)
        self.assertNotIn("ZED_STATELESS", env)
        self.assertEqual(env["COD_PREVIEW_DIR"], "/tmp/data")
        self.assertEqual(env["COD_E2E_SOCKET"], "/tmp/driver/cod.sock")
        self.assertEqual(base["ZED_STATELESS"], "1")
        stateless = cod_e2e.cod_environment({"PATH": "/usr/bin"}, Path("/tmp/data"), Path("/tmp/driver/cod.sock"), Path("/tmp/home"))
        self.assertEqual(stateless["ZED_STATELESS"], "1")

    def test_environment(self) -> None:
        base = {
            "DISPLAY": ":0",
            "COD_SPIKE_URL": "https://example.invalid",
            "WAYLAND_DISPLAY": "wayland-1",
            "PATH": "/usr/bin",
            "HOME": "/home/someone",
            "XDG_CONFIG_HOME": "/home/someone/.config",
            "XDG_RUNTIME_DIR": "/tmp/runtime",
            "COD_CEF_DIR": "/opt/cef",
        }
        env = cod_e2e.cod_environment(base, Path("/tmp/data"), Path("/tmp/driver/cod.sock"), Path("/tmp/home"))
        self.assertEqual(env["HOME"], "/tmp/home")
        self.assertEqual(env["XDG_CONFIG_HOME"], "/tmp/home/.config")
        self.assertEqual(env["XDG_DATA_HOME"], "/tmp/home/.local/share")
        self.assertEqual(env["XDG_CACHE_HOME"], "/tmp/home/.cache")
        self.assertEqual(env["XDG_STATE_HOME"], "/tmp/home/.local/state")
        self.assertEqual(env["XDG_RUNTIME_DIR"], "/tmp/runtime")
        self.assertEqual(env["COD_CEF_DIR"], "/opt/cef")
        self.assertEqual(env["PATH"], "/usr/bin")
        self.assertEqual(base["HOME"], "/home/someone")
        self.assertNotIn("DISPLAY", env)
        self.assertNotIn("COD_SPIKE_URL", env)
        self.assertEqual(env["WAYLAND_DISPLAY"], "wayland-1")
        self.assertEqual(env["COD_PREVIEW_DIR"], "/tmp/data")
        self.assertEqual(env["COD_E2E_SOCKET"], "/tmp/driver/cod.sock")
        self.assertEqual(env["ZED_STATELESS"], "1")
        self.assertEqual(env["ZED_ALLOW_EMULATED_GPU"], "1")
        self.assertEqual(base["DISPLAY"], ":0")

    def test_settings_turn_off_unsolicited_network_work(self) -> None:
        settings = cod_e2e.cod_settings()
        self.assertEqual(settings["session"], {"trust_all_worktrees": True})
        # Zed installs the html extension on the first start unless this says false (assets/settings/default.json).
        self.assertEqual(settings["auto_install_extensions"], {"html": False})
        self.assertEqual(settings["auto_update_extensions"], {"html": False})
        self.assertIs(settings["auto_update"], False)
        self.assertEqual(settings["telemetry"], {"diagnostics": False, "metrics": False})
        self.assertEqual(json.loads(json.dumps(settings)), settings)

    def test_settings_use_zeds_own_pickers_and_a_palette_without_history(self) -> None:
        settings = cod_e2e.cod_settings()
        # The headless sway has no desktop portal: Zed's own path picker and prompts must render (assets/settings/default.json).
        self.assertIs(settings["use_system_path_prompts"], False)
        self.assertIs(settings["use_system_prompts"], False)
        # Without history the palette ranks by the query alone, which the driver needs to name the highlighted command.
        self.assertEqual(settings["command_palette"], {"use_command_history": False})


class PaletteTest(unittest.TestCase):
    def state(self, modal: str | None, selection: str | None, query: str = "workspace: new workspace", matches: int = 1) -> dict:
        return {
            "modal": modal,
            "palette_selection": selection,
            "palette": {"query": query, "match_count": matches, "selected_index": 0} if modal == "command_palette" else None,
        }

    def test_ready_when_the_palette_highlights_exactly_the_command(self) -> None:
        self.assertIsNone(cod_e2e.palette_problem("workspace: new workspace", self.state("command_palette", "workspace: new workspace")))

    def test_a_near_match_is_refused_with_expected_and_actual(self) -> None:
        problem = cod_e2e.palette_problem("workspace: new workspace", self.state("command_palette", "workspace: new window"))
        self.assertEqual(
            problem,
            "expected palette selection 'workspace: new workspace', actual 'workspace: new window' "
            "(query 'workspace: new workspace', 1 matches, highlighted row 0)",
        )

    def test_no_selection_and_a_closed_palette_are_refused(self) -> None:
        problem = cod_e2e.palette_problem("workspace: new workspace", self.state("command_palette", None, matches=0))
        self.assertTrue(problem.startswith("expected palette selection 'workspace: new workspace', actual None"), problem)
        self.assertEqual(
            cod_e2e.palette_problem("workspace: new workspace", self.state(None, None)), "expected modal 'command_palette', actual None"
        )


class StubDriver:
    """Answers like the driver from a list of states; records keys and typed text."""

    def __init__(self, states: list[dict]) -> None:
        self.states = states
        self.actions: list[tuple[str, str]] = []

    def keys(self, keys: str) -> None:
        self.actions.append(("keys", keys))

    def type(self, text: str) -> None:
        self.actions.append(("type", text))

    def state(self) -> dict:
        return dict(self.states.pop(0) if len(self.states) > 1 else self.states[0])


class RunCommandTest(unittest.TestCase):
    class Sample(cod_e2e.CodTestCase):
        def test_sample(self) -> None:
            pass

    def sample(self, states: list[dict]) -> tuple[cod_e2e.CodTestCase, StubDriver]:
        sample = self.Sample("test_sample")
        driver = StubDriver(states)
        sample.cod = mock.Mock(driver=driver)
        patcher = mock.patch.object(sample, "window_title", return_value=None)
        patcher.start()
        self.addCleanup(patcher.stop)
        return sample, driver

    def test_runs_the_command_once_the_palette_highlights_it(self) -> None:
        palette = {"modal": "command_palette", "palette_selection": None, "palette": None}
        ready = {"modal": "command_palette", "palette_selection": "workspace: new workspace", "palette": None}
        closed = {"modal": None, "palette_selection": None, "palette": None}
        sample, driver = self.sample([palette, palette, ready, closed])
        sample.run_command("workspace: new workspace", timeout=5)
        self.assertEqual(driver.actions, [("keys", "ctrl-shift-p"), ("type", "workspace: new workspace"), ("keys", "enter")])

    def test_never_presses_enter_on_another_command(self) -> None:
        near = {
            "modal": "command_palette",
            "palette_selection": "workspace: new window",
            "palette": {"query": "workspace: new workspace", "match_count": 1, "selected_index": 0},
        }
        sample, driver = self.sample([near])
        with self.assertRaises(AssertionError) as caught:
            sample.run_command("workspace: new workspace", timeout=0.3)
        self.assertIn("expected palette selection 'workspace: new workspace', actual 'workspace: new window'", str(caught.exception))
        self.assertNotIn(("keys", "enter"), driver.actions)


class RestartTest(unittest.TestCase):
    class Sample(cod_e2e.CodTestCase):
        def test_sample(self) -> None:
            pass

    def test_restart_needs_a_running_persistent_cod(self) -> None:
        sample = self.Sample("test_sample")
        sample.cod = None
        with self.assertRaises(RuntimeError) as caught:
            sample.restart_cod()
        self.assertIn("call start_cod first", str(caught.exception))
        sample.cod = mock.Mock(process=object(), persistent=False)
        with self.assertRaises(RuntimeError) as caught:
            sample.restart_cod()
        self.assertIn("persistent=True", str(caught.exception))
        sample.cod.quit.assert_not_called()

    def test_restart_quits_and_starts_again_without_a_folder(self) -> None:
        sample = self.Sample("test_sample")
        cod = mock.Mock(process=object(), persistent=True, folder=Path("/tmp/billing-api"))
        sample.cod = cod
        sample.restart_cod(timeout=7)
        self.assertEqual([call[0] for call in cod.method_calls], ["quit", "start", "connect"])
        self.assertIsNone(cod.folder)
        cod.connect.assert_called_once_with(7)


class SwayTest(unittest.TestCase):
    TREE = {
        "type": "root",
        "nodes": [
            {
                "type": "output",
                "nodes": [
                    {
                        "type": "workspace",
                        "nodes": [
                            {"type": "con", "pid": 41, "name": "billing-api - invoice.rs", "focused": False, "nodes": []},
                            {"type": "con", "pid": 99, "name": "someone else", "focused": True, "nodes": []},
                        ],
                        "floating_nodes": [{"type": "floating_con", "pid": 42, "name": "docs-site", "focused": True, "nodes": []}],
                    }
                ],
            }
        ],
    }

    def test_window_titles_of_the_cod_processes(self) -> None:
        self.assertEqual(
            cod_e2e.window_titles(self.TREE, {41, 42}), [("billing-api - invoice.rs", False), ("docs-site", True)]
        )
        self.assertEqual(cod_e2e.window_titles(self.TREE, {7}), [])

    def test_window_title_prefers_the_focused_window(self) -> None:
        self.assertEqual(cod_e2e.window_title([("a", False), ("b", True)]), "b")
        self.assertEqual(cod_e2e.window_title([("a", False)]), "a")
        self.assertIsNone(cod_e2e.window_title([("a", False), ("b", False)]))
        self.assertIsNone(cod_e2e.window_title([]))

    def test_sway_socket(self) -> None:
        with tempfile.TemporaryDirectory(prefix="cod-e2e-unit-") as folder:
            runtime = Path(folder)
            self.assertIsNone(cod_e2e.sway_socket(runtime))
            (runtime / "wayland-1").touch()
            (runtime / "sway-ipc.1000.4242.sock").touch()
            self.assertEqual(cod_e2e.sway_socket(runtime), runtime / "sway-ipc.1000.4242.sock")


class ArtifactsTest(unittest.TestCase):
    """The artifact folder of a test: failure.txt after a failed step, final.txt otherwise, never both."""

    class Sample(cod_e2e.CodTestCase):
        def test_sample(self) -> None:
            pass

    def run_sample(self, fail_step: bool) -> Path:
        folder = Path(tempfile.mkdtemp(prefix="cod-e2e-unit-"))
        self.addCleanup(shutil.rmtree, folder, ignore_errors=True)
        sample = self.Sample("test_sample")
        stderr = io.StringIO()
        shot = subprocess.CompletedProcess(["grim"], 0, "", "")
        with (
            mock.patch.dict(os.environ, {"COD_E2E_ARTIFACTS": str(folder)}),
            mock.patch.object(cod_e2e.subprocess, "run", return_value=shot) as run,
            contextlib.redirect_stderr(stderr),
        ):
            sample.setUp()
            if fail_step:
                with contextlib.suppress(AssertionError), sample.step("check the active tab"):
                    raise AssertionError("expected active tab 'ledger.rs', actual 'invoice.rs'")
            sample._save_final_artifacts()
            sample.doCleanups()
        self.assertEqual(run.call_args.args[0][0], "grim")
        return folder / cod_e2e.artifact_name(sample.id())

    def test_a_failed_step_saves_failure_txt_only(self) -> None:
        target = self.run_sample(fail_step=True)
        self.assertEqual(sorted(path.name for path in target.iterdir()), ["failure.txt"])
        self.assertIn("failed step: check the active tab", (target / "failure.txt").read_text())

    def test_a_passing_test_saves_final_txt(self) -> None:
        target = self.run_sample(fail_step=False)
        self.assertEqual(sorted(path.name for path in target.iterdir()), ["final.txt"])
        self.assertIn("no step failed", (target / "final.txt").read_text())


if __name__ == "__main__":
    unittest.main()
