"""A disposable SSH service on 127.0.0.1 for the remote workspace tests of story #24.

It runs the system sshd as the current user (no root, no system config): its own host key, its own authorized_keys with a key pair made for this test, password and keyboard-interactive login off, bound to 127.0.0.1 on a free high port.
Cod's ssh gets its key and known_hosts file through the "args" of an ssh_connections entry in settings.json, so nothing in ~/.ssh is read or written.
The remote server binary is not downloaded: Zed runs ~/.zed_server/zed-remote-server-<channel>-<version> when "<binary> version" succeeds there (crates/remote/src/transport/ssh.rs), so the binary built in CI (COD_E2E_REMOTE_SERVER) is copied to that path in the passwd home of the current user, which is the home sshd logs in to.
Every reason the service cannot run is returned as a skip reason, so a missing sshd never looks like a pass.
"""

from __future__ import annotations

import os
import pwd
import re
import shutil
import signal
import socket
import subprocess
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SSHD_CANDIDATES = ("/usr/sbin/sshd", "/usr/bin/sshd", "/usr/local/sbin/sshd")


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def remote_server_name() -> str:
    """The file name Zed looks for on the remote host: zed-remote-server-<channel>-<version> (SshRemoteConnection::ensure_server_binary)."""
    channel = (REPO_ROOT / "crates" / "zed" / "RELEASE_CHANNEL").read_text().strip()
    match = re.search(r'^version = "([^"]+)"', (REPO_ROOT / "crates" / "zed" / "Cargo.toml").read_text(), re.MULTILINE)
    if match is None:
        raise RuntimeError("no version in crates/zed/Cargo.toml")
    return f"zed-remote-server-{channel}-{match.group(1)}"


def required() -> bool:
    """Whether these tests must run (COD_E2E_REQUIRE_SSH=1, set in CI): then every reason they cannot run is a failure, not a skip."""
    return os.environ.get("COD_E2E_REQUIRE_SSH") == "1"


def missing_reason() -> str | None:
    """Why the loopback SSH tests cannot run here, or None when they can."""
    if shutil.which("ssh") is None:
        return "no ssh client on PATH (Ubuntu: openssh-client)"
    if shutil.which("ssh-keygen") is None:
        return "no ssh-keygen on PATH (Ubuntu: openssh-client)"
    if sshd_path() is None:
        return f"no sshd in {', '.join(SSHD_CANDIDATES)} (Ubuntu: openssh-server)"
    server = os.environ.get("COD_E2E_REMOTE_SERVER")
    if not server:
        return "COD_E2E_REMOTE_SERVER is not set (the remote_server binary: cargo build --profile release-fast -p remote_server)"
    if not os.access(server, os.X_OK):
        return f"COD_E2E_REMOTE_SERVER is not an executable file: {server}"
    return None


def sshd_path() -> str | None:
    for candidate in SSHD_CANDIDATES:
        if os.access(candidate, os.X_OK):
            return candidate
    return None


