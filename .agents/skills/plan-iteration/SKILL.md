---
name: plan-iteration
description: PM procedure for Cod's sprints and stakeholder meetings. A PM session is a sprint - start it from the last minutes and the queue, run the team on its own, and end it with a meeting (obligations, demo and feedback on features and UX, escalations, minutes, next sprint's obligations). Use at the start of a session, when choosing the next story, when deciding whether something needs the stakeholder, and when the stakeholder asks for the meeting.
---

# Plan and run a sprint

A PM session is a sprint. It has no fixed length. At the start, read `AGENTS.md`, the last minutes in `docs/meetings/` and the pinned `Queue` issue; the obligations in the last minutes are the sprint goal. The team works on its own until the obligations are met, then the PM asks the stakeholder for the meeting. The meeting ends the session; the next sprint starts in a new session.

## The queue

The queue is a pinned issue titled `Queue`. Its body is the ranked list of story issues, most valuable to daily use first:

```
Ranked by value to daily use. The team works top down.
1. #<n> <title> (S) - <one plain sentence on why it matters>
2. ...
Waiting to close: #<n>, ...
```

- Keep at least two ready stories (criteria and size set, see `write-story`) at the top, so the team never waits for a meeting.
- Put a "look check" story early when the look of a new area has not been confirmed.
- Update it with `gh issue edit <queue> --body-file <file>` whenever the order or state changes.

## Between meetings: work alone

1. Take the top unstarted story and drive it through the workflow in `AGENTS.md`: QA tests PR -> developer PR -> reviewer -> CI -> check list. Run independent stories in parallel when they own different files.
2. Keep each agent task under about 20 minutes with a checkpoint. If a task runs long, stop it, read its last checkpoint and split the rest.
3. Keep one owner per file: before a handoff, check no other open task owns the same files.
4. Spot-check, do not redo: open one test and one diff per story and compare them with the criteria.
5. Decide technical questions and product details that fit `docs/vision.md` yourself. Record the decision in the issue or PR.
6. Escalate only these, as an issue labeled `stakeholder` with a default ("If I don't hear back, I will ..."): a change to the vision or scope, spending money, anything public about the stakeholder or their employer, and anything that cannot be undone. Then continue with unblocked work.

## Meeting

The meeting closes the sprint, in the same session. It happens when the obligations are met (or clearly will not be) and the stakeholder is available. Keep it under about 15 minutes. Before it, run the beta tester pass (the `user-story` skill: invented personas explore the app) over each story that reached "check list PASS", and file its findings as follow-up issues. Agenda:

0. Previous obligations: read the last minutes in `docs/meetings/` and mark each obligation met or missed, with evidence. A missed one gets a reason and a new date.
1. Demo: show what the user can now do since the last meeting, using each story's `report-to-stakeholder` report and check list, plus what the beta tester found. Talk about features, bugs, expectations and UX, not issue numbers. Do not ask the stakeholder to accept or reject issues.
2. Close: close each story whose check list passed, that the beta tester ran, and that the stakeholder said nothing against. Feedback against a story goes back into it in the stakeholder's words, as a new criterion or comment; other feedback becomes new stories or bugs.
3. Escalations: go through the open `stakeholder` issues. Record each answer on its issue and close it.
4. Next: show the top of the queue and take the stakeholder's pick, reorder or cuts.
5. Obligations: state what will be ready for the next meeting. Each one is either delivered then or comes with a written reason and the next step. No "almost done". Write each as numbered checks the stakeholder can verify in a few minutes, plus "not included".
6. Minutes: write `docs/meetings/<YYYY-MM-DD>.md` (sections: previous obligations, decisions D1..., stories closed and follow-ups filed, obligations O1..., open questions with defaults) and show it before the meeting ends.

## Releases

Closed stories merged to `master` are the release. From the first version the stakeholder uses daily (0.0.1) onwards, end each day with closed stories with a tag `v0.0.<n>` and a release note listing them (`gh release create`).

## Output format

```
Since last meeting
| What you can now do | Result | Check list | PR |
| --- | --- | --- | --- |
| <feature in plain words> | ready, closes unless you object / not done: <reason> | <link> | <link> |

Beta tester found:
1. <friction in plain words> - follow-up <link>

Questions (#<issue> each):
1. <question>? If I don't hear back, I will <default>.

Next (from the queue):
1. #<n> <title> (S) - <why>
2. ...
Ready by next meeting: <features>. Please pick, reorder or cut.
```
