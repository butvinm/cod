---
name: implement-story
description: Developer procedure to implement one Cod story test-first until its acceptance tests pass - branch naming, small commits, the PR template, and never touching test assertions. Use when the PM hands over a story with a failing tests PR.
---

# Implement a story

Output: one pull request for one story, all its acceptance tests green, and the hand-back from `.claude/agents/developer.md`.

## Steps

1. Read the handoff and the issue (`gh issue view <issue>`). Write down AC1...ACn and the files you own. If something is missing, stop and ask.
2. Branch from the tests branch (or from `master` once the tests PR is merged):

   ```
   git switch -c story/<issue>-<slug> tests/<issue>-<slug>
   ```

3. Run the acceptance tests and confirm they fail as QA reported. If they pass already, stop and report.
4. Look check first: if the story changes the screen, build the smallest visible slice, take a screenshot and hand it to the PM before building the rest.
5. Work criterion by criterion. For each one:
   - make the smallest change that makes its test pass;
   - add unit tests for any non-trivial logic you write (write them first, watch them fail);
   - run the criterion's test and the unit tests;
   - commit: `git add <specific files>` then `git commit -m "<imperative subject> (#<issue>)"`. Never `git add .`.
6. Checkpoint at least every 20 minutes or after each criterion, whichever is first. If the story is bigger than expected, stop after a checkpoint and report what is left.
7. Never change acceptance test assertions, skip tests, raise timeouts to hide slowness or loosen selectors. If a test seems wrong, stop and report it as an open question for the PM and QA.
8. When all criteria pass, run the full suite: `npm test` and `npx playwright test` (on macOS wrap long runs in `caffeinate -dimsu`).
9. Self-check before the PR:
   - keyboard: every new control is reachable and usable without a mouse;
   - minimal UI: no new settings, no extra panels;
   - text rules and public-repo rules from `AGENTS.md` (grep your diff for banned characters and for work URLs).
10. Push and open the PR with the template:

    ```
    git push -u origin story/<issue>-<slug>
    gh pr create --title "<title> (#<issue>)" --body-file <file>
    ```

    The body follows `.github/pull_request_template.md`: "Closes #<issue>", the AC table with test names, how to try it, and "Acceptance tests modified: none".

## Output format

The hand-back block from `.claude/agents/developer.md`, with one PASS/FAIL row per criterion and the test output as evidence.
