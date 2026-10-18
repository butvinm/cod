"""Library for Cod's end-to-end tests (issue #22).

A test starts the real Cod app through cod-wayland.sh, exactly as users start it, and drives it only through the end-to-end driver: a Unix socket that Cod opens when it is started with COD_E2E_SOCKET=<path>.
The protocol is one JSON object per line, one JSON reply per line:

    {"cmd": "keys", "keys": "ctrl-p"}    presses each space-separated keystroke (Zed keystroke syntax)
    {"cmd": "type", "text": "invoice"}   types the text, one key event per character
    {"cmd": "state"}                     {"active_tab": ..., "tabs": [...], "worktrees": [...], "modal": ...}

Every reply has "ok". A failed command replies {"ok": false, "error": "..."}.
Actions go through keys and typing only, never through private calls that a user cannot make. "state" only observes what the user sees.

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


def cod_command(run_dir: Path, folder: Path) -> list[str]:
    """The command that starts Cod on the folder, as a user does, with its own session bus.

    The empty first argument means no start page (cod-wayland.sh takes [url] [folder]).
    """
    return ["dbus-run-session", "--", str(run_dir / "cod-wayland.sh"), "", str(folder)]


def cod_environment(base: Mapping[str, str], preview_dir: Path, socket_path: Path) -> dict[str, str]:
    """Environment for one Cod start: a private data folder, the driver socket, no X11, no spike settings."""
    env = dict(base)
    for name in ("DISPLAY", "COD_SPIKE_URL", "COD_SPIKE_DRIVE", "COD_SPIKE_PROBE", "COD_SPIKE_SHOTS"):
        env.pop(name, None)
    env.update(
        COD_PREVIEW_DIR=str(preview_dir),
        COD_E2E_SOCKET=str(socket_path),
        ZED_STATELESS="1",
        ZED_ALLOW_EMULATED_GPU="1",
        XDG_SESSION_TYPE="wayland",
    )
    env.setdefault("RUST_LOG", "info")
    return env


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


class Cod:
    """One running Cod app, started through cod-wayland.sh."""

    def __init__(self, workdir: Path, folder: Path) -> None:
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        if not os.environ.get("WAYLAND_DISPLAY"):
            raise RuntimeError("WAYLAND_DISPLAY is not set: run the end-to-end tests with script/e2e")
        self.run_dir = Path(run_dir)
        self.folder = folder
        self.preview_dir = workdir / "cod-data"
        socket_dir = workdir / "driver"
        socket_dir.mkdir(mode=0o700)
        socket_dir.chmod(0o700)
        self.socket_path = socket_dir / "cod.sock"
        if len(os.fsencode(self.socket_path)) > MAX_SOCKET_PATH:
            raise RuntimeError(f"the driver socket path is too long for a Unix socket: {self.socket_path}")
        self.output_path = workdir / "cod-output.log"
        self.process: subprocess.Popen[bytes] | None = None
        self.driver: Driver | None = None

    def start(self) -> None:
        # Cod is started with --user-data-dir <COD_PREVIEW_DIR> by cod-wayland.sh, so its settings live in <COD_PREVIEW_DIR>/config.
        config = self.preview_dir / "config"
        config.mkdir(parents=True)
        (config / "settings.json").write_text(json.dumps({"session": {"trust_all_worktrees": True}}, indent=2) + "\n")
        env = cod_environment(os.environ, self.preview_dir, self.socket_path)
        command = cod_command(self.run_dir, self.folder)
        log(f"  starting: {' '.join(repr(part) if part == '' else part for part in command)}")
        with open(self.output_path, "wb") as output:
            self.process = subprocess.Popen(
                command,
                cwd=self.folder,
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

    def stop(self) -> None:
        """Stops Cod and every process it started (the session bus, cod-browser, the browser's helpers)."""
        if self.driver is not None:
            self.driver.close()
            self.driver = None
        if self.process is None:
            return
        marker = f"COD_E2E_SOCKET={self.socket_path}"
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=10)
        # Helpers that left the process group still carry the driver socket in their environment.
        for _ in range(2):
            leftovers = processes_with_marker(marker)
            if not leftovers:
                break
            for pid in leftovers:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    os.kill(pid, signal.SIGKILL)
            time.sleep(0.5)
        self.process = None


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

    def start_cod(self, folder: Path, timeout: float = DRIVER_START_TIMEOUT) -> None:
        """Starts Cod on the folder and connects to its driver. Cod is stopped when the test ends."""
        if self.cod is not None:
            raise RuntimeError("Cod is already running in this test")
        self.cod = Cod(self.workdir, folder)
        self.addCleanup(self._stop_cod)
        self.cod.start()
        self.cod.connect(timeout)

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
        return self.driver.state()

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

    # Checks and failures

    def expect_equal(self, what: str, expected: Any, actual: Any) -> None:
        expect_equal(what, expected, actual)

    def step(self, description: str) -> contextlib.AbstractContextManager[None]:
        return step(description, self._save_artifacts)

    def _save_artifacts(self, description: str, error: BaseException) -> None:
        """Saves a screenshot, Cod's logs and the last state into $COD_E2E_ARTIFACTS/<test id>/, once per test."""
        if self._artifacts_saved:
            return
        self._artifacts_saved = True
        target = artifacts_root() / artifact_name(self.id())
        shutil.rmtree(target, ignore_errors=True)
        target.mkdir(parents=True)
        (target / "failure.txt").write_text(f"test: {self.id()}\nfailed step: {description}\n{type(error).__name__}: {error}\n")
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
            if self.cod.output_path.exists():
                shutil.copy2(self.cod.output_path, target / "cod-output.log")
            stderr_log = logs / "cod-stderr.log"
            if stderr_log.exists():
                tail = stderr_log.read_text(errors="replace").splitlines()[-30:]
                log("  last lines of cod-stderr.log:")
                for line in tail:
                    log(f"    {line}")
        saved = sorted(str(path.relative_to(target)) for path in target.rglob("*") if path.is_file())
        log(f"  artifacts saved to {target}: {', '.join(saved)}")
