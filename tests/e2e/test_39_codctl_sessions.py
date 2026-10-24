"""Story #39: codctl sessions - new, list, send, wait and a status line.

Builds on #37 (the codctl socket and workspace commands) and the workspace chats of #28. Tests
start Cod with a temp data folder and run the shipped `codctl` binary as a separate process, the
way an agent does through Bash, against the scripted fake agent of fake_acp_agent.py registered
as both `claude` and `codex` (#39's own test setup: two `agent_servers` entries of the same fake
agent, each with its own script, so their session counters do not mix).
"""

from __future__ import annotations

import json
import os
import subprocess
import time
import unittest
from pathlib import Path
from typing import Any

from cod_e2e import CodTestCase
from fake_acp_agent import fake_agent_settings

COMMAND_TIMEOUT = 30.0
SHOW_TIMEOUT = 15.0
SETTLE_TIME = 1.0

BILLING_API = {
    "Cargo.toml": '[package]\nname = "billing-api"\nversion = "0.1.0"\nedition = "2021"\n',
    "src/main.rs": "mod invoice;\n\nfn main() {\n    println!(\"billing-api\");\n}\n",
    "src/invoice.rs": "pub fn total(lines: &[u64]) -> u64 {\n    lines.iter().sum()\n}\n",
}


def chat(state: dict[str, Any]) -> dict[str, Any]:
    return state.get("chat") or {}


def prompt_texts_and_cancels(log_path: Path) -> tuple[list[str], int]:
    """Reads the fake agent's own `COD_FAKE_ACP_LOG` (RC review C2 new defect #2): the text of
    every `session/prompt` call the agent received, in arrival order, and how many
    `session/cancel` notifications arrived. A cancel while a turn runs is exactly what the
    legacy transport's `send_content` path used to trigger."""
    prompts: list[str] = []
    cancels = 0
    if not log_path.exists():
        return prompts, cancels
    for line in log_path.read_text(encoding="utf-8").splitlines():
        direction, _, payload = line.partition(" ")
        if direction != "in" or not payload:
            continue
        message = json.loads(payload)
        method = message.get("method")
        if method == "session/prompt":
            blocks = (message.get("params") or {}).get("prompt") or []
            text = "".join(block.get("text", "") for block in blocks if block.get("type") == "text")
            prompts.append(text)
        elif method == "session/cancel":
            cancels += 1
    return prompts, cancels


class Codctl:
    """Runs the shipped codctl binary as a separate process with the test's HOME."""

    def __init__(self, binary: Path, home: Path) -> None:
        self.binary = binary
        self.home = home

    def environment(self) -> dict[str, str]:
        env = dict(os.environ)
        for name in ("COD_WORKSPACE", "COD_SOCKET"):
            env.pop(name, None)
        env["HOME"] = str(self.home)
        return env

    def run(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.binary), *args],
            env=self.environment(),
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT,
            stdin=subprocess.DEVNULL,
        )


class CodctlSessionsTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.home = self.workdir / "home"
        run_dir = os.environ.get("COD_E2E_RUN_DIR")
        if not run_dir:
            raise RuntimeError("COD_E2E_RUN_DIR is not set: run the end-to-end tests with script/e2e")
        self.codctl = Codctl(Path(run_dir) / "codctl", self.home)

    # Fixtures and helpers

    def write_script(self, name: str, content: dict[str, Any]) -> Path:
        path = self.workdir / name
        path.write_text(json.dumps(content))
        return path

    def start_with_agents(self, claude_script: Path | None, codex_script: Path | None) -> Path:
        """Starts Cod on a fresh billing-api repo with the fake agent registered as `claude`
        and/or `codex` (#39's own test setup, built on #28's fake_agent_settings)."""
        repo = self.make_repo("billing-api", BILLING_API)
        agent_servers: dict[str, Any] = {}
        if claude_script is not None:
            agent_servers["claude"] = fake_agent_settings(claude_script, log=self.workdir / "claude.log")
        if codex_script is not None:
            agent_servers["codex"] = fake_agent_settings(codex_script, log=self.workdir / "codex.log")
        self.start_cod(repo, extra_settings={"agent_servers": agent_servers})
        self.wait_for(
            lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
            SHOW_TIMEOUT,
            "the billing-api folder to open",
        )
        self.wait_for(lambda state: state.get("agent_panel_ready") is True, SHOW_TIMEOUT, "the agent panel to load")
        return repo

    def expect_result(
        self, result: subprocess.CompletedProcess[str], code: int, stdout: str | None = None, stderr: str | None = None
    ) -> None:
        details = f"stdout {result.stdout!r}, stderr {result.stderr!r}"
        self.expect_equal(f"exit code of {result.args[1:]} ({details})", code, result.returncode)
        if stdout is not None:
            self.expect_equal(f"stdout of {result.args[1:]}", stdout, result.stdout)
        if stderr is not None:
            self.expect_equal(f"stderr of {result.args[1:]}", stderr, result.stderr)

    def new_session(self, agent: str) -> str:
        result = self.codctl.run("session", "new", "--agent", agent)
        self.expect_result(result, 0, stderr="")
        session_id = result.stdout.strip()
        assert session_id, f"codctl session new --agent {agent} printed nothing"
        return session_id

    # AC1

    def test_ac1_new_session_opens_a_chat_and_prints_its_session_id(self) -> None:
        with self.step("create the fixture repo and a one-reply claude script"):
            script = self.write_script(
                "claude.json",
                {"session_ids": ["claude-session-1"], "turns": [{"steps": [{"chunk": "hi"}, {"end": "end_turn"}]}]},
            )
            self.start_with_agents(script, None)

        with self.step("check that the workspace's default fresh chat is not already claude's (#28 AC2)"):
            self.expect_equal("chat agent is not claude yet", False, chat(self.state()).get("agent") == "claude")

        with self.step("run codctl session new --agent claude"):
            result = self.codctl.run("session", "new", "--agent", "claude")
            self.expect_result(result, 0, stdout="claude-session-1\n", stderr="")

        with self.step("the chat opened in the agent panel with that session id and agent"):
            state = self.wait_for(
                lambda state: chat(state).get("session_id") == "claude-session-1",
                SHOW_TIMEOUT,
                "the new claude chat",
            )
            self.expect_equal("agent", "claude", chat(state)["agent"])

    def test_ac1_new_session_in_a_workspace_made_just_before(self) -> None:
        """#39 AC1 with --workspace, from the executed beta on a6712b37b0: a script that runs `workspace new` and then at once `session new --workspace <id>` got "the workspace has no agent panel", because the new workspace attaches its agent panel a moment later."""
        with self.step("start Cod with the fake agent as claude and make a second fixture repo"):
            script = self.write_script(
                "claude.json",
                {"session_ids": ["claude-session-web-1"], "turns": [{"steps": [{"chunk": "hi"}, {"end": "end_turn"}]}]},
            )
            self.start_with_agents(script, None)
            web = self.make_repo("billing-web", BILLING_API)

        with self.step("run codctl workspace new --folder billing-web, then at once session new --workspace <its id>"):
            made = self.codctl.run("workspace", "new", "--folder", str(web))
            self.expect_result(made, 0, stderr="")
            workspace_id = made.stdout.strip()
            result = self.codctl.run("session", "new", "--agent", "claude", "--workspace", workspace_id)
            self.expect_result(result, 0, stdout="claude-session-web-1\n", stderr="")

        with self.step("session list for that workspace has the new session"):
            listed = json.loads(self.codctl.run("session", "list", "--workspace", workspace_id, "--json").stdout)
            self.expect_equal("session ids of the new workspace", ["claude-session-web-1"], [entry["id"] for entry in listed])

    def test_ac1_new_session_with_ai_disabled_fails_at_once(self) -> None:
        """#39 AC1, from the review of the wait above: with `disable_ai` the agent panel never comes, so session new must fail at once with one line instead of waiting for it."""
        with self.step("start Cod with disable_ai on a fixture repo"):
            repo = self.make_repo("billing-api", BILLING_API)
            self.start_cod(repo, extra_settings={"disable_ai": True})
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                SHOW_TIMEOUT,
                "the billing-api folder to open",
            )

        with self.step("session new --agent claude fails with one stderr line in under 3 seconds"):
            started = time.monotonic()
            result = self.codctl.run("session", "new", "--agent", "claude")
            elapsed = time.monotonic() - started
            self.expect_equal(f"exit code ({result.stderr!r})", 1, result.returncode)
            self.expect_equal("stdout", "", result.stdout)
            self.expect_equal(f"one stderr line ({result.stderr!r})", 1, len(result.stderr.splitlines()))
            self.expect_equal(f"failed in under 3 seconds (took {elapsed:.1f} s)", True, elapsed < 3)

    # AC2

    def test_ac2_list_sessions_plain_and_json(self) -> None:
        with self.step("create the fixture repo and register the fake agent as claude and codex"):
            claude_script = self.write_script("claude.json", {"session_ids": ["claude-session-1"], "turns": []})
            codex_script = self.write_script("codex.json", {"session_ids": ["codex-session-1"], "turns": []})
            self.start_with_agents(claude_script, codex_script)

        with self.step("start a claude session and a codex session"):
            claude_id = self.new_session("claude")
            codex_id = self.new_session("codex")

        with self.step("codctl session list prints one line per session"):
            result = self.codctl.run("session", "list")
            self.expect_result(result, 0, stderr="")
            lines = [line for line in result.stdout.splitlines() if line]
            self.expect_equal("one line per session", 2, len(lines))
            ids = {line.split("\t")[0] for line in lines}
            self.expect_equal("session ids", {claude_id, codex_id}, ids)

        with self.step("codctl session list --json adds the workspace id"):
            result = self.codctl.run("session", "list", "--json")
            self.expect_result(result, 0, stderr="")
            listed = json.loads(result.stdout)
            self.expect_equal("count", 2, len(listed))
            for entry in listed:
                self.expect_equal(f"{entry['id']} has a workspace id", True, entry.get("workspace") is not None)
            agents = sorted(entry["agent"] for entry in listed)
            self.expect_equal("agents", ["claude", "codex"], agents)
            states = {entry["state"] for entry in listed}
            self.expect_equal("fresh sessions are idle", {"idle"}, states)

    # AC3

    def test_ac3_send_shows_the_message_and_starts_the_agent(self) -> None:
        with self.step("create the fixture repo and a claude script that replies once"):
            script = self.write_script(
                "claude.json",
                {"session_ids": ["claude-session-1"], "turns": [{"steps": [{"chunk": "On it."}, {"end": "end_turn"}]}]},
            )
            self.start_with_agents(script, None)

        with self.step("start a session and send a message"):
            session_id = self.new_session("claude")
            result = self.codctl.run("session", "send", session_id, "Fix the rounding in src/invoice.rs")
            self.expect_result(result, 0, stdout="", stderr="")

        with self.step("the message shows as the user's and the agent starts working"):
            self.wait_for(
                lambda state: "Fix the rounding in src/invoice.rs" in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "the sent message in the transcript",
            )
            self.wait_for(
                lambda state: "On it." in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "the scripted reply in the transcript",
            )

    def test_c2_send_keeps_the_users_unsent_draft(self) -> None:
        """RC review C2: `session send` used to insert its text at the message box's cursor and
        submit everything there, so a draft the user had not sent yet went out (and was cleared)
        along with the command's own text."""
        with self.step("create the fixture repo and a claude script with two plain turns"):
            script = self.write_script(
                "claude.json",
                {
                    "session_ids": ["claude-session-1"],
                    "turns": [
                        {"steps": [{"chunk": "Done."}, {"end": "end_turn"}]},
                        {"steps": [{"chunk": "Noted."}, {"end": "end_turn"}]},
                    ],
                },
            )
            self.start_with_agents(script, None)

        with self.step("start a session, focus its message box, type an unsent draft, then send a different message"):
            session_id = self.new_session("claude")
            # codctl session new starts the chat without giving the window real keyboard focus
            # (no UI click or keystroke caused it), unlike opening a chat by hand; focus it for
            # real before typing, the same as every other test that types into a chat.
            self.run_command("agent: toggle focus")
            self.wait_for(
                lambda state: state.get("agent_panel_focused") is True,
                SHOW_TIMEOUT,
                "the new chat's message box to take the focus",
            )
            self.type("unsent draft about the ledger export")
            result = self.codctl.run("session", "send", session_id, "Fix the rounding in src/invoice.rs")
            self.expect_result(result, 0, stdout="", stderr="")

        with self.step("the sent message and its reply show, without the draft swept into them"):
            state = self.wait_for(
                lambda state: "Done." in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "the scripted reply to the sent message",
            )
            self.expect_equal(
                "the sent message is exactly the requested text",
                True,
                "Fix the rounding in src/invoice.rs" in chat(state).get("transcript", ""),
            )
            self.expect_equal(
                "the unsent draft was not swept into the sent message",
                False,
                "unsent draft about the ledger export" in chat(state).get("transcript", ""),
            )

        with self.step("the draft is still in the message box: Enter sends it on its own"):
            self.keys("enter")
            self.wait_for(
                lambda state: "unsent draft about the ledger export" in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "the preserved draft sent on its own",
            )

    def test_rc_c2_send_queues_behind_a_running_turn_instead_of_interrupting_it(self) -> None:
        """RC review C2 new defect #2: `session send` called `ThreadView::send_content`
        directly, which always starts a turn right away; while a turn was already running, this
        cancelled it on the legacy transport and overtook whatever the user had already queued
        behind it. A turn that blocks on a `wait` step lets the test hold it open and queue a
        plain user message ahead of the codctl message, then check from the fake agent's own
        log that no cancel ever happened and that both messages reached the agent in the order
        they were queued, not the order codctl called in."""
        claude_log = self.workdir / "claude.log"
        with self.step("create the fixture repo and a claude script whose first turn blocks on a control file"):
            script = self.write_script(
                "claude.json",
                {
                    "session_ids": ["claude-session-1"],
                    "turns": [
                        {"steps": [{"chunk": "T1 chunk"}, {"wait": "release-1"}, {"end": "end_turn"}]},
                        {"steps": [{"chunk": "T2 chunk"}, {"end": "end_turn"}]},
                        {"steps": [{"chunk": "T3 chunk"}, {"end": "end_turn"}]},
                    ],
                },
            )
            repo = self.make_repo("billing-api", BILLING_API)
            self.start_cod(
                repo,
                extra_settings={"agent_servers": {"claude": fake_agent_settings(script, log=claude_log)}},
            )
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                SHOW_TIMEOUT,
                "the billing-api folder to open",
            )
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, SHOW_TIMEOUT, "the agent panel to load")

        with self.step("start a session, focus its message box, and send the first message"):
            session_id = self.new_session("claude")
            self.run_command("agent: toggle focus")
            self.wait_for(
                lambda state: state.get("agent_panel_focused") is True,
                SHOW_TIMEOUT,
                "the new chat's message box to take the focus",
            )
            self.type("Begin turn one please")
            self.keys("enter")
            self.wait_for(
                lambda state: "T1 chunk" in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "turn one to start generating",
            )

        with self.step("while turn one is still generating, queue a plain user message through the UI"):
            self.type("Queued user message about tax rates")
            self.keys("enter")
            time.sleep(SETTLE_TIME)

        with self.step("while still generating, send a codctl message: it must queue, not interrupt"):
            result = self.codctl.run("session", "send", session_id, "Codctl message about ledger export")
            self.expect_result(result, 0, stdout="", stderr="")
            time.sleep(SETTLE_TIME)

        with self.step("release turn one and let the queue drain"):
            (self.workdir / "release-1").write_text("go\n")
            self.wait_for(
                lambda state: "T3 chunk" in chat(state).get("transcript", ""),
                SHOW_TIMEOUT,
                "every queued turn to finish",
            )

        with self.step("the fake agent's own log shows no cancel, and the FIFO order of the two queued messages"):
            prompts, cancels = prompt_texts_and_cancels(claude_log)
            self.expect_equal("no session/cancel while turn one was generating", 0, cancels)
            self.expect_equal("number of prompts the fake agent received", 3, len(prompts))
            if len(prompts) == 3:
                self.expect_equal("prompt 1 is the first, idle send", "Begin turn one please", prompts[0])
                self.expect_equal(
                    "prompt 2 is the queued user message, not the codctl message that queued behind it",
                    "Queued user message about tax rates",
                    prompts[1],
                )
                self.expect_equal(
                    "prompt 3 is the codctl message, sent only once the queue ahead of it drained",
                    "Codctl message about ledger export",
                    prompts[2],
                )

    # AC4

    def test_ac4_wait_until_idle_and_needs_input(self) -> None:
        with self.step("create the fixture repo and a claude script with one plain turn and one permission turn"):
            script = self.write_script(
                "claude.json",
                {
                    "session_ids": ["claude-session-1", "claude-session-2"],
                    "turns": [
                        {"steps": [{"chunk": "All done."}, {"end": "end_turn"}]},
                        {"steps": [{"permission": "Allow edit to src/invoice.rs?"}]},
                    ],
                },
            )
            self.start_with_agents(script, None)

        with self.step("wait --until idle prints the agent's last reply once the turn ends"):
            session_id = self.new_session("claude")
            self.codctl.run("session", "send", session_id, "Fix it")
            result = self.codctl.run("session", "wait", session_id, "--until", "idle", "--timeout", "20")
            self.expect_result(result, 0, stderr="")
            self.expect_equal("the last reply is printed", True, "All done." in result.stdout)

        with self.step("a second session: wait --until needs-input prints the pending prompt"):
            session_id_2 = self.new_session("claude")
            self.codctl.run("session", "send", session_id_2, "Fix it too")
            result = self.codctl.run("session", "wait", session_id_2, "--until", "needs-input", "--timeout", "20")
            self.expect_result(result, 0, stderr="")
            self.expect_equal("the pending prompt is printed", True, "Allow edit to src/invoice.rs?" in result.stdout)

    def test_ac4_wait_times_out(self) -> None:
        with self.step("create the fixture repo and a claude script whose turn sleeps longer than the timeout"):
            script = self.write_script(
                "claude.json",
                {"session_ids": ["claude-session-1"], "turns": [{"steps": [{"sleep": 2}, {"chunk": "late"}, {"end": "end_turn"}]}]},
            )
            self.start_with_agents(script, None)

        with self.step("wait with a short timeout exits 3 before the turn ends"):
            session_id = self.new_session("claude")
            self.codctl.run("session", "send", session_id, "Fix it")
            result = self.codctl.run("session", "wait", session_id, "--until", "idle", "--timeout", "0.3")
            self.expect_equal("exit code", 3, result.returncode)
            self.expect_equal("stdout", "", result.stdout)

    def test_ac4_wait_and_status_reload_a_session_the_idle_eviction_cache_dropped(self) -> None:
        """RC review C8 continuation: the agent panel keeps at most `max_idle_retained_threads`
        idle chats resident (default 5) and drops the rest from memory, though every chat it ever
        started stays a member of the workspace (`codctl session list` still shows it). `wait` and
        `status` used to read such a dropped session straight from that eviction cache, so they
        reported it `idle` with an empty transcript even though its turn had actually finished
        with a real reply. Setting `max_idle_retained_threads` to 0 drops a session the moment a
        second one opens, the fastest way to force this without juggling five others."""
        with self.step("create the fixture repo, a claude script with two plain turns, and no idle retention"):
            script = self.write_script(
                "claude.json",
                {
                    "session_ids": ["claude-session-1", "claude-session-2"],
                    "turns": [
                        {"steps": [{"chunk": "Reply one"}, {"end": "end_turn"}]},
                        {"steps": [{"chunk": "Reply two"}, {"end": "end_turn"}]},
                    ],
                },
            )
            repo = self.make_repo("billing-api", BILLING_API)
            self.start_cod(
                repo,
                extra_settings={
                    "agent_servers": {"claude": fake_agent_settings(script, log=self.workdir / "claude.log")},
                    "agent": {"max_idle_retained_threads": 0},
                },
            )
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                SHOW_TIMEOUT,
                "the billing-api folder to open",
            )
            self.wait_for(lambda state: state.get("agent_panel_ready") is True, SHOW_TIMEOUT, "the agent panel to load")

        with self.step("finish a first session's turn, then drop it from memory by opening a second"):
            first_id = self.new_session("claude")
            self.codctl.run("session", "send", first_id, "Fix the rounding")
            result = self.codctl.run("session", "wait", first_id, "--until", "idle", "--timeout", "20")
            self.expect_result(result, 0, stderr="")
            self.expect_equal("the first reply, while still resident", True, "Reply one" in result.stdout)
            # A second session evicts the first from `retained_threads` at once
            # (`max_idle_retained_threads: 0`), though the workspace still owns it.
            self.new_session("claude")

        with self.step("wait --until idle on the evicted session still prints its real last reply"):
            result = self.codctl.run("session", "wait", first_id, "--until", "idle", "--timeout", "20", "--json")
            self.expect_result(result, 0, stderr="")
            payload = json.loads(result.stdout)
            self.expect_equal("state", "done", payload.get("state"))
            self.expect_equal("text", True, "Reply one" in (payload.get("text") or ""))

        with self.step("status without --line on the evicted session reports done, not a placeholder idle"):
            result = self.codctl.run("session", "status", first_id)
            self.expect_result(result, 0, stderr="")
            state_word, line = result.stdout.rstrip("\n").split("\t", 1)
            self.expect_equal("state", "done", state_word)
            self.expect_equal("line", "done", line)

    # AC5

    def test_ac5_status_line(self) -> None:
        with self.step("create the fixture repo and name the workspace with a ticket prefix"):
            script = self.write_script("claude.json", {"session_ids": ["claude-session-1"], "turns": []})
            self.start_with_agents(script, None)
            listed = json.loads(self.codctl.run("workspace", "list", "--json").stdout)
            workspace_id = str(listed[0]["id"])
            self.codctl.run("workspace", "rename", workspace_id, "[PAY-1234] (billing-api) Fix invoice rounding")

        with self.step("start a session and set its status line"):
            session_id = self.new_session("claude")
            result = self.codctl.run("session", "status", session_id, "--line", "reading 0042_tax.sql")
            self.expect_result(result, 0, stdout="", stderr="")

        with self.step("the overview's second line shows it at once"):
            self.keys("ctrl-alt-o")
            state = self.wait_for(
                lambda state: isinstance(state.get("overview"), dict) and len(state["overview"]["rows"]) > 0,
                SHOW_TIMEOUT,
                "the overview to open with its rows",
            )
            session_row = next((row for row in state["overview"]["rows"] if row["kind"] == "session"), None)
            assert session_row is not None, f"expected a session row, actual {state['overview']['rows']!r}"
            self.expect_equal("second line", "[PAY-1234] - reading 0042_tax.sql", session_row["detail"])
            self.keys("escape")

        with self.step("codctl session status without --line reports the state and the line"):
            result = self.codctl.run("session", "status", session_id)
            self.expect_result(result, 0, stderr="")
            state_word, line = result.stdout.rstrip("\n").split("\t", 1)
            self.expect_equal("state", "idle", state_word)
            self.expect_equal("line", "reading 0042_tax.sql", line)

        with self.step("an empty --line clears it"):
            result = self.codctl.run("session", "status", session_id, "--line", "")
            self.expect_result(result, 0, stdout="", stderr="")
            self.keys("ctrl-alt-o")
            state = self.wait_for(lambda state: isinstance(state.get("overview"), dict), SHOW_TIMEOUT, "the overview to reopen")
            session_row = next((row for row in state["overview"]["rows"] if row["kind"] == "session"), None)
            assert session_row is not None
            self.expect_equal("second line after clearing", "[PAY-1234]", session_row["detail"])
            self.keys("escape")

    # AC6

    def test_ac6_unknown_session_id_errors(self) -> None:
        with self.step("create the fixture repo, no sessions started"):
            script = self.write_script("claude.json", {"session_ids": [], "turns": []})
            self.start_with_agents(script, None)

        with self.step("every session command prints a one-line error and exits 1 for an unknown id"):
            for args in (
                ["session", "send", "no-such-session", "hi"],
                ["session", "wait", "no-such-session", "--until", "idle", "--timeout", "1"],
                ["session", "status", "no-such-session"],
                ["session", "status", "no-such-session", "--line", "x"],
            ):
                result = self.codctl.run(*args)
                self.expect_equal(f"exit code of {args}", 1, result.returncode)
                self.expect_equal(f"stdout of {args}", "", result.stdout)
                lines = result.stderr.splitlines()
                self.expect_equal(f"one stderr line for {args} ({result.stderr!r})", 1, len(lines))


if __name__ == "__main__":
    unittest.main()
