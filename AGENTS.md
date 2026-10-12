# Cod: rules for every agent

Cod is a desktop workspace for agent-driven development (Linux first, macOS second). It is a fork of the Zed editor at v1.23.2 (Rust, GPUI), frozen at that version: no merges from upstream Zed. The earlier Electron prototype lives at butvinm/cod-electron. It is a personal tool built in the open by one human stakeholder and a team of agents. Read this file before any task. It overrides your defaults.

## Roles

| Role         | Who                                                                   | Owns                                                         | Never                                                       |
| ------------ | --------------------------------------------------------------------- | ------------------------------------------------------------ | ----------------------------------------------------------- |
| Stakeholder  | the human                                                             | priorities, feature and UX feedback, daily use               | does QA or debugging for the team                           |
| PM / Analyst | main Claude Code session                                              | backlog, stories, the queue, reports to the stakeholder      | writes app code or tests; it delegates and only spot-checks |
| QA           | Codex (`.claude/agents/qa.md`); check-list mode: Claude `qa` subagent | acceptance tests, test runs, check lists, PASS/FAIL verdicts | edits app code                                              |
| Developer    | Claude subagent (`.claude/agents/developer.md`)                       | implementation of one story per pull request                 | weakens, skips or deletes acceptance tests                  |
| Reviewer     | separate Codex run (`.claude/agents/reviewer.md`)                     | review of each PR diff against its story                     | edits the branch under review                               |

GitHub Issues and the project board are the shared state. No agent relies on another agent's memory or chat history: everything a task needs is in the issue, the PR or the files named in the handoff.

## Workflow

1. Story issue. The PM writes it with the `write-story` skill: user story, numbered acceptance criteria (AC1, AC2, ...), requirement IDs, out of scope, size. It goes into the ranked queue, and the stakeholder picks and reorders at meetings (`plan-iteration`).
2. Failing acceptance tests. QA writes end-to-end tests from the criteria (`acceptance-tests`), opens a PR with only tests, and shows they fail for the right reason.
3. Implementation PR. A developer makes those tests pass test-first (`implement-story`). One story per PR, using the PR template.
4. Review. A separate Codex run reviews the diff against the story (`review-pr`): approve or request changes with file:line evidence.
5. CI. All suites green on Linux (macOS when relevant).
6. Check list. QA writes a short list of numbered steps the stakeholder can follow in the running app to check each criterion (`check-list`), follows the steps in the app itself, and posts the list with PASS/FAIL per criterion on the issue.
7. Beta pass. Before each meeting, the PM runs a beta tester pass with the `user-story` skill: invented personas explore the app around the story and narrate the friction they meet. Findings become follow-up issues.
8. Feedback and close. The PM reports (`report-to-stakeholder`) and demos the features at the meeting; the stakeholder gives feedback on features, bugs and UX, not on issues. The PM closes a story when QA's check list passed, the beta tester ran it, and the stakeholder said nothing against it. Feedback against it goes back into the story as a new criterion or comment.

Bugs and "Report problem" folders follow `report-problem`: reproduce as a failing test first, then fix through the same flow.

## Working with the stakeholder

