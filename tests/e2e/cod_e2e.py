"""Library for Cod's end-to-end tests (issue #22).

A test starts the real Cod app through cod-wayland.sh, exactly as users start it, and drives it only through the end-to-end driver: a Unix socket that Cod opens when it is started with COD_E2E_SOCKET=<path>.
The protocol is one JSON object per line, one JSON reply per line:

    {"cmd": "keys", "keys": "ctrl-p"}    presses each space-separated keystroke (Zed keystroke syntax)
    {"cmd": "type", "text": "invoice"}   types the text, one key event per character
    {"cmd": "state"}                     {"active_tab": ..., "tabs": [...], "worktrees": [...], "modal": ..., ...} (all fields in README.md)

Every reply has "ok". A failed command replies {"ok": false, "error": "..."}.
Actions go through keys and typing only, never through private calls that a user cannot make. "state" only observes what the user sees.
CodTestCase.state() adds "window_title", the title the compositor shows for Cod's window, and "sway_windows", sway's ids of Cod's windows, both read from sway (the platform cannot read a Wayland title back).

Run the tests with script/e2e, which builds Cod, assembles a run folder like the release tarball and starts a private headless sway.
See tests/e2e/README.md.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from collections.abc import Callable, Iterator, Mapping
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]

# How long Cod may take from the start until its driver socket accepts connections.
DRIVER_START_TIMEOUT = 60.0
# How long one driver command may take before the client gives up.
COMMAND_TIMEOUT = 10.0
# How often wait_for reads the state.
POLL_INTERVAL = 0.1
# Unix socket paths are limited to 108 bytes on Linux, including the final zero byte.
MAX_SOCKET_PATH = 107
# How long Cod may take to exit after Ctrl-Q, and how long the processes it started may take to follow.
QUIT_TIMEOUT = 30.0
# How long stop() keeps killing processes of a test that left its process group before it fails. Cod starts helpers in sessions of their own (Zed's login shell that reads the environment, language servers), and one of them can be in the middle of exec, with an empty /proc/<pid>/environ, when the first scan runs.
STOP_LEFTOVER_TIMEOUT = 10.0
# The pause between the two scans that must both find no process before stop() reports the test clean.
STOP_SETTLE_INTERVAL = 0.2
# How long the command palette may take to highlight the typed command.
PALETTE_TIMEOUT = 10.0

# Invented identity for the commits of fixture repos.
FIXTURE_AUTHOR_NAME = "Cod Fixture"
FIXTURE_AUTHOR_EMAIL = "fixture@example.invalid"


def log(message: str) -> None:
    """Writes one line to stderr, where unittest writes its own output, so the order is kept."""
    print(message, file=sys.stderr, flush=True)


class DriverError(Exception):
    """The driver socket is missing, closed, or answered with an error."""


def failure_message(what: str, expected: Any, actual: Any) -> str:
    """The message of a failed check, for example: expected active tab 'ledger.rs', actual 'invoice.rs'."""
    return f"expected {what} {expected!r}, actual {actual!r}"


def expect_equal(what: str, expected: Any, actual: Any) -> None:
    """Fails with a message that names what was checked, the expected value and the actual value."""
    if expected != actual:
        raise AssertionError(failure_message(what, expected, actual))


def differences(what: str, expected: Mapping[str, Any], actual: Mapping[str, Any]) -> list[str]:
    """One failure message for each key of expected whose value differs in actual (a missing key counts as None), for example: expected workspace A split 'vertical', actual None."""
    return [failure_message(f"{what} {key}", value, actual.get(key)) for key, value in expected.items() if actual.get(key) != value]


@contextlib.contextmanager
def step(description: str, on_failure: Callable[[str, BaseException], None] | None = None) -> Iterator[None]:
    """Logs "STEP: <description>". On any failure inside it logs "FAILED STEP: <description>" and the error, calls on_failure, and re-raises."""
    log(f"STEP: {description}")
    try:
        yield
    except Exception as error:
        log(f"FAILED STEP: {description}")
        log(f"  {type(error).__name__}: {error}")
        if on_failure is not None:
            try:
                on_failure(description, error)
            except Exception as collect_error:  # The original failure matters more than a failed collection.
                log(f"  could not collect the failure artifacts: {collect_error}")
        raise


def encode_request(cmd: str, **fields: Any) -> bytes:
    """One driver request as a JSON line."""
    if cmd == "keys":
        keys = fields.get("keys")
        if not isinstance(keys, str) or not keys.split():
            raise ValueError(f"keys must name at least one keystroke, got {keys!r}")
    if cmd == "type":
        text = fields.get("text")
        if not isinstance(text, str) or text == "":
            raise ValueError(f"type needs a non-empty text, got {text!r}")
    return (json.dumps({"cmd": cmd, **fields}) + "\n").encode()


def decode_reply(line: bytes) -> dict[str, Any]:
    """Parses one driver reply. Raises DriverError for a closed connection, invalid JSON or an error reply."""
    if not line:
        raise DriverError("Cod closed the driver connection")
    try:
        reply = json.loads(line)
    except json.JSONDecodeError as error:
        raise DriverError(f"the driver sent invalid JSON: {line[:200]!r}") from error
    if not isinstance(reply, dict):
        raise DriverError(f"the driver sent a reply that is not an object: {reply!r}")
    if reply.get("ok") is not True:
        raise DriverError(f"the driver refused the command: {reply.get('error', reply)}")
    return reply


class Driver:
    """Client of the end-to-end driver socket."""

    def __init__(self, path: Path, timeout: float = COMMAND_TIMEOUT) -> None:
        self.path = path
        self._socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._socket.settimeout(timeout)
        try:
            self._socket.connect(str(path))
        except OSError:
            self._socket.close()
            raise
        self._file = self._socket.makefile("rwb")

    def close(self) -> None:
        with contextlib.suppress(OSError):
            self._file.close()
        with contextlib.suppress(OSError):
            self._socket.close()

    def request(self, cmd: str, **fields: Any) -> dict[str, Any]:
        data = encode_request(cmd, **fields)
        try:
            self._file.write(data)
            self._file.flush()
            line = self._file.readline()
        except socket.timeout as error:
            raise DriverError(f"the driver did not answer {data.decode().strip()} in time") from error
        except OSError as error:
            raise DriverError(f"the driver connection failed: {error}") from error
        return decode_reply(line)

    def keys(self, keys: str) -> None:
        """Presses each space-separated keystroke, for example "ctrl-p" or "escape ctrl-shift-p"."""
        self.request("keys", keys=keys)

    def type(self, text: str) -> None:
        """Types the text one character at a time, like a user."""
        self.request("type", text=text)

    def state(self) -> dict[str, Any]:
        reply = self.request("state")
        state = reply.get("state")
        if not isinstance(state, dict):
            raise DriverError(f"the state reply has no state object: {reply!r}")
        return state


def git_environment(base: Mapping[str, str]) -> dict[str, str]:
    """Environment for git in fixture repos: no user or system config, an invented author."""
    env = dict(base)
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME=FIXTURE_AUTHOR_NAME,
        GIT_AUTHOR_EMAIL=FIXTURE_AUTHOR_EMAIL,
        GIT_COMMITTER_NAME=FIXTURE_AUTHOR_NAME,
        GIT_COMMITTER_EMAIL=FIXTURE_AUTHOR_EMAIL,
        GIT_AUTHOR_DATE="2026-01-01T12:00:00Z",
        GIT_COMMITTER_DATE="2026-01-01T12:00:00Z",
    )
    return env


def make_repo(parent: Path, name: str, files: Mapping[str, str]) -> Path:
    """Creates <parent>/<name> as a real git repo with the files (relative path -> content) in one commit."""
    if not files:
        raise ValueError("a fixture repo needs at least one file")
    repo = parent / name
    repo.mkdir(parents=True)
    for relative, content in files.items():
        path = repo / relative
        if repo.resolve() not in path.resolve().parents:
            raise ValueError(f"fixture file {relative!r} is outside the repo")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    env = git_environment(os.environ)

    def git(*args: str) -> None:
        subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True, text=True)

    git("-c", "init.defaultBranch=main", "init", "--quiet")
    git("add", "--all")
    git("-c", "commit.gpgsign=false", "commit", "--quiet", "--no-verify", "-m", "Initial commit")
    return repo


def cod_command(run_dir: Path, folder: Path | None) -> list[str]:
    """The command that starts Cod on the folder, as a user does, with its own session bus. Without a folder Cod restores its last session.

    The empty first argument means no start page (cod-wayland.sh takes [url] [folder]).
    """
    command = ["dbus-run-session", "--", str(run_dir / "cod-wayland.sh"), ""]
    if folder is not None:
        command.append(str(folder))
    return command


def cod_environment(
    base: Mapping[str, str], preview_dir: Path, socket_path: Path, home: Path, persistent: bool = False
) -> dict[str, str]:
    """Environment for one Cod start: a private data folder, a private home, the driver socket, no X11, no spike settings.

    The home and the XDG base folders point into the test's temp folder, so nothing from the user's home (shell rc files, git config, ~/.agents) reaches Cod.
    XDG_RUNTIME_DIR (the private sway's socket) and COD_CEF_DIR (an absolute path set by script/e2e) are kept.
    By default ZED_STATELESS=1 keeps Zed's database in memory. A persistent start leaves it out, so Zed keeps its database and its session in the data folder and the next start restores them.
    """
    env = dict(base)
    for name in ("DISPLAY", "COD_SPIKE_URL", "COD_SPIKE_DRIVE", "COD_SPIKE_PROBE", "COD_SPIKE_SHOTS", "ZED_STATELESS"):
        env.pop(name, None)
    env.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(home / ".config"),
        XDG_DATA_HOME=str(home / ".local" / "share"),
        XDG_CACHE_HOME=str(home / ".cache"),
        XDG_STATE_HOME=str(home / ".local" / "state"),
        COD_PREVIEW_DIR=str(preview_dir),
        COD_E2E_SOCKET=str(socket_path),
        ZED_ALLOW_EMULATED_GPU="1",
        XDG_SESSION_TYPE="wayland",
    )
    if not persistent:
        env["ZED_STATELESS"] = "1"
    env.setdefault("RUST_LOG", "info")
    return env


def cod_settings() -> dict[str, Any]:
    """The settings.json of every test start. The keys are Zed's (assets/settings/default.json).

    Trusted worktrees, so no trust prompt covers the window.
    No unsolicited network work: no automatic install of the html extension, no extension or app updates, no telemetry.
    Zed's own path picker and prompts instead of the system dialogs: the headless sway has no desktop portal, and the driver can read Zed's picker and prompts.
    No command history in the palette, so the palette ranks matches by the query alone and earlier commands of a test never change which row a command name highlights.
    """
    return {
        "session": {"trust_all_worktrees": True},
        "auto_install_extensions": {"html": False},
        "auto_update_extensions": {"html": False},
        "auto_update": False,
        "telemetry": {"diagnostics": False, "metrics": False},
        "use_system_path_prompts": False,
        "use_system_prompts": False,
        "command_palette": {"use_command_history": False},
    }


def palette_problem(expected: str, state: Mapping[str, Any]) -> str | None:
    """Why the command palette is not ready to run `expected`, or None when it highlights exactly that command."""
    if state.get("modal") != "command_palette":
        return failure_message("modal", "command_palette", state.get("modal"))
    selection = state.get("palette_selection")
    if selection != expected:
        palette = state.get("palette") or {}
        return (
            f"{failure_message('palette selection', expected, selection)} "
            f"(query {palette.get('query')!r}, {palette.get('match_count')} matches, highlighted row {palette.get('selected_index')})"
        )
    return None


def sway_socket(runtime_dir: Path) -> Path | None:
    """The IPC socket of the sway that owns the runtime folder (sway-ipc.<uid>.<pid>.sock), or None."""
    sockets = sorted(runtime_dir.glob("sway-ipc.*.sock"))
    return sockets[0] if sockets else None


def compositor_windows(tree: Mapping[str, Any], pids: set[int]) -> list[dict[str, Any]]:
    """{"id", "title", "focused"} of every window in sway's tree (swaymsg -t get_tree) whose process is one of pids, in tree order. The id is sway's id of the window."""
    found: list[dict[str, Any]] = []

    def walk(node: Mapping[str, Any]) -> None:
        if node.get("pid") in pids and node.get("type") in ("con", "floating_con"):
            found.append({"id": node.get("id"), "title": node.get("name"), "focused": bool(node.get("focused"))})
        for child in [*node.get("nodes", []), *node.get("floating_nodes", [])]:
            walk(child)

    walk(tree)
    return found


def window_titles(tree: Mapping[str, Any], pids: set[int]) -> list[tuple[str | None, bool]]:
    """(title, focused) of every window in sway's tree (swaymsg -t get_tree) whose process is one of pids."""
    return [(window["title"], window["focused"]) for window in compositor_windows(tree, pids)]


def window_title(windows: list[tuple[str | None, bool]]) -> str | None:
    """The title of the focused window among them, else of the only one, else None."""
    for title, focused in windows:
        if focused:
            return title
    return windows[0][0] if len(windows) == 1 else None


def artifact_name(test_id: str) -> str:
    """A folder name for a test id: letters, digits, dots, dashes and underscores only."""
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", test_id).strip("_") or "test"


def artifacts_root() -> Path:
    value = os.environ.get("COD_E2E_ARTIFACTS")
    return Path(value) if value else REPO_ROOT / "target" / "e2e-artifacts"


def processes_with_marker(marker: str) -> list[int]:
    """Process ids of this user's processes whose environment holds the line `marker` (for example COD_E2E_SOCKET=<path>)."""
    needle = marker.encode()
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            environ = (entry / "environ").read_bytes()
        except OSError:
            continue
        if needle in environ.split(b"\0"):
            found.append(int(entry.name))
    return found


def describe_processes(pids: list[int]) -> str:
    """Each process id with its command line, for error messages: `123 (/bin/bash -l -i -c ...)`, or `123 (gone)` when it exited meanwhile."""
    parts = []
    for pid in pids:
        try:
            raw = (Path("/proc") / str(pid) / "cmdline").read_bytes()
        except OSError:
            parts.append(f"{pid} (gone)")
            continue
        command = " ".join(part.decode(errors="replace") for part in raw.split(b"\0") if part)
        parts.append(f"{pid} ({command or 'no command line'})")
    return ", ".join(parts)


class Cod:
    """One running Cod app, started through cod-wayland.sh. A persistent Cod can be quit and started again on the same data folder and home."""

    def __init__(self, workdir: Path, folder: Path | None, persistent: bool = False) -> None:
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        if not os.environ.get("WAYLAND_DISPLAY"):
            raise RuntimeError("WAYLAND_DISPLAY is not set: run the end-to-end tests with script/e2e")
        self.run_dir = Path(run_dir)
        self.workdir = workdir
        self.folder = folder
        self.persistent = persistent
        self.preview_dir = workdir / "cod-data"
        self.home = workdir / "home"
        socket_dir = workdir / "driver"
        socket_dir.mkdir(mode=0o700, exist_ok=True)
        socket_dir.chmod(0o700)
        self.socket_path = socket_dir / "cod.sock"
        if len(os.fsencode(self.socket_path)) > MAX_SOCKET_PATH:
            raise RuntimeError(f"the driver socket path is too long for a Unix socket: {self.socket_path}")
        self.output_path = workdir / "cod-output.log"
        self.process: subprocess.Popen[bytes] | None = None
        self.driver: Driver | None = None
        # Settings added to cod_settings() at every start, and arguments (for example an ssh:// url) added to the next start only.
        self.extra_settings: dict[str, Any] = {}
        self.extra_args: list[str] = []

    @property
    def marker(self) -> str:
        """The environment line that every process of this Cod carries."""
        return f"COD_E2E_SOCKET={self.socket_path}"

    def start(self) -> None:
        if self.process is not None:
            raise RuntimeError("this Cod is already running")
        # Cod is started with --user-data-dir <COD_PREVIEW_DIR> by cod-wayland.sh, so its settings live in <COD_PREVIEW_DIR>/config.
        config = self.preview_dir / "config"
        config.mkdir(parents=True, exist_ok=True)
        (config / "settings.json").write_text(json.dumps({**cod_settings(), **self.extra_settings}, indent=2) + "\n")
        for folder in (".config", ".local/share", ".cache", ".local/state"):
            (self.home / folder).mkdir(parents=True, exist_ok=True)
        env = cod_environment(os.environ, self.preview_dir, self.socket_path, self.home, persistent=self.persistent)
        command = cod_command(self.run_dir, self.folder) + self.extra_args
        self.extra_args = []
        log(f"  starting: {' '.join(repr(part) if part == '' else part for part in command)}")
        # Appended, so the output of every start of this test is kept.
        with open(self.output_path, "ab") as output:
            self.process = subprocess.Popen(
                command,
                cwd=self.folder if self.folder is not None else self.workdir,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

    def connect(self, timeout: float = DRIVER_START_TIMEOUT) -> Driver:
        """Waits until the driver socket accepts a connection."""
        assert self.process is not None
        deadline = time.monotonic() + timeout
        last_error: OSError | None = None
        while time.monotonic() < deadline:
            status = self.process.poll()
            if status is not None:
                raise DriverError(
                    f"Cod exited with status {status} before the driver socket {self.socket_path} appeared "
                    f"(see cod-output.log and logs/cod-stderr.log in the artifacts)"
                )
            if self.socket_path.exists():
                try:
                    self.driver = Driver(self.socket_path)
                    return self.driver
                except OSError as error:
                    last_error = error
            time.sleep(POLL_INTERVAL)
        if self.socket_path.exists():
            raise DriverError(f"the driver socket {self.socket_path} exists but refused connections for {timeout:g} s: {last_error}")
        raise DriverError(
            f"the driver socket {self.socket_path} did not appear within {timeout:g} s: "
            f"Cod is running but does not listen on COD_E2E_SOCKET (does this build have the end-to-end driver?)"
        )

    def quit(self, timeout: float = QUIT_TIMEOUT) -> None:
        """Quits Cod as a user does, with Ctrl-Q, and waits until it and every process it started have exited. Fails when Cod exits with an error or does not exit in time."""
        if self.process is None or self.driver is None:
            raise RuntimeError("Cod is not running")
        try:
            self.driver.keys("ctrl-q")
        except DriverError as error:
            # Cod may close the connection before it answers: it is quitting.
            log(f"  the driver did not answer ctrl-q ({error}); waiting for Cod to exit")
        self.driver.close()
        self.driver = None
        try:
            status = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise AssertionError(f"Cod did not exit within {timeout:g} s after ctrl-q") from error
        self.process = None
        deadline = time.monotonic() + timeout
        left = processes_with_marker(self.marker)
        while left and time.monotonic() < deadline:
            time.sleep(POLL_INTERVAL)
            left = processes_with_marker(self.marker)
        if left:
            raise AssertionError(f"processes of this Cod were still running {timeout:g} s after it quit: {describe_processes(left)}")
        if status != 0:
            raise AssertionError(f"Cod exited with status {status} after ctrl-q (see cod-output.log and logs/cod-stderr.log)")
        # The socket file of the old Cod stays behind; the next Cod replaces it, but connect() must not see it in the meantime.
        with contextlib.suppress(FileNotFoundError):
            self.socket_path.unlink()
        log("  Cod quit; no process of it is left")

    def kill(self, timeout: float = QUIT_TIMEOUT) -> None:
        """Kills Cod as a crash or a power loss would: SIGKILL to its process group, no clean quit, then SIGKILL to every process it started that left the group. The data folder and home stay for the next start."""
        if self.process is None:
            raise RuntimeError("Cod is not running")
        if self.driver is not None:
            self.driver.close()
            self.driver = None
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(self.process.pid, signal.SIGKILL)
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise AssertionError(f"Cod did not exit within {timeout:g} s after SIGKILL") from error
        self.process = None
        deadline = time.monotonic() + timeout
        left = processes_with_marker(self.marker)
        while left and time.monotonic() < deadline:
            for pid in left:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.kill(pid, signal.SIGKILL)
            time.sleep(POLL_INTERVAL)
            left = processes_with_marker(self.marker)
        if left:
            raise AssertionError(f"processes of this Cod were still running {timeout:g} s after SIGKILL: {describe_processes(left)}")
        # The killed Cod leaves its socket file behind; connect() must not see it before the next Cod listens.
        with contextlib.suppress(FileNotFoundError):
            self.socket_path.unlink()
        log("  Cod killed with SIGKILL; no process of it is left")

    def stop(self) -> None:
        """Stops Cod and every process it started (the session bus, cod-browser, the browser's helpers)."""
        if self.driver is not None:
            self.driver.close()
            self.driver = None
        marker = self.marker
        # After a failed quit Cod itself may be gone while processes it started still run: those are stopped too.
        if self.process is None and not processes_with_marker(marker):
            return
        if self.process is not None:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=10)
        self.process = None
        # Helpers that left the process group still carry the driver socket in their environment. They are killed until two scans in a row find none, for up to STOP_LEFTOVER_TIMEOUT.
        deadline = time.monotonic() + STOP_LEFTOVER_TIMEOUT
        clean_scans = 0
        while clean_scans < 2 and time.monotonic() < deadline:
            leftovers = processes_with_marker(marker)
            if not leftovers:
                clean_scans += 1
                if clean_scans < 2:
                    time.sleep(STOP_SETTLE_INTERVAL)
                continue
            clean_scans = 0
            log(f"  killing processes of this test that left its process group: {describe_processes(leftovers)}")
            for pid in leftovers:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.kill(pid, signal.SIGKILL)
            time.sleep(POLL_INTERVAL)
        left = processes_with_marker(marker)
        if left:
            raise RuntimeError(
                f"processes of this test are still running {STOP_LEFTOVER_TIMEOUT:g} s after Cod was stopped: {describe_processes(left)}"
            )
        log("  Cod stopped; no process of this test is left")


class CodTestCase(unittest.TestCase):
    """Base class for story tests. Every action goes inside `with self.step("...")`, so a failure names its step and saves the artifacts."""

    def setUp(self) -> None:
        self.workdir = Path(tempfile.mkdtemp(prefix="cod-e2e-"))
        self.addCleanup(shutil.rmtree, self.workdir, ignore_errors=True)
        self.cod: Cod | None = None
        self._artifacts_saved = False
        log(f"TEST: {self.id()}")

    # Fixtures

    def make_repo(self, name: str, files: Mapping[str, str]) -> Path:
        """Creates a real git repo for this test in its temp folder and returns its path."""
        return make_repo(self.workdir / "repos", name, files)

    # The app

    def start_cod(self, folder: Path | None, persistent: bool = False, timeout: float = DRIVER_START_TIMEOUT) -> None:
        """Starts Cod on the folder (None: no folder) and connects to its driver. Cod is stopped when the test ends.

        persistent=True starts Cod without ZED_STATELESS, so it keeps its database and session in the test's data folder and restart_cod() can bring them back.
        """
        if self.cod is not None:
            raise RuntimeError("Cod was already started in this test: use restart_cod() to start it again")
        self.cod = Cod(self.workdir, folder, persistent=persistent)
        self.addCleanup(self._stop_cod)
        # Cleanups run last in, first out: the final artifacts are taken while Cod still runs.
        self.addCleanup(self._save_final_artifacts)
        self.cod.start()
        self.cod.connect(timeout)

    def restart_cod(self, timeout: float = DRIVER_START_TIMEOUT) -> None:
        """Quits Cod with Ctrl-Q, waits until it has exited, and starts it again with the same data folder and home and no folder, so Zed restores its last session."""
        if self.cod is None or self.cod.process is None:
            raise RuntimeError("Cod is not running: call start_cod first")
        if not self.cod.persistent:
            raise RuntimeError("a stateless Cod forgets its session: start it with start_cod(folder, persistent=True)")
        self.cod.quit()
        self.cod.folder = None
        self.cod.start()
        self.cod.connect(timeout)

    def kill_cod(self) -> None:
        """Kills Cod with SIGKILL (its process group and every process it started), with no clean quit, as a crash would. start_cod_again() starts it on the same data folder."""
        if self.cod is None or self.cod.process is None:
            raise RuntimeError("Cod is not running: call start_cod first")
        self.cod.kill()

    def start_cod_again(self, connect: bool = True, timeout: float = DRIVER_START_TIMEOUT) -> None:
        """Starts Cod again after kill_cod(), with the same data folder and home and no folder, so Zed restores its last session. connect=False only launches it (to kill it again while it starts)."""
        if self.cod is None:
            raise RuntimeError("Cod was never started in this test: call start_cod first")
        if self.cod.process is not None:
            raise RuntimeError("Cod is still running: use restart_cod() or kill_cod() first")
        if not self.cod.persistent:
            raise RuntimeError("a stateless Cod forgets its session: start it with start_cod(folder, persistent=True)")
        self.cod.folder = None
        self.cod.start()
        if connect:
            self.cod.connect(timeout)

    def run_command(self, command: str, timeout: float = PALETTE_TIMEOUT) -> None:
        """Runs a command from the command palette with keys only: ctrl-shift-p, type the command, wait until the palette highlights exactly that command, enter.

        Fails with the expected and the highlighted command when the palette highlights another one, so a near match never runs instead.
        """
        self.keys("ctrl-shift-p")
        self.wait_for(lambda state: state["modal"] == "command_palette", timeout, "the command palette to open")
        self.type(command)
        deadline = time.monotonic() + timeout
        while True:
            state = self.state()
            problem = palette_problem(command, state)
            if problem is None:
                break
            if time.monotonic() >= deadline:
                raise AssertionError(f"{problem}; after {timeout:g} s")
            time.sleep(POLL_INTERVAL)
        self.keys("enter")
        self.wait_for(lambda state: state["modal"] != "command_palette", timeout, "the command palette to close")

    def _stop_cod(self) -> None:
        if self.cod is not None:
            self.cod.stop()

    @property
    def driver(self) -> Driver:
        if self.cod is None or self.cod.driver is None:
            raise DriverError("Cod is not running: call start_cod first")
        return self.cod.driver

    def keys(self, keys: str) -> None:
        self.driver.keys(keys)

    def type(self, text: str) -> None:
        self.driver.type(text)

    def state(self) -> dict[str, Any]:
        """The driver's state plus what sway shows: "window_title", the title of Cod's window, and "sway_windows", sway's ids of Cod's windows."""
        state = self.driver.state()
        windows = self.compositor_windows()
        state["window_title"] = window_title([(window["title"], window["focused"]) for window in windows])
        state["sway_windows"] = [window["id"] for window in windows]
        return state

    def window_title(self) -> str | None:
        """The title of Cod's window as the compositor shows it (the focused one when there are several), or None when Cod has no window."""
        return window_title([(window["title"], window["focused"]) for window in self.compositor_windows()])

    def compositor_windows(self) -> list[dict[str, Any]]:
        """Cod's windows as sway shows them: {"id", "title", "focused"} each."""
        if self.cod is None:
            raise DriverError("Cod is not running: call start_cod first")
        runtime = os.environ.get("XDG_RUNTIME_DIR")
        sway = sway_socket(Path(runtime)) if runtime else None
        if sway is None:
            raise DriverError(f"no sway IPC socket in XDG_RUNTIME_DIR={runtime!r}: run the end-to-end tests with script/e2e")
        try:
            reply = subprocess.run(
                ["swaymsg", "-s", str(sway), "-t", "get_tree", "-r"], capture_output=True, text=True, timeout=COMMAND_TIMEOUT
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            raise DriverError(f"swaymsg failed: {error}") from error
        if reply.returncode != 0:
            raise DriverError(f"swaymsg failed with status {reply.returncode}: {reply.stderr.strip()}")
        pids = set(processes_with_marker(self.cod.marker))
        return compositor_windows(json.loads(reply.stdout), pids)

    def wait_for(self, predicate: Callable[[dict[str, Any]], bool], timeout: float, description: str) -> dict[str, Any]:
        """Reads the state until predicate(state) is true and returns that state. Fails after timeout seconds."""
        deadline = time.monotonic() + timeout
        while True:
            state = self.state()
            if predicate(state):
                return state
            if time.monotonic() >= deadline:
                raise AssertionError(f"timed out after {timeout:g} s waiting for {description}; last state: {json.dumps(state)}")
            time.sleep(POLL_INTERVAL)

    def wait_for_values(
        self, what: str, expected: Mapping[str, Any], read: Callable[[dict[str, Any]], Mapping[str, Any]], timeout: float
    ) -> dict[str, Any]:
        """Reads the state until read(state) has every value of expected, and returns that state.

        Fails after timeout seconds with one "expected <what> <key> ..., actual ..." message per value that still differs, then the last state.
        """
        deadline = time.monotonic() + timeout
        while True:
            state = self.state()
            problems = differences(what, expected, read(state))
            if not problems:
                return state
            if time.monotonic() >= deadline:
                raise AssertionError(f"timed out after {timeout:g} s waiting for {what}: {'; '.join(problems)}; last state: {json.dumps(state)}")
            time.sleep(POLL_INTERVAL)

    # Checks and failures

    def expect_equal(self, what: str, expected: Any, actual: Any) -> None:
        expect_equal(what, expected, actual)

    def step(self, description: str) -> contextlib.AbstractContextManager[None]:
        return step(description, self._save_artifacts)

    def _save_artifacts(self, description: str, error: BaseException) -> None:
        """On the first failed step: failure.txt plus the artifacts of _collect_artifacts."""
        self._collect_artifacts(
            "failure.txt", f"test: {self.id()}\nfailed step: {description}\n{type(error).__name__}: {error}\n", show_stderr=True
        )

    def _save_final_artifacts(self) -> None:
        """At the end of every test that started Cod and had no failed step: final.txt plus the artifacts of _collect_artifacts."""
        try:
            self._collect_artifacts("final.txt", f"test: {self.id()}\nno step failed; saved at the end of the test\n", show_stderr=False)
        except Exception as error:  # The final artifacts are a convenience; they never fail a test.
            log(f"  could not save the final artifacts: {error}")

    def _collect_artifacts(self, note_name: str, note: str, show_stderr: bool) -> None:
        """Saves a note, a screenshot, Cod's logs, its hang traces and the last state into $COD_E2E_ARTIFACTS/<test id>/, once per test."""
        if self._artifacts_saved:
            return
        self._artifacts_saved = True
        target = artifacts_root() / artifact_name(self.id())
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir(parents=True)
        (target / note_name).write_text(note)
        try:
            shot = subprocess.run(["grim", str(target / "screenshot.png")], capture_output=True, text=True, timeout=20)
            if shot.returncode != 0:
                log(f"  grim failed with status {shot.returncode}: {shot.stderr.strip()}")
        except (OSError, subprocess.TimeoutExpired) as shot_error:
            log(f"  grim failed: {shot_error}")
        if self.cod is not None:
            if self.cod.driver is not None:
                with contextlib.suppress(Exception):
                    (target / "state.json").write_text(json.dumps(self.cod.driver.state(), indent=2) + "\n")
            # The data folder is also Cod's --user-data-dir, so logs/ holds Zed.log as well as cod-stderr.log.
            logs = self.cod.preview_dir / "logs"
            if logs.is_dir():
                shutil.copytree(logs, target / "logs", dirs_exist_ok=True)
            # Zed's hang detector saves its task traces into <user data dir>/hang_traces (paths::hang_traces_dir).
            hang_traces = self.cod.preview_dir / "hang_traces"
            if hang_traces.is_dir():
                shutil.copytree(hang_traces, target / "hang_traces", dirs_exist_ok=True)
            if self.cod.output_path.exists():
                shutil.copy2(self.cod.output_path, target / "cod-output.log")
            stderr_log = logs / "cod-stderr.log"
            if show_stderr and stderr_log.exists():
                tail = stderr_log.read_text(errors="replace").splitlines()[-30:]
                log("  last lines of cod-stderr.log:")
                for line in tail:
                    log(f"    {line}")
        saved = sorted(str(path.relative_to(target)) for path in target.rglob("*") if path.is_file())
        log(f"  artifacts saved to {target}: {', '.join(saved)}")
