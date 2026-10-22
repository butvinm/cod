"""Exercise missing Mac page paths in the extracted app, using the existing driver.

Driver clicks and keys are in-process native GPUI input, not OS injection. The
workflow separately retains its System Events keyboard proof. Fixtures are invented.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

from cod_e2e import Driver, cod_settings, make_repo, palette_problem
from fake_acp_agent import fake_agent_settings


def prepare(data: Path) -> None:
    config = data / "config"
    config.mkdir(parents=True, exist_ok=True)
    script = data / "mac-agent.json"
    script.write_text(json.dumps({"session_ids": ["mac-link-session"], "turns": [
        {"steps": [{"chunk": "Open https://example.com/ for the reference."}, {"end": "end_turn"}]}
    ]}))
    settings = cod_settings()
    settings["agent_servers"] = {
        "claude-acp": fake_agent_settings(script, log=data / "mac-agent.log")
    }
    (config / "settings.json").write_text(json.dumps(settings))
    make_repo(data, "project", {"README.md": "# Mac candidate fixture\n"})


def exercise(data: Path, socket_path: Path, codctl: str, artifacts: Path) -> None:
    driver = Driver(socket_path)
    last_state: dict = {}

    def wait(predicate, description: str, timeout: float = 30) -> dict:
        nonlocal last_state
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            last_state = driver.state()
            if predicate(last_state):
                return last_state
            time.sleep(0.1)
        raise AssertionError(f"Timed out: {description}")

    def ctl(*args: str) -> str:
        return subprocess.run([codctl, *args], check=True, capture_output=True, text=True,
                              timeout=35).stdout.strip()

    def command(name: str) -> None:
        driver.keys("super-shift-p")
        wait(lambda s: s["modal"] == "command_palette", "command palette")
        driver.type(name)
        wait(lambda s: palette_problem(name, s) is None, name)
        driver.keys("enter")
        wait(lambda s: s["modal"] != "command_palette", "palette closed")

    def page(page_id: str, title: str) -> dict:
        state = wait(lambda s: any(p["id"] == page_id and p["title"] == title
                                   and p["state"] == "loaded" for p in s["pages"]["open"]), title)
        return next(p for p in state["pages"]["open"] if p["id"] == page_id)

    def capture(name: str) -> None:
        subprocess.run(["screencapture", "-x", "-t", "jpg", str(artifacts / f"{name}.jpg")],
                       check=True, timeout=10)
        (artifacts / f"{name}.json").write_text(json.dumps(driver.state(), indent=2))

    try:
        # Follow an actual in-page link, then assert both history destinations.
        (data / "history-next.html").write_text("<title>Mac history destination</title><h1>Destination</h1>")
        (data / "history.html").write_text(
            '<title>Mac history start</title><a href="history-next.html">Continue</a>')
        history_id = ctl("open", (data / "history.html").as_uri())
        current = page(history_id, "Mac history start")
        bounds = current["bounds"]
        assert bounds["width"] > 100 and bounds["height"] > 100, bounds
        driver.request("click", x=bounds["left"] + 30, y=bounds["top"] + 16)
        page(history_id, "Mac history destination")
        ctl("page", "back", history_id)
        page(history_id, "Mac history start")
        ctl("page", "forward", history_id)
        page(history_id, "Mac history destination")
        (data / "history-next.html").write_text("<title>Mac history reloaded</title><h1>Reloaded</h1>")
        ctl("page", "reload", history_id)
        page(history_id, "Mac history reloaded")
        capture("mac-history")
        ctl("page", "close", history_id)
        wait(lambda s: all(p["id"] != history_id for p in s["pages"]["open"]), "history closed")
        print("PASS real in-page navigation, back, forward, reload and close", flush=True)

        # Type and correct text, submit through the existing restricted action bridge.
        form = data / "typed.html"
        form.write_text('<title>Mac typed input</title><input id="answer" autofocus>'
                        '<button data-cod="submit" data-cod-value-from="#answer">Send</button>')
        with subprocess.Popen([codctl, "open", "--html", str(form), "--wait"],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as pending:
            try:
                state = wait(lambda s: any(p["title"] == "Mac typed input" and p["state"] == "loaded"
                                           for p in s["pages"]["open"]), "typed form")
                current = next(p for p in state["pages"]["open"] if p["title"] == "Mac typed input")
                bounds = current["bounds"]
                driver.request("click", x=bounds["left"] + 40, y=bounds["top"] + 16)
                driver.type("mac-edix")
                driver.keys("backspace")
                driver.type("ted")
                capture("mac-input")
                driver.keys("tab enter")
                stdout, stderr = pending.communicate(timeout=20)
                assert pending.returncode == 0 and stdout.strip() == "mac-edited", (stdout, stderr)
            finally:
                if pending.poll() is None:
                    pending.terminate()
                    pending.communicate(timeout=5)
        print("PASS native page text input, backspace and submitted value", flush=True)

        # A file link must go through the editor's ordinary URL action.
        document = data / "reference.txt"
        document.write_text("https://example.com/\n")
        ctl("open", str(document))
        wait(lambda s: s["active_tab"] == "reference.txt", "reference file")
        driver.keys("super-home")
        driver.keys("right right")
        command("editor: open url")
        state = wait(lambda s: any(p["title"] == "Example Domain" and p["state"] == "loaded"
                                   and p["active"] for p in s["pages"]["open"]), "file link page")
        opened = next(p for p in state["pages"]["open"] if p["title"] == "Example Domain" and p["active"])
        capture("mac-file-link")
        ctl("page", "close", opened["id"])
        wait(lambda s: all(p["id"] != opened["id"] for p in s["pages"]["open"]), "file link closed")
        print("PASS file-context URL opens in a page tab", flush=True)

        wait(lambda s: s.get("agent_panel_ready") is True, "agent panel")
        command("agent: toggle focus")
        command("agent: toggle new thread menu")
        driver.keys("end up enter")
        wait(lambda s: (s.get("chat") or {}).get("agent") == "claude-acp", "scripted agent")
        driver.type("Where is the reference?")
        driver.keys("enter")
        wait(lambda s: "https://example.com/" in (s.get("chat") or {}).get("transcript", ""), "agent reply")
        driver.keys("alt-up tab")
        wait(lambda s: ((s.get("chat") or {}).get("focused_link") or {}).get("url")
             == "https://example.com/", "reply link focused")
        driver.keys("enter")
        wait(lambda s: any(p["title"] == "Example Domain" and p["state"] == "loaded" and p["active"]
                           for p in s["pages"]["open"]), "chat link page")
        capture("mac-chat-link")
        print("PASS agent reply keyboard link opens inside Cod", flush=True)
    finally:
        try:
            capture("mac-candidate-final")
        finally:
            driver.close()


if __name__ == "__main__":
    if sys.argv[1] == "prepare":
        prepare(Path(sys.argv[2]).resolve())
    else:
        exercise(Path(sys.argv[2]).resolve(), Path(sys.argv[3]), sys.argv[4], Path(sys.argv[5]))
