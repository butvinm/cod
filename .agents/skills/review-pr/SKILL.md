---
name: review-pr
description: Reviewer procedure to check one Cod pull request diff against its story's acceptance criteria, the product rules (minimal UI, keyboard everywhere, looks like Zed) and code quality, and output APPROVE or REQUEST CHANGES with file:line evidence. Use for every implementation or tests PR.
---

# Review a pull request

Output: the hand-back block from `.claude/agents/reviewer.md`. You do not change any file.

## Steps

1. Load the inputs: `gh pr view <pr>`, `gh pr diff <pr>`, `gh issue view <issue>` (or the saved files the PM points at). List AC1...ACn.
2. Story check, one row per criterion:
   - Is there an end-to-end test that checks exactly this criterion? Cite `file:line`.
   - Does the code implement it, not just satisfy the test by accident? Cite `file:line`.
3. Test integrity: compare the acceptance test files with the tests PR. Any removed, skipped or weakened assertion, raised timeout or catch-all selector is blocking.
4. Scope: the PR covers one story only, touches only files it needs, and adds nothing the story lists as out of scope.
5. Product rules:
   - Minimal UI: no new settings, toggles or panels the story did not ask for.
   - Keyboard everywhere: new controls are focusable, have shortcuts or palette commands where the story implies them, and show focus.
   - Looks like Zed: quiet colors, dense layout, no decorative chrome. If there is a screenshot, look at it.
6. Code quality:
   - the stack in `AGENTS.md` (TypeScript in strict mode for app code and tests); any deviation is blocking unless the PM approved it in the issue;
   - clear module boundaries (UI, main-process services, agent providers); no app APIs exposed to external web pages;
   - no new dependency without a reason in the PR body;
   - errors handled at boundaries, no swallowed exceptions;
   - names and structure an agent can change easily; no dead code.
7. Public repo and text rules: grep the diff for work URLs, internal names, tokens, and for banned characters (em dash, en dash, ellipsis, curly quotes, unicode arrows).
8. Commits: atomic, clear subjects, no AI attribution trailers.
9. Decide. REQUEST CHANGES if any blocking finding exists; otherwise APPROVE. Non-blocking suggestions never block.

## Output format

```
PR: #<pr> <title>
Story: #<issue>
Verdict: APPROVE | REQUEST CHANGES
| AC | Covered by test | Implemented | Evidence |
Findings (blocking first): numbered, each "[blocking|non-blocking] file:line - problem - what to change"
Checklist: story scope | stack | tests untouched | keyboard | minimal UI | Zed look | public-repo scrub | text rules | commits (each OK or FAIL)
```