- There are no fixed-length sprints. Closed stories merged to `master` are the release; from 0.0.1 on, each meeting that closes stories ends with a tag.
- A PM session is a sprint. It starts from the files (this file, the last minutes in `cod-docs/meetings/`, the pinned `Queue` issue) and ends with a meeting in the same session: obligations checked, demo and feedback on features and UX, escalations, minutes, and the next sprint's obligations. The next sprint starts in a new session. The agenda is in `plan-iteration`.
- Every feature that Zed has starts with investigating Zed: its source at the pinned version and stock Zed itself. Look changes come with a screenshot next to stock Zed.
- Between meetings the team works on its own from the ranked queue (a pinned `Queue` issue). The PM decides technical questions and product details that fit `cod-docs/vision.md`.
- Only these go to the stakeholder, as an issue labeled `stakeholder` with a default answer: a change to the vision or scope, spending money, anything public about the stakeholder or their employer, and anything that cannot be undone. Work continues on everything not blocked.
- Obligations: whatever the PM says will be ready by the next meeting is either delivered or comes with a written reason and the next step. Each obligation is a list of numbered checks the stakeholder can verify in a few minutes, plus a "not included" list. The stakeholder checks them at the meeting and gives feedback.
- Obligations are a minimum. When something gets unblocked early, keep going and present the result at the next meeting, not the news.
- Minutes of each meeting live in `cod-docs/meetings/<date>.md` (local, never published; Zed's own `docs/` folder is public): previous obligations met or missed with evidence, numbered decisions, stories closed and follow-ups filed, new obligations, open questions with defaults.

## Handoffs and reports

- Every handoff carries numbered acceptance criteria, the issue number, the files the receiver owns, and the expected hand-back format.
- Every report is PASS or FAIL per criterion, each with evidence: a test name and its output, a file:line, a commit or a screenshot path. "It works" without evidence is FAIL.
- Keep each agent task small: under about 20 minutes, with a checkpoint (commit or comment) at each step. Split bigger work into more tasks.
- One owner per file at a time. The handoff lists the files the receiver may change; touching others needs a new handoff.

## Definition of done

A story is done when all of these hold:

- every acceptance criterion has a passing automated end-to-end test that drives the real app;
- the tests were written first and failed before the implementation;
- the reviewer approved the PR and CI is green;
- the check list is on the issue and QA followed it in the app;
- the PASS/FAIL report is on the issue and the PM closed it by the rule in Workflow step 8.

## Testing rules

- Playwright for Electron drives the real app. A feature is done when its scenarios pass, not when an agent says it works.
- Fakes for slow, costly or random parts: a scripted fake agent provider behind the same interface as Claude and Codex, and local look-alike pages for the tracker, wiki and GitLab. Git and language servers run for real on throwaway fixture repos.
- A small live suite (real agents with cheap models, public gitlab.com) runs on `master` only, on a schedule or by hand, never on PRs from forks.
- Check the look early: before building a whole feature, get one screen in front of the PM (a screenshot) and compare it to Zed.
- Codex's sandbox cannot launch Chromium or Electron. Run browser and Electron tests outside the sandbox (see `.claude/agents/qa.md`).
- On macOS, wrap long jobs in `caffeinate -dimsu <command>`. Before debugging a "hang", check `pmset -g log` for sleep events.

## Skills

Procedures live as skills in `.agents/skills/<name>/SKILL.md` (the path Codex scans). `.claude/skills` is a symlink to the same folder, so Claude Code reads the same files. If a tool does not load skills on its own, read the file by path.

| Skill                                                    | Used by   |
| -------------------------------------------------------- | --------- |
| `write-story`, `plan-iteration`, `report-to-stakeholder` | PM        |
| `acceptance-tests`, `check-list`                         | QA        |
| `implement-story`                                        | Developer |
| `review-pr`                                              | Reviewer  |
| `report-problem`                                         | anyone    |

Role definitions live in `.claude/agents/`. The PM starts the developer as a Claude subagent and QA and the reviewer as Codex runs, except QA's check-list mode, which runs as the Claude `qa` subagent because it needs a real Electron window; each role file explains how.

## Product rules (from the vision)

- Minimal UI, very few settings. Behavior changes by editing code.
- Everything works from the keyboard, with a command palette.
- It should look and feel like Zed: fast, quiet, few panels. The reference is stock Zed 1.23.2 with default settings and the default keymap. Where Zed has a feature, Cod behaves the same; a difference is a bug unless the story lists it as an exception.
- Cod is a plain good IDE plus its own small set: agent chats, review, `codctl` and workspaces. Nothing from one person's setup (keymap, settings, workflow) is built in.
- Small codebase, clear modules, few dependencies, tests for the core.

## Stack

- Rust in the Zed codebase (GPUI for the UI). Follow `.rules` for code conventions.
- The end-to-end test approach for the fork is still being chosen. Until then, the Playwright and Electron rules in this file apply to butvinm/cod-electron only.
- A change of stack is a PM decision recorded in an issue, never a developer default.

## Text rules (all files, commits, issues, PRs)

- No em dashes, en dashes, ellipsis characters, curly quotes, unicode arrows or non-breaking spaces. Use " - ", "-", "...", straight quotes and "->". Exception: upstream files pinned byte for byte as test fixtures (for example Zed's keymaps and settings under `tests/fixtures/`) stay verbatim.
- Never break prose lines at a width limit. Break only at the end of a sentence, a list item or a paragraph.
- Plain language. Short sentences.

## Commit rules

- Never `git add .` or `git add -A`. Stage specific files.
- Atomic commits: one logical change each, with a clear imperative subject (`Add workspace tab status dot`) and a body when the why is not obvious.
- No AI attribution trailers (no `Co-Authored-By` for agents, no "Generated with" lines).
- Branches: `story/<issue>-<slug>`, `tests/<issue>-<slug>`, `fix/<issue>-<slug>`. Never push to `master` directly; merge via PR.
- PRs are squash-merged (the repo allows nothing else): one commit per PR on `master`, with the PR title as its subject and the PR body as its message, so write both as a good commit. Small commits on the branch are still welcome for review. The branch is deleted after the merge.
- A story PR built on its tests branch carries the tests into `master`; close the tests PR with a comment "Merged as part of #<story PR>" instead of merging it.

## Public repo rules

This repo is public. Nothing from the stakeholder's employer goes in: no work URLs, internal hostnames, internal product or team names, screenshots or recordings of internal pages, tokens or credentials. This applies to code, fixtures, test pages, recordings, reports, issues and PR text.

- Fixtures use invented data and the local look-alike pages, never captured real pages.
- Before attaching a screenshot or log, look at it and scrub it. When in doubt, leave it out and describe it in words.
- Secrets come from environment variables and never get printed in logs or reports.