class LoopbackSsh:
    """One sshd on 127.0.0.1:<port> for the current user. stop() and start() again keep the same port, keys and known_hosts."""

    def __init__(self, folder: Path, port: int | None = None) -> None:
        self.folder = folder
        self.folder.mkdir(parents=True, exist_ok=True)
        self.port = port or free_port()
        self.user = pwd.getpwuid(os.getuid()).pw_name
        self.home = Path(pwd.getpwuid(os.getuid()).pw_dir)
        self.client_key = folder / "client_ed25519"
        self.host_key = folder / "host_ed25519"
        self.known_hosts = folder / "known_hosts"
        self.config = folder / "sshd_config"
        self.log = folder / "sshd.log"
        self.process: subprocess.Popen[bytes] | None = None
        self._make_keys()
        self._write_config()

    def _keygen(self, path: Path) -> None:
        if not path.exists():
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "cod-e2e", "-f", str(path)], check=True, capture_output=True)

    def _make_keys(self) -> None:
        self._keygen(self.client_key)
        self._keygen(self.host_key)
        (self.folder / "authorized_keys").write_text(Path(f"{self.client_key}.pub").read_text())
        (self.folder / "authorized_keys").chmod(0o600)
        host_public = Path(f"{self.host_key}.pub").read_text().split()
        self.known_hosts.write_text(f"[127.0.0.1]:{self.port} {host_public[0]} {host_public[1]}\n")

    def _write_config(self) -> None:
        lines = [
            "ListenAddress 127.0.0.1",
            f"Port {self.port}",
            f"HostKey {self.host_key}",
            f"PidFile {self.folder / 'sshd.pid'}",
            f"AuthorizedKeysFile {self.folder / 'authorized_keys'}",
            f"AllowUsers {self.user}",
            "PubkeyAuthentication yes",
            "PasswordAuthentication no",
            "KbdInteractiveAuthentication no",
            "PermitRootLogin no",
            "UsePAM no",
            "StrictModes no",
            "PrintMotd no",
            "LogLevel VERBOSE",
            "Subsystem sftp internal-sftp",
        ]
        self.config.write_text("\n".join(lines) + "\n")

    def client_args(self) -> list[str]:
        """The ssh arguments Cod gets from settings.json: this test's key and known_hosts only, no user ssh config."""
        return [
            "-F",
            "none",
            "-i",
            str(self.client_key),
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            f"UserKnownHostsFile={self.known_hosts}",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "ConnectTimeout=5",
        ]

    def connection_setting(self) -> dict[str, object]:
        """The ssh_connections entry of settings.json for this service (SshConnection in crates/settings_content)."""
        return {"host": "127.0.0.1", "port": self.port, "username": self.user, "args": self.client_args()}

    def url(self, path: Path) -> str:
        return f"ssh://{self.user}@127.0.0.1:{self.port}{path}"

    def start(self, timeout: float = 10.0) -> str | None:
        """Starts sshd and waits until it accepts a login. Returns None, or why it could not start."""
        if self.process is not None:
            return None
        sshd = sshd_path()
        if sshd is None:
            return "no sshd"
        with open(self.log, "ab") as log:
            self.process = subprocess.Popen([sshd, "-D", "-e", "-f", str(self.config)], stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.process = None
                return f"sshd exited at start; its log: {self.log.read_text(errors='replace')[-2000:]}"
            probe = subprocess.run(
                ["ssh", *self.client_args(), "-o", "BatchMode=yes", "-p", str(self.port), f"{self.user}@127.0.0.1", "true"],
                capture_output=True,
                text=True,
            )
            if probe.returncode == 0:
                return None
            time.sleep(0.2)
        self.stop()
        return f"sshd did not accept a key login on 127.0.0.1:{self.port} within {timeout}s; its log: {self.log.read_text(errors='replace')[-2000:]}"

    def stop(self) -> None:
        if self.process is None:
            return
        self.process.send_signal(signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process = None
        # sshd -D leaves its per-connection children running; the remote server keeps those alive, and a stopped service must refuse new logins only, which it does once the listener is gone.

    def install_remote_server(self) -> Path:
        """Copies COD_E2E_REMOTE_SERVER to the path Zed checks on the remote host, so no download is needed."""
        source = Path(os.environ["COD_E2E_REMOTE_SERVER"])
        target = self.home / ".zed_server" / remote_server_name()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.unlink(missing_ok=True)
        shutil.copyfile(source, target)
        target.chmod(0o755)
        return target

    def install_slow_remote_server(self, delay: float) -> Path:
        """Puts a wrapper at the path Zed checks on the remote host that sleeps `delay` seconds on every run, then runs COD_E2E_REMOTE_SERVER, so the remote client stays in its startup while sshd is up. install_remote_server() puts the real binary back."""
        source = Path(os.environ["COD_E2E_REMOTE_SERVER"]).resolve()
        target = self.home / ".zed_server" / remote_server_name()
        target.parent.mkdir(parents=True, exist_ok=True)
        target.unlink(missing_ok=True)
        target.write_text(f'#!/bin/sh\nsleep {delay:g}\nexec "{source}" "$@"\n')
        target.chmod(0o755)
        return target
