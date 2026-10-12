---
name: write-story
description: PM procedure to turn a requirement ID or a stakeholder request into a Cod story issue on GitHub with a user story, numbered acceptance criteria, requirement IDs, out-of-scope notes and a size estimate. Use when adding work to the backlog or splitting a story that is too big.
---

# Write a story

Output: one GitHub issue per story, created with `gh issue create`, following `.github/ISSUE_TEMPLATE/story.md`.

## Steps

1. Collect the source. Note the requirement IDs (for example WS3, C9) or quote the stakeholder's request in their own words. If the request is vague, write down your assumption and list it as an open question instead of blocking.
2. Write the user story in one sentence: "As the Cod user, I want <capability>, so that <benefit>."
3. Write numbered acceptance criteria, AC1, AC2, ... Each one:
   - is observable in the running app (what the user does, what the user sees);
   - can become one end-to-end test with a clear pass or fail;
   - names keyboard paths where relevant (every flow must work without a mouse);
   - uses fakes and invented data, never real work pages.
     Use "Given / When / Then" when it makes the criterion clearer. Aim for 3-7 criteria.
4. Write "Out of scope": what a reader might expect but this story does not do, with links to the follow-up issues if they exist.
5. Estimate the size: S (one developer task, under about 20 minutes of agent work), M (two or three tasks), L (split it). Never create an L story: split it into S and M stories by user-visible slice, not by layer.
6. Add a "Look check" line if the story changes what the screen looks like: which screen or element to compare with Zed early, before the whole story is built.
7. Scrub: no employer names, work URLs, internal screenshots or tokens.
8. Create the issue:

   ```
   gh issue create --title "<short user-facing title>" --label story --label size:<S|M> --body-file <file>
   ```

   Write the body to a temporary file first with your editor tool, using the template sections: Story, Acceptance criteria, Requirements, Out of scope, Size, Look check, Open questions.

   If the labels do not exist yet (new repo), create them once: `gh label create story`, `gh label create size:S`, `gh label create size:M`, `gh label create bug`.

9. Add it to the project board (`gh project item-add <project-number> --owner <owner> --url <issue-url>`) in the "Backlog" column.

## Output format

Report back:

```
Created #<n> <title> (size S|M, requirements: <IDs>) <url>
Open questions: <numbered list or "none">
```
