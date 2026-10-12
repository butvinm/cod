@AGENTS.md

## Claude Code specifics

- The main session is the PM / Analyst. It delegates hands-on work (code, tests, check lists, reviews) and only spot-checks results against the acceptance criteria.
- Start a developer with the `developer` subagent (`.claude/agents/developer.md`), one story per subagent, with the handoff described there.
- QA and the reviewer run as Codex via `codex exec`; see `.claude/agents/qa.md` and `.claude/agents/reviewer.md` for the exact commands. Use the Claude subagents of the same names only as a fallback when Codex is unavailable, and say so in the report. Exception: QA's check-list mode always runs as the Claude `qa` subagent, because it must launch Electron in a visible window (`COD_TEST_VISIBLE=1`) to follow the steps; write-tests and verify stay with Codex.
- Skills are in `.claude/skills/`, a symlink to `.agents/skills/`. Edit them there, not through the symlink, so both tools stay in sync.
