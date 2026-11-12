# Ticket rules

All work on this repo is tracked in the Jira project **Terminal (KAN)** at https://jarettforzano.atlassian.net. The Bot acting as PM files, dispatches and closes tickets and enforces these rules.

## Hierarchy

- **Epic**: one roadmap theme with a goal and exit criteria (see [ROADMAP.md](ROADMAP.md)).
- **Story**: user-visible behavior. **Task**: engineering work users don't see. **Bug**: something that worked as specified and broke, or a spec violation found in testing.
- **Subtask**: only to split a ticket that one PR can't close.
- Every Story, Task and Bug has a parent Epic.

## Required fields

- **Summary**: an imperative phrase, 3 to 8 words, no prefixes (for example "Support alternate screen and scroll regions").
- **Labels**: exactly one milestone (`v0.1`, `v0.2`, `v0.3`, `v1.0`) and one area (`area-core`, `area-render`, `area-ux`, `area-app`, `area-platform`, `area-infra`, `area-release`).
- **Priority**: Highest (blocks the milestone), High, Medium, Low.
- **Dependencies**: a `Blocks` link from each prerequisite.

## Description template

```
## Goal
Why this matters (one or two sentences).

## Scope
What to build. Name the escape sequences, config keys and platforms involved.

## Acceptance criteria
- Checkable outcomes
- CI green (fmt, clippy, tests) on Linux and macOS
- Verified on macOS and Linux; screenshot or recording on the PR for visible changes

## Dependencies
- Blocked by KAN-n: ...

## Links
- Epic, PRs, related tickets
```

Bugs replace Scope with **Steps to reproduce / Expected / Actual / Platform**.

## Workflow

| Status | Meaning |
|---|---|
| To Do | Ready: template complete and all blockers Done |
| In Progress | Someone (a person or a Devin session) is working on it; the ticket links to them |
| In Review | A PR is open and linked |
| Done | PR merged and acceptance criteria verified |

- One PR per ticket where possible. Put the key in the branch name and PR title (`KAN-12: Support alternate screen`).
- A ticket can't start while a blocker is open.
- Anything found along the way gets its own ticket instead of growing the current one.
