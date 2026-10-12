---
name: report-problem
description: Turn a Cod "Report problem" folder (screenshot, app state, recent logs, workspace and session IDs) into a reproduction as a failing test, then a bug or story issue, scrubbing anything work-related before it becomes public. Use by any role when the stakeholder hands over a report path or describes a problem.
---

# Report a problem

Output: a scrubbed bug issue (`.github/ISSUE_TEMPLATE/bug.md`) with a failing test on a `fix/<issue>-<slug>` branch, or a story issue if it is a missing feature rather than a bug.

The report folder is private. It may contain work URLs, internal page screenshots, names and tokens. Nothing from it goes into the repo or an issue until it is scrubbed.

## Steps

1. Read the folder from the path given (under Cod's data directory). Expect: `screenshot.png`, `state.json`, `logs/`, `ids.json` (active workspace and session IDs). Do not copy it into the repo; the repo ignores report folders.
2. Understand the problem: what the user did, what they expected, what happened. Use the logs and state to find the failing step. Ask the stakeholder one question only if the report cannot tell you.
3. Scrub before writing anything public:
   - replace work URLs and hostnames with the look-alike pages (`http://localhost:<port>/tracker/...`);
   - replace internal project, team, people and product names with invented ones;
   - remove tokens, cookies, emails and session contents from logs;
   - never attach the original screenshot if it shows a work page; describe it in words or re-create it with fixtures.
     Then grep your draft for the employer's names and domains and for anything that looks like a secret. If unsure, leave it out.
4. Classify: a bug (Cod behaves wrongly) or a missing feature (run `write-story` instead and stop here).
5. Reproduce as a failing test: create `fix/<issue>-<slug>` (create the issue first if you need the number, step 6), write a Playwright for Electron test with fixtures and fakes that shows the wrong behavior, and confirm it fails for that reason. Commit only the test and fixtures.
6. Create the issue:

   ```
   gh issue create --title "<what goes wrong, in user terms>" --label bug --body-file <file>
   ```

   Sections: What happened, Expected, Steps to reproduce (with fixtures), Failing test (path and failure message), Environment (OS, Cod commit), Acceptance criteria for the fix (AC1: the failing test passes; AC2 ...).

7. Hand the issue to the PM, who schedules the fix through the normal workflow.

## Output format

```
Report: <folder path> (kept local)
Issue: #<n> <title> <url> (bug | story)
Failing test: tests/e2e/<file>.spec.ts "<name>" on fix/<n>-<slug> -> FAIL: <message>
Scrubbed: <what was replaced or removed, in general terms>
```
