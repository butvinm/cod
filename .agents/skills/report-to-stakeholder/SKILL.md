---
name: report-to-stakeholder
description: PM procedure to report a finished Cod story to the stakeholder as a short PASS/FAIL table with links to the issue, the PR and the step-by-step check list, plus open questions, in plain language without jargon. Use when a story has a QA verdict and a check list, and for the demo at each stakeholder meeting.
---

# Report to the stakeholder

The stakeholder judges features by following the check list in the app and from daily use, not from logs, and gives feedback on features and UX instead of accepting issues. Keep it short enough to read in one minute.

## Steps

1. Gather: the issue (`gh issue view <issue>`), the PR and CI status (`gh pr view <pr>`, `gh pr checks <pr>`), the QA hand-back, the reviewer verdict and the link to the check list comment.
2. Spot-check one criterion yourself: open its test and its check list block, and confirm they match the wording of the criterion and that the steps are clear enough to follow without help. If not, send the story back to QA instead of reporting.
3. Rewrite each criterion as a plain sentence about what the user can now do. No file names, test names, stack traces or internal terms in the table.
4. Mark each PASS or FAIL. A FAIL gets one plain sentence on what is missing and what happens next.
5. Collect open questions that need the stakeholder's decision. Each one offers a default ("If I don't hear back, I will ..."), so silence never blocks the team.
6. Scrub: no work-related names, URLs or screenshots in the message.
7. Post it as a comment on the issue (`gh issue comment <issue> --body-file <file>`) and send the same text to the stakeholder.

## Output format

```
<Story title> (#<issue>) - ready, closes unless you object | not ready

| What you can now do | Result |
| --- | --- |
| Switch to the workspace that needs you with one shortcut | PASS |
| See the needs-input sessions at the top of the list | FAIL - sorting is wrong when two sessions finish at once; fix in progress |

How to check it (about 3 min): <link to the check list comment>
Issue: <link>  PR: <link>

Questions:
1. <question>? If I don't hear back, I will <default>.
```
