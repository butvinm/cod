---
name: acceptance-tests
description: QA procedure to write failing end-to-end acceptance tests (Playwright for Electron) from a Cod story's numbered acceptance criteria before development, run the suites, and report PASS/FAIL per criterion with evidence. Use in QA write-tests and verify modes.
---

# Acceptance tests

Output: a tests-only branch `tests/<issue>-<slug>` with one test per acceptance criterion, and a PASS/FAIL report in the format from `.claude/agents/qa.md`.

## Rules

- Drive the real Electron app with Playwright for Electron (`_electron.launch`). No testing of internals through the back door.
- Fakes for agents and websites: the scripted fake agent provider (same interface as Claude and Codex, replaying a script or a recorded session) and the local look-alike pages for the tracker, wiki and GitLab. Never real accounts or work pages.
- Real git and real language servers, on throwaway fixture repos created per test in a temp directory.
- One test per criterion, named after it: `test("AC2: switcher lists needs-input sessions first", ...)`. Group a story's tests in `tests/e2e/<req-id>-<slug>.spec.ts`.
- Select elements by role and accessible name first, then `data-testid`. Never by CSS layout classes.
- Drive keyboard paths with `page.keyboard`, since every flow must work without a mouse. Add a mouse variant only when the criterion asks for it.
- No sleeps. Wait for visible state (`expect(locator).toBeVisible()`, `toHaveText`).
- Tests must be deterministic: fixed fake scripts, fixed clock where time matters, isolated user data dir per test.

## Steps: write-tests mode

1. Read the issue: `gh issue view <issue>`. List the criteria AC1...ACn. If a criterion cannot be tested as written, stop and send it back to the PM with a proposed rewording.
2. Create the branch: `git switch -c tests/<issue>-<slug> master`.
3. Add or extend fixtures: fixture repo builders in `tests/fixtures/`, fake agent scripts in `tests/fakes/agents/`, look-alike pages in `tests/fakes/sites/`. Invented data only. When the story copies Zed behavior, pin the upstream source you rely on (keymap, settings, theme) under `tests/fixtures/` byte for byte, with its URL and commit, instead of guessing.
   Write the developer contract (test hooks, `data-testid` list, chosen keys) in its own file `tests/e2e/contracts/<issue>-<slug>.md`, never in a shared file, so parallel test branches do not conflict.
4. Write one test per criterion. Commit after every one or two tests (`Add failing test for #<issue> AC1`).
5. Run the new tests (outside Codex's sandbox, see `qa.md`; on macOS wrap in `caffeinate -dimsu`):

   ```
   npx playwright test tests/e2e/<file>.spec.ts --reporter=list
   ```

6. Confirm every new test fails because the feature is missing (the assertion message says so), not because of a syntax error, a missing fixture or a crash at launch. Fix your test if it fails for the wrong reason.
7. Open the PR: `gh pr create --title "Tests: #<issue> <title>" --body "Failing acceptance tests for #<issue>. ..."` using the PR template, and list the expected failures.

## Steps: verify mode

1. Check out the implementation branch. Confirm the acceptance test files are unchanged since the tests PR: `git diff <tests-commit> -- tests/e2e/<file>.spec.ts` must be empty (or only additions you approve). Any weakened assertion is an automatic FAIL.
2. Run the story's tests, then the whole e2e suite: `npx playwright test --reporter=list`.
3. For each failure, save the trace and the screenshot path from `test-results/` as evidence.
4. If time allows, try the flow by keyboard once more in a way the tests do not cover, and report anything odd as a non-blocking note.

## Output format

The hand-back table from `.claude/agents/qa.md`: one row per AC with test name, result and evidence, then the suite totals and the verdict. In write-tests mode the expected result per row is "FAIL (expected)" with the failure message.
