---
name: check-list
description: QA procedure to write a step-by-step check list for a Cod story - numbered steps the stakeholder can follow in the running app in a few minutes to check each acceptance criterion - follow it in the app, and post it on the issue. Use after verify mode passes.
---

# Write a check list

The check list lets the stakeholder try a story in the running app in a few minutes and give feedback on it. It replaces video clips (stakeholder decision 2026-10-12).

Output: one comment on the issue with the check list, and a check list line in the QA hand-back.

## Rules

- One block per acceptance criterion, or per flow when one flow covers several criteria. Name the criteria each block covers.
- Each block is a few numbered steps: what to start, what to press, what you should see. The whole list takes a few minutes.
- Plain language. No test names, file paths or internal terms in the steps.
- Keyboard paths first, with the exact keys (for example `cmd-shift-p` on macOS, `ctrl-shift-p` on Linux). Mention a mouse path only when the criterion is about the mouse.
- Invented data only: a throwaway fixture repo or a folder the steps create, the fake agent provider, the look-alike pages. Nothing from an employer, no real accounts or tokens.
- Write it only after verify mode passes, and follow every step yourself in the app before posting. A step you could not follow is FAIL.

## Quiet mode

`tests/e2e/helpers/electron.ts` sets `COD_TEST_QUIET=1`, so test windows are hidden and the app never activates on macOS. To follow the steps in a real visible window, run with `COD_TEST_VISIBLE=1` (for example `COD_TEST_VISIBLE=1 caffeinate -dimsu npx playwright test ...`), or start the app with `npm start`, which is not affected. Codex's sandbox cannot open an Electron window, so this mode runs as the Claude `qa` subagent, which launches Electron itself with `COD_TEST_VISIBLE=1` and follows the steps (see `.claude/agents/qa.md`). Write-tests and verify stay with Codex. The PM does not do this step.

## Steps

1. Read the criteria (`gh issue view <issue>`) and the QA verify hand-back.
2. Draft the blocks: setup (how to start the app and with what data), then numbered steps with the expected result after each key step.
3. Start the app from the PR branch with a visible window and follow every step. Fix the wording wherever the app differs from what you wrote; if the app is wrong, the story is FAIL.
4. Take a screenshot where it helps as evidence. Look at it and scrub it before attaching.
5. Post the list on the issue (`gh issue comment <issue> --body-file <file>`) and note the comment link.

## Output format

Issue comment:

```
Check list for #<issue> <title>

Setup: <how to start the app and with what invented data>

AC1 - <criterion in plain words>
1. <action, with keys>
2. <action>. You should see <result>.

AC2, AC3 - <flow in plain words>
1. ...

QA followed these steps on <branch or commit>: AC1 PASS, AC2 PASS, AC3 PASS
```

Hand-back line:

```
Check list: <issue comment link>, followed in the app: AC1 PASS, AC2 PASS | <problem>
```
